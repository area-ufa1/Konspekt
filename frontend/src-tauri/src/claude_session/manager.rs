//! Диалог записи: поднимается с началом записи, гасится с её концом, держит
//! очередь вопросов (один в работе), перезапускает упавший процесс с хвостом
//! в 15 минут и выключается после трёх падений за 5 минут.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::{Mutex, Notify};

use super::context::{next_message, restart_message, Cursor};
use super::dialog::{Dialog, DialogConfig};
use crate::audio::recording_saver::TranscriptSegment;
use crate::meeting_notes::Note;
use crate::privacy::anonymize::Session as PrivacySession;

pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const CRASH_WINDOW: Duration = Duration::from_secs(300);
const CRASH_LIMIT: usize = 3;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AssistantStatus {
    pub available: bool,
    pub reason: Option<String>,
    pub model: Option<String>,
}

/// Как поднимать диалог: либо готовый конфиг, либо причина, почему нельзя.
pub enum StartSetup {
    Ready { config: DialogConfig, shield: Option<StdMutex<PrivacySession>> },
    Unavailable(String),
}

pub struct AskInput<'a> {
    pub question: &'a str,
    pub asked_at: f64,
    pub lines: &'a [TranscriptSegment],
    pub notes: &'a [Note],
}

#[derive(Default)]
pub struct LiveAssistant {
    inner: Mutex<Option<Live>>,
    /// Будит вопрос или разогрев, которые ждут Claude, когда запись остановили.
    stop_signal: Notify,
    /// Растёт на каждом `stop()`. `start()` запоминает его в начале и не ставит
    /// диалог, если за время запуска/разогрева случился `stop()`.
    generation: AtomicU64,
}

struct Live {
    config: DialogConfig,
    dialog: Option<Dialog>,
    cursor: Cursor,
    crashes: Vec<Instant>,
    disabled: Option<String>,
    shield: Option<StdMutex<PrivacySession>>,
}

/// Чем кончился один ход диалога.
enum Asked {
    Answer(String),
    /// Claude жив и ответил ошибкой (лимит, вход): показать текст, без перезапуска.
    TurnFailed(String),
    /// Процесс вышел, оборвал stdout или молчал дольше `idle_timeout`.
    ProcessDied(String),
    Stopped,
}

const STOPPED: &str = "Запись остановлена";
const NOT_RECORDING: &str = "Запись не идёт — ассистент работает во время записи";

impl LiveAssistant {
    pub async fn start(&self, setup: StartSetup) {
        let generation = self.generation.load(Ordering::SeqCst);
        let stopped = self.stop_signal.notified();
        tokio::pin!(stopped);
        stopped.as_mut().enable();
        let live = match setup {
            StartSetup::Unavailable(reason) => Live::disabled(reason),
            StartSetup::Ready { config, shield } => {
                let mut live = Live {
                    config,
                    dialog: None,
                    cursor: Cursor::default(),
                    crashes: Vec::new(),
                    disabled: None,
                    shield,
                };
                match Dialog::spawn(&live.config).await {
                    Ok(mut dialog) => {
                        if self.generation.load(Ordering::SeqCst) != generation {
                            // stop() пришёл, пока процесс запускался.
                            dialog.close().await;
                            return;
                        }
                        let started = Instant::now();
                        let warmed = tokio::select! {
                            result = dialog.ask(super::WARM_UP, |_| {}) => Some(result),
                            _ = &mut stopped => None,
                        };
                        match warmed {
                            None => {
                                // stop() пришёл посреди разогрева.
                                dialog.close().await;
                                return;
                            }
                            Some(Err(error)) => {
                                log::warn!("Разогрев ассистента не удался: {error}");
                                live.crash(error);
                                dialog.close().await;
                            }
                            Some(Ok(_)) => {
                                log::info!("Ассистент записи готов за {} мс", started.elapsed().as_millis());
                                live.dialog = Some(dialog);
                            }
                        }
                    }
                    Err(error) => {
                        log::warn!("Ассистент записи не запустился: {error}");
                        live.disabled = Some(error);
                    }
                }
                live
            }
        };
        let mut guard = self.inner.lock().await;
        if self.generation.load(Ordering::SeqCst) != generation {
            // Запись уже остановлена — диалог не ставим и закрываем.
            drop(guard);
            if let Some(dialog) = live.dialog {
                dialog.close().await;
            }
            return;
        }
        let previous = guard.replace(live);
        drop(guard);
        // Второй старт без стопа: прежний процесс не бросаем.
        if let Some(dialog) = previous.and_then(|previous| previous.dialog) {
            dialog.close().await;
        }
    }

