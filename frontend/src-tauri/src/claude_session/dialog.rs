//! Один процесс `claude -p` в режиме stream-json: вопрос — сообщение в stdin,
//! ответ — дельты и result из stdout.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::mpsc;

use super::protocol::{parse_line, user_message, Event};
use crate::claude_cli::{build_command, is_safe_model_name, working_dir, SystemPromptFile};

#[derive(Debug, Clone)]
pub struct DialogConfig {
    pub binary: PathBuf,
    pub model: String,
    pub system_prompt: String,
    pub idle_timeout: Duration,
}

pub struct Dialog {
    child: Child,
    stdin: ChildStdin,
    lines: mpsc::Receiver<String>,
    idle_timeout: Duration,
    _prompt: SystemPromptFile,
}

pub fn args_for(prompt_file: &Path, model: &str) -> Vec<String> {
    let mut args: Vec<String> = [
        "-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose",
        "--include-partial-messages", "--tools", "", "--permission-mode", "dontAsk",
        "--disable-slash-commands", "--strict-mcp-config", "--setting-sources", "",
        "--no-session-persistence", "--system-prompt-file",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    args.push(prompt_file.to_string_lossy().into_owned());
    if !model.is_empty() && model != "default" {
        args.push("--model".into());
        args.push(model.into());
    }
    args
}

impl Dialog {
    pub async fn spawn(config: &DialogConfig) -> Result<Dialog, String> {
        if !is_safe_model_name(&config.model) {
            return Err(format!("Недопустимое имя модели: {}", config.model));
        }
        let dir = working_dir();
        let prompt = SystemPromptFile::create(&dir, &config.system_prompt)
            .map_err(|e| format!("Не удалось записать системный промпт: {e}"))?;
        let args = args_for(prompt.path(), &config.model);
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let mut command = build_command(&config.binary, &arg_refs, &dir);
        command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
        let mut child = command.spawn().map_err(|e| format!("Claude Code не запустился: {e}"))?;
        let stdin = child.stdin.take().ok_or("нет stdin у Claude Code")?;
        let stdout = child.stdout.take().ok_or("нет stdout у Claude Code")?;
        let (sender, lines) = mpsc::channel(256);
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                if sender.send(line).await.is_err() {
                    break;
                }
            }
        });
        Ok(Dialog { child, stdin, lines, idle_timeout: config.idle_timeout, _prompt: prompt })
    }

    pub async fn ask(&mut self, message: &str, mut on_delta: impl FnMut(&str)) -> Result<String, String> {
        let dead = || "Claude Code завершился посреди диалога".to_string();
        self.stdin.write_all(user_message(message).as_bytes()).await.map_err(|_| dead())?;
        self.stdin.flush().await.map_err(|_| dead())?;
        let mut streamed = String::new();
        loop {
            let next = tokio::time::timeout(self.idle_timeout, self.lines.recv()).await;
            let line = match next {
                Err(_) => return Err(format!("Claude Code не ответил за {} с", self.idle_timeout.as_secs())),
                Ok(None) => return Err(dead()),
                Ok(Some(line)) => line,
            };
            match parse_line(&line) {
                Event::Delta(text) => {
                    streamed.push_str(&text);
                    on_delta(&text);
                }
                Event::Done(text) => return Ok(if text.is_empty() { streamed } else { text }),
                Event::Failed(reason) => return Err(reason),
                Event::Other => {}
            }
        }
    }

    pub async fn close(mut self) {
        let _ = self.stdin.shutdown().await;
        drop(self.stdin);
        if tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await.is_err() {
            let _ = self.child.kill().await;
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::claude_session::fake_claude::FakeClaude;

    fn config(fake: &FakeClaude) -> DialogConfig {
        DialogConfig {
            binary: fake.binary.clone(),
            model: "haiku".into(),
            system_prompt: "Отвечай по-русски".into(),
            idle_timeout: Duration::from_secs(5),
        }
    }

    #[test]
    fn args_are_the_documented_flags() {
        let args = args_for(Path::new("C:/p.txt"), "haiku");
        for flag in ["-p", "--input-format", "stream-json", "--output-format", "--verbose",
                     "--include-partial-messages", "--no-session-persistence", "--model", "haiku"] {
            assert!(args.iter().any(|a| a == flag), "нет {flag}");
        }
    }

    #[tokio::test]
    async fn two_questions_in_one_process_stream_their_answers() {
        let fake = FakeClaude::new("echo");
        let mut dialog = Dialog::spawn(&config(&fake)).await.unwrap();
        let mut deltas = Vec::new();
        let first = dialog.ask("Привет", |d| deltas.push(d.to_string())).await.unwrap();
        assert_eq!(first, "turn 1 len 6");
        assert_eq!(deltas.concat(), "turn 1 len 6");
        let second = dialog.ask("Второй \"вопрос\"\nс переносом", |_| {}).await.unwrap();
        assert!(second.starts_with("turn 2"), "тот же процесс: {second}");
        dialog.close().await;
    }

    #[tokio::test]
    async fn two_dialogs_answer_independently() {
        // Две записи подряд или тест рядом с живым диалогом: процессы не делят ни stdin, ни счёт ходов.
        let (fake_a, fake_b) = (FakeClaude::new("echo"), FakeClaude::new("echo"));
        let mut a = Dialog::spawn(&config(&fake_a)).await.unwrap();
        let mut b = Dialog::spawn(&config(&fake_b)).await.unwrap();
        assert_eq!(a.ask("раз", |_| {}).await.unwrap(), "turn 1 len 3");
        let (from_a, from_b) = tokio::join!(a.ask("два", |_| {}), b.ask("три!", |_| {}));
        assert_eq!(from_a.unwrap(), "turn 2 len 3");
        assert_eq!(from_b.unwrap(), "turn 1 len 4");
        a.close().await;
        b.close().await;
    }

    #[tokio::test]
    async fn a_line_split_across_writes_is_read_whole() {
        // Подделка пишет половину JSON-строки, ждёт 300 мс и дописывает остаток.
        let fake = FakeClaude::new("split");
        let mut dialog = Dialog::spawn(&config(&fake)).await.unwrap();
        let mut deltas = Vec::new();
        let answer = dialog.ask("Привет", |d| deltas.push(d.to_string())).await.unwrap();
        assert_eq!(deltas, ["turn 1", " len 6"], "обрыв между чтениями не теряет и не дробит дельты");
        assert_eq!(answer, "turn 1 len 6");
        dialog.close().await;
    }

    #[tokio::test]
    async fn a_failed_turn_returns_its_reason() {
        let fake = FakeClaude::new("limit");
        let mut dialog = Dialog::spawn(&config(&fake)).await.unwrap();
        let error = dialog.ask("x", |_| {}).await.unwrap_err();
        assert!(error.contains("usage limit reached"), "{error}");
        dialog.close().await;
    }

    #[tokio::test]
    async fn a_dead_process_is_an_error_not_a_hang() {
        let fake = FakeClaude::new("crash-second");
        let mut dialog = Dialog::spawn(&config(&fake)).await.unwrap();
        dialog.ask("one", |_| {}).await.unwrap();
        let error = dialog.ask("two", |_| {}).await.unwrap_err();
        assert!(error.contains("завершился"), "{error}");
    }

    #[tokio::test]
    async fn silence_times_out() {
        let fake = FakeClaude::new("silent");
        let mut config = config(&fake);
        config.idle_timeout = Duration::from_secs(2);
        let mut dialog = Dialog::spawn(&config).await.unwrap();
        let started = std::time::Instant::now();
        let error = dialog.ask("x", |_| {}).await.unwrap_err();
        assert!(error.contains("не ответил"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(5));
        dialog.close().await;
    }

    #[tokio::test]
    async fn close_lets_the_process_exit_on_its_own() {
        // Закрытый stdin — штатный выход подделки, а не убийство через 5 с.
        let fake = FakeClaude::new("echo");
        let mut dialog = Dialog::spawn(&config(&fake)).await.unwrap();
        dialog.ask("x", |_| {}).await.unwrap();
        assert!(fake.started());
        let started = std::time::Instant::now();
        dialog.close().await;
        assert!(started.elapsed() < Duration::from_secs(5), "close не дождался выхода");
        assert!(fake.wait_for(FakeClaude::exited, Duration::from_secs(2)).await);
    }

    #[tokio::test]
    #[ignore = "нужен установленный и залогиненный Claude Code"]
    async fn real_cli_answers_twice_in_one_process() {
        let binary = crate::claude_cli::resolve_binary(None).unwrap();
        let config = DialogConfig { binary, model: "haiku".into(), system_prompt: "Отвечай одним словом.".into(), idle_timeout: Duration::from_secs(60) };
        let mut dialog = Dialog::spawn(&config).await.unwrap();
        let started = std::time::Instant::now();
        let mut first_delta = None;
        let answer = dialog.ask("Скажи: готов", |_| { first_delta.get_or_insert(started.elapsed()); }).await.unwrap();
        assert!(!answer.is_empty());
        let second = std::time::Instant::now();
        dialog.ask("Скажи: да", |_| {}).await.unwrap();
        eprintln!("первая дельта {:?}, второй ответ {:?}", first_delta, second.elapsed());
        dialog.close().await;
    }
}
