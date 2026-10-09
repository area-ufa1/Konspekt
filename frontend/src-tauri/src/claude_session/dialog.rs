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
    /// Всё дерево процесса: при npm-установке прямой потомок — `cmd.exe`,
    /// а сам Claude Code — его `node`. Закрытие (или drop) убивает обоих.
    tree: ProcessTree,
    _prompt: SystemPromptFile,
}

/// Job Object с `KILL_ON_JOB_CLOSE`: закрытие описателя убивает каждый процесс
/// в задании, включая внуков, которых `Child::kill` не достаёт.
#[cfg(windows)]
struct ProcessTree(Option<windows_sys::Win32::Foundation::HANDLE>);

// Описатель задания — просто ядерный объект, им можно владеть из любого потока.
#[cfg(windows)]
unsafe impl Send for ProcessTree {}
#[cfg(windows)]
unsafe impl Sync for ProcessTree {}

#[cfg(windows)]
impl ProcessTree {
    fn adopt(child: &Child) -> ProcessTree {
        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };
        let Some(process) = child.raw_handle() else {
            return ProcessTree(None);
        };
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                log::warn!("Claude Code: Job Object не создан, принудительный стоп дойдёт только до прямого потомка");
                return ProcessTree(None);
            }
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let configured = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) != 0;
            if !configured || AssignProcessToJobObject(job, process as HANDLE) == 0 {
                log::warn!("Claude Code: процесс не помещён в Job Object, принудительный стоп дойдёт только до прямого потомка");
                CloseHandle(job);
                return ProcessTree(None);
            }
            ProcessTree(Some(job))
        }
    }

    /// Убить всё дерево сейчас (закрыть задание).
    fn kill(&mut self) {
        if let Some(job) = self.0.take() {
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(job);
            }
        }
    }
}

#[cfg(windows)]
impl Drop for ProcessTree {
    fn drop(&mut self) {
        self.kill();
    }
}

#[cfg(not(windows))]
struct ProcessTree;

#[cfg(not(windows))]
impl ProcessTree {
    fn adopt(_child: &Child) -> ProcessTree {
        ProcessTree
    }
    fn kill(&mut self) {}
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
        // Сразу после запуска: cmd.exe ещё не успел породить node, и тот
        // унаследует задание.
        let tree = ProcessTree::adopt(&child);
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
        Ok(Dialog { child, stdin, lines, idle_timeout: config.idle_timeout, tree, _prompt: prompt })
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
            // Сначала дерево целиком, пока cmd.exe жив; потом прямой потомок.
            self.tree.kill();
            let _ = self.child.kill().await;
        }
        // Внуки, пережившие штатный выход прямого потомка, гаснут с заданием.
        self.tree.kill();
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
        // Флаги безопасности: без инструментов, без вопросов о разрешениях, свой промпт.
        let pair = |flag: &str, value: &str| {
            args.windows(2).any(|w| w[0] == flag && w[1] == value)
        };
        assert!(pair("--tools", ""), "--tools \"\"");
        assert!(pair("--permission-mode", "dontAsk"), "--permission-mode dontAsk");
        assert!(pair("--system-prompt-file", "C:/p.txt"), "--system-prompt-file <путь>");
        assert!(args.iter().any(|a| a == "--no-session-persistence"));
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
        // Сломанное распознавание EOF должно уронить тест, а не повесить прогон.
        let error = tokio::time::timeout(Duration::from_secs(4), dialog.ask("two", |_| {}))
            .await
            .expect("ask завис на мёртвом процессе")
            .unwrap_err();
        assert!(error.contains("завершился"), "{error}");
    }

    use crate::claude_session::fake_claude::process_alive;

    /// Подделка `hang`: не выходит по закрытому stdin и держит живого внука.
    async fn hung_dialog() -> (FakeClaude, Dialog, [u32; 3]) {
        let fake = FakeClaude::new("hang");
        let mut dialog = Dialog::spawn(&config(&fake)).await.unwrap();
        dialog.ask("x", |_| {}).await.unwrap();
        let shim = dialog.child.id().expect("cmd.exe жив");
        let pids = [shim, fake.pid("fake").unwrap(), fake.pid("grandchild").unwrap()];
        for pid in pids {
            assert!(process_alive(pid), "до закрытия жив {pid}");
        }
        (fake, dialog, pids)
    }

    async fn all_gone(pids: [u32; 3]) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            if pids.iter().all(|&pid| !process_alive(pid)) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        pids.iter().all(|&pid| !process_alive(pid))
    }

    #[tokio::test]
    async fn close_kills_a_hung_process_tree_after_five_seconds() {
        let (_fake, dialog, pids) = hung_dialog().await;
        let started = std::time::Instant::now();
        dialog.close().await;
        let took = started.elapsed();
        assert!(took >= Duration::from_secs(5) && took < Duration::from_secs(8), "close шёл {took:?}");
        assert!(all_gone(pids).await, "после close остались процессы из {pids:?}");
    }

    #[tokio::test]
    async fn dropping_a_dialog_kills_its_process_tree() {
        let (_fake, dialog, pids) = hung_dialog().await;
        drop(dialog);
        assert!(all_gone(pids).await, "после drop остались процессы из {pids:?}");
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