    pub async fn stop(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.stop_signal.notify_waiters();
        let live = self.inner.lock().await.take();
        if let Some(dialog) = live.and_then(|live| live.dialog) {
            dialog.close().await;
        }
    }

    pub async fn status(&self) -> AssistantStatus {
        match self.inner.lock().await.as_ref() {
            None => AssistantStatus { available: false, reason: Some("Запись не идёт".into()), model: None },
            Some(live) => AssistantStatus {
                available: live.disabled.is_none(),
                reason: live.disabled.clone(),
                model: Some(live.config.model.clone()).filter(|model| !model.is_empty()),
            },
        }
    }

    pub async fn ask(&self, input: AskInput<'_>, mut on_text: impl FnMut(&str)) -> Result<String, String> {
        let generation = self.generation.load(Ordering::SeqCst);
        let stopped = self.stop_signal.notified();
        tokio::pin!(stopped);
        stopped.as_mut().enable();
        // Очередь: следующий вопрос ждёт здесь, пока предыдущий не отпустит `inner`.
        let mut guard = self.inner.lock().await;
        let live = guard.as_mut().ok_or(NOT_RECORDING)?;
        if self.generation.load(Ordering::SeqCst) != generation {
            // Вопрос ждал в очереди, а запись тем временем остановили.
            return Err(STOPPED.into());
        }
        if let Some(reason) = &live.disabled {
            return Err(reason.clone());
        }
        // «Только этот компьютер» могли включить посреди записи: проверяем на каждый вопрос.
        if crate::network_policy::local_only() {
            return Err(crate::claude_cli::LOCAL_ONLY_REFUSAL.to_string());
        }
        let started = Instant::now();
        let mut restarted = false;
        for attempt in 0..2 {
            // Диалог вынут из `Live` на время хода: если этот future бросят посреди
            // ответа, процесс уйдёт вместе с ним (drop убивает дерево), а не
            // останется с недочитанным ответом для следующего вопроса.
            let mut dialog = match live.dialog.take() {
                Some(dialog) => dialog,
                None => match Dialog::spawn(&live.config).await {
                    Ok(dialog) => {
                        restarted = true;
                        dialog
                    }
                    Err(error) => {
                        log::warn!("Ассистент: Claude Code не перезапустился: {error}");
                        return Err(live.crash(error));
                    }
                },
            };
            let message = if restarted {
                restart_message(&mut live.cursor, input.lines, input.notes, input.question, input.asked_at)
            } else {
                next_message(&mut live.cursor, input.lines, input.notes, input.question, input.asked_at)
            };
            let message = crate::privacy::hide(live.shield.as_ref(), &message);
            let shield = live.shield.as_ref();
            let mut sofar = String::new();
            let outcome = tokio::select! {
                result = dialog.ask(&message, |delta| {
                    sofar.push_str(delta);
                    on_text(&crate::privacy::restore(shield, &sofar));
                }) => match result {
                    Ok(answer) => Asked::Answer(answer),
                    Err(error) if error.contains("завершился") || error.contains("не ответил") => Asked::ProcessDied(error),
                    Err(error) => Asked::TurnFailed(error),
                },
                _ = &mut stopped => Asked::Stopped,
            };
            match outcome {
                Asked::Answer(answer) => {
                    live.dialog = Some(dialog);
                    let answer = crate::privacy::restore(live.shield.as_ref(), &answer);
                    on_text(&answer);
                    log::info!(
                        "Ассистент: ответ {} симв. за {} мс{}",
                        answer.chars().count(),
                        started.elapsed().as_millis(),
                        if restarted { " (после перезапуска)" } else { "" }
                    );
                    return Ok(answer);
                }
                Asked::TurnFailed(error) => {
                    // Ход закончился ошибкой — процесс не переиспользуем: следующий
                    // вопрос поднимет новый с хвостом. Падением это не считается.
                    log::warn!("Ассистент: Claude Code ответил ошибкой: {error}");
                    dialog.close().await;
                    return Err(error);
                }
                Asked::Stopped => {
                    dialog.close().await;
                    return Err(STOPPED.into());
                }
                Asked::ProcessDied(error) => {
                    log::warn!("Ассистент: Claude Code упал (попытка {}): {error}", attempt + 1);
                    dialog.close().await;
                    let reported = live.crash(error);
                    if live.disabled.is_some() || attempt == 1 {
                        return Err(reported);
                    }
                    restarted = true;
                }
            }
        }
        Err("Claude Code не ответил".into())
    }
}

impl Live {
    fn disabled(reason: String) -> Live {
        Live {
            config: DialogConfig {
                binary: PathBuf::new(),
                model: String::new(),
                system_prompt: String::new(),
                idle_timeout: IDLE_TIMEOUT,
            },
            dialog: None,
            cursor: Cursor::default(),
            crashes: Vec::new(),
            disabled: Some(reason),
            shield: None,
        }
    }

    /// Учитывает падение; после третьего за 5 минут выключает ассистента.
    /// Возвращает текст ошибки для пользователя.
    fn crash(&mut self, error: String) -> String {
        let now = Instant::now();
        self.crashes.retain(|at| now.duration_since(*at) < CRASH_WINDOW);
        self.crashes.push(now);
        if self.crashes.len() >= CRASH_LIMIT {
            let reason = format!(
                "Ассистент выключен до конца записи: Claude Code трижды не ответил за 5 минут. Последняя ошибка: {error}"
            );
            log::warn!("Ассистент выключен до конца записи после {} падений", self.crashes.len());
            self.disabled = Some(reason.clone());
            return reason;
        }
        error
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::claude_session::fake_claude::FakeClaude;
    use std::sync::Arc;

    fn ready(fake: &FakeClaude) -> StartSetup {
        StartSetup::Ready {
            config: DialogConfig {
                binary: fake.binary.clone(),
                model: "haiku".into(),
                system_prompt: "test".into(),
                idle_timeout: Duration::from_secs(5),
            },
            shield: None,
        }
    }

    /// `ask` отказывает в local-only, а `LOCAL_ONLY` стартует с `true`: тесты,
    /// которые задают вопросы живому диалогу, выключают режим под общим замком.
    fn online() -> std::sync::MutexGuard<'static, ()> {
        let guard = crate::network_policy::test_lock();
        crate::network_policy::set_for_test(false);
        guard
    }

    fn input<'a>(question: &'a str) -> AskInput<'a> {
        AskInput { question, asked_at: 10.0, lines: &[], notes: &[] }
    }

    #[tokio::test]
    async fn start_without_cli_disables_with_reason() {
        let assistant = LiveAssistant::default();
        assistant.start(StartSetup::Unavailable("Claude Code не найден".into())).await;
        let status = assistant.status().await;
        assert!(!status.available);
        assert_eq!(status.reason.as_deref(), Some("Claude Code не найден"));
        assert_eq!(assistant.ask(input("x"), |_| {}).await.unwrap_err(), "Claude Code не найден");
    }

    #[tokio::test]
    async fn asking_without_a_recording_is_refused() {
        let assistant = LiveAssistant::default();
        assert!(assistant.ask(input("x"), |_| {}).await.unwrap_err().contains("Запись не идёт"));
    }

    #[tokio::test]
    async fn answers_stream_and_questions_share_one_process() {
        let _policy = online();
        let fake = FakeClaude::new("echo");
        let assistant = LiveAssistant::default();
        assistant.start(ready(&fake)).await;
        let mut seen = Vec::new();
        let first = assistant.ask(input("Первый"), |text| seen.push(text.to_string())).await.unwrap();
        // turn 1 — разогрев, первый настоящий вопрос — turn 2.
        assert!(first.starts_with("turn 2"), "{first}");
        assert_eq!(seen.last().unwrap(), &first, "on_text получает весь текст на данный момент");
        let second = assistant.ask(input("Второй"), |_| {}).await.unwrap();
        assert!(second.starts_with("turn 3"), "{second}");
        assistant.stop().await;
    }

    #[tokio::test]
    async fn a_second_question_waits_for_the_first() {
        // Очередь: пока первый вопрос в работе, второй ждёт `inner` и уходит следующим ходом того же процесса.
        let _policy = online();
        let fake = FakeClaude::new("slow"); // каждый ответ — через 800 мс
        let assistant = Arc::new(LiveAssistant::default());
        assistant.start(ready(&fake)).await; // разогрев — ход 1
        let first = {
            let assistant = assistant.clone();
            tokio::spawn(async move {
                let answer = assistant.ask(input("первый"), |_| {}).await;
                (answer, Instant::now())
            })
        };
        tokio::time::sleep(Duration::from_millis(200)).await;
        let second = {
            let assistant = assistant.clone();
            tokio::spawn(async move {
                let answer = assistant.ask(input("второй"), |_| {}).await;
                (answer, Instant::now())
            })
        };
        let (first, first_done) = first.await.unwrap();
        let (second, second_done) = second.await.unwrap();
        let (first, second) = (first.unwrap(), second.unwrap());
        assert!(first.starts_with("turn 2"), "{first}");
        assert!(second.starts_with("turn 3"), "второй — следующий ход, не вперемешку с первым: {second}");
        assert!(second_done >= first_done, "второй закончился не раньше первого");
        assistant.stop().await;
    }

    #[tokio::test]
    async fn local_only_turned_on_mid_recording_refuses_the_next_question() {
        let _policy = online();
        let fake = FakeClaude::new("echo");
        let assistant = LiveAssistant::default();
        assistant.start(ready(&fake)).await; // разогрев — ход 1
        assert!(assistant.ask(input("до"), |_| {}).await.unwrap().starts_with("turn 2"));
        crate::network_policy::set_for_test(true);
        let refused = assistant.ask(input("после"), |_| {}).await.unwrap_err();
        assert_eq!(refused, crate::claude_cli::LOCAL_ONLY_REFUSAL);
        crate::network_policy::set_for_test(false);
        let next = assistant.ask(input("снова"), |_| {}).await.unwrap();
        assert!(next.starts_with("turn 3"), "отказанный вопрос в Claude не ушёл: {next}");
        assistant.stop().await;
    }

    #[tokio::test]
    async fn a_crash_restarts_with_the_tail() {
        let _policy = online();
        let fake = FakeClaude::new("crash-second");
        let assistant = LiveAssistant::default();
        assistant.start(ready(&fake)).await; // разогрев — ход 1, вопрос — ход 2 → падение
        let answer = assistant.ask(input("после падения"), |_| {}).await.unwrap();
        assert!(answer.starts_with("turn 1"), "новый процесс отвечает с первого хода: {answer}");
        assistant.stop().await;
    }

    #[tokio::test]
    async fn a_failed_turn_is_shown_not_counted_as_a_crash() {
        // Лимит подписки: Claude жив и отвечает ошибкой — её текст уходит в панель,
        // ассистент не выключается, сколько бы таких ответов ни было.
        let _policy = online();
        let fake = FakeClaude::new("limit");
        let assistant = LiveAssistant::default();
        assistant.start(ready(&fake)).await;
        for _ in 0..4 {
            let error = assistant.ask(input("x"), |_| {}).await.unwrap_err();
            assert!(error.contains("usage limit reached"), "{error}");
        }
        let status = assistant.status().await;
        assert!(status.available, "{:?}", status.reason);
        assistant.stop().await;
    }

    #[tokio::test]
    async fn three_crashes_in_five_minutes_switch_it_off() {
        let _policy = online();
        let fake = FakeClaude::new("silent");
        let assistant = LiveAssistant::default();
        assistant.start(ready(&fake)).await;
        for _ in 0..3 {
            let _ = assistant.ask(input("x"), |_| {}).await;
        }
        let status = assistant.status().await;
        assert!(!status.available);
        assert!(status.reason.unwrap().contains("выключен до конца записи"));
        assistant.stop().await;
    }

    #[tokio::test]
    async fn stop_during_answer_ends_the_question() {
        let _policy = online();
        let fake = FakeClaude::new("silent");
        let assistant = Arc::new(LiveAssistant::default());
        assistant.start(ready(&fake)).await;
        let asking = {
            let assistant = assistant.clone();
            tokio::spawn(async move { assistant.ask(input("x"), |_| {}).await })
        };
        tokio::time::sleep(Duration::from_millis(300)).await;
        let started = Instant::now();
        assistant.stop().await;
        let result = tokio::time::timeout(Duration::from_secs(5), asking).await.expect("не повис").unwrap();
        assert!(result.unwrap_err().contains("остановлена"));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn stop_during_warm_up_leaves_nothing_running() {
        // Конец записи, пока диалог ещё разогревается: процесс закрыт, после stop ничего не ставится.
        let fake = FakeClaude::new("slow-warmup"); // на разогрев отвечает через 3 с
        let assistant = Arc::new(LiveAssistant::default());
        let starting = {
            let assistant = assistant.clone();
            let setup = ready(&fake);
            tokio::spawn(async move { assistant.start(setup).await })
        };
        assert!(fake.wait_for(FakeClaude::started, Duration::from_secs(15)).await, "подделка не получила разогрев");
        assistant.stop().await;
        tokio::time::timeout(Duration::from_secs(6), starting).await.expect("start не повис").unwrap();
        let status = assistant.status().await;
        assert_eq!(status.reason.as_deref(), Some("Запись не идёт"), "после stop диалог не поставлен");
        assert!(fake.wait_for(FakeClaude::exited, Duration::from_secs(6)).await, "процесс claude не закрыт");
    }
}
