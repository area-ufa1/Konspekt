//! Диалог записи: поднимается с началом записи, гасится с её концом, держит
//! очередь вопросов (один в работе), перезапускает упавший процесс с хвостом
//! в 15 минут и выключается после трёх падений за 5 минут.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::futures::Notified;
use tokio::sync::{Mutex, Notify};

use super::context::{next_message, restart_message, Cursor};
use super::dialog::{Dialog, DialogConfig, DialogError};
use crate::audio::recording_saver::TranscriptSegment;
use crate::meeting_notes::Note;
use crate::privacy::anonymize::Session as PrivacySession;

pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const CRASH_WINDOW: Duration = Duration::from_secs(300);
const CRASH_LIMIT: usize = 3;

const STOPPED: &str = "Запись остановлена";
const NOT_RECORDING: &str = "Запись не идёт — ассистент работает во время записи";
/// Пустая причина = на старте записи был выбран не Claude Code; провайдер сменили посреди записи.
pub const PROVIDER_CHANGED: &str =
    "Ассистент этой записи не запущен: на её старте был выбран другой провайдер ИИ. Начните запись заново.";

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AssistantStatus {
    pub available: bool,
    pub reason: Option<String>,
    pub model: Option<String>,
}

/// Как поднимать диалог: готовый конфиг, причина, почему нельзя, или «позже».
pub enum StartSetup {
    Ready { config: DialogConfig, shield: Option<StdMutex<PrivacySession>> },
    Unavailable(String),
    /// «Только этот компьютер» включён на старте: ничего не запускаем, а первый
    /// вопрос после выключения режима поднимет диалог этой записи.
    Deferred,
}

/// Метка начала записи. Берётся синхронно, как только пришло событие начала, —
/// до проверки Claude Code: `stop()`, пришедший во время проверки, её устаревает.
#[derive(Debug, Clone, Copy)]
pub struct StartToken(u64);

/// Метка конца записи: `finish_stop` закрывает только диалог, начатый до неё.
#[derive(Debug, Clone, Copy)]
pub struct StopToken(u64);

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
    /// Растёт на каждом `stop()`. Начало записи запоминает его в `StartToken` и
    /// не ставит диалог, если с тех пор случился `stop()`.
    generation: AtomicU64,
    /// Статус без `inner`: окно спрашивает его, пока идёт ответ.
    snapshot: StdMutex<Snapshot>,
}

#[derive(Clone, Default)]
enum Snapshot {
    #[default]
    NotRecording,
    Deferred,
    Running { disabled: Option<String>, model: Option<String> },
}

struct Live {
    /// Поколение, в котором началась эта запись.
    token: u64,
    /// `None` — диалог отложен или недоступен.
    config: Option<DialogConfig>,
    deferred: bool,
    dialog: Option<Dialog>,
    cursor: Cursor,
    crashes: Vec<Instant>,
    disabled: Option<String>,
    shield: Option<StdMutex<PrivacySession>>,
}

/// Чем кончился запуск процесса с разогревом.
enum Launch {
    Ready(Dialog),
    Stopped,
    SpawnFailed(String),
    WarmUpFailed(DialogError),
}

/// Первая строка, не длиннее 120 символов: ошибки Claude Code в лог только так —
/// тело «API Error: …» может процитировать запрос.
fn brief(text: &str) -> String {
    let first = text.lines().next().unwrap_or("");
    let short: String = first.chars().take(120).collect();
    if short.len() < text.len() {
        format!("{short}…")
    } else {
        short
    }
}

/// Закрыть процесс в фоне (stdin, через 5 с — убийство дерева), не задерживая
/// ответ. Если рантайм кончится раньше, drop диалога убьёт дерево сразу.
fn discard(dialog: Dialog) {
    tokio::spawn(dialog.close());
}

fn shown(reason: &str) -> String {
    if reason.is_empty() {
        PROVIDER_CHANGED.to_string()
    } else {
        reason.to_string()
    }
}

impl LiveAssistant {
    /// Начало записи: метка берётся до проверки Claude Code.
    pub fn begin(&self) -> StartToken {
        StartToken(self.generation.load(Ordering::SeqCst))
    }

    /// Конец записи, синхронная часть: всё, что запускается или ждёт Claude,
    /// узнаёт об остановке сразу. Закрывает процесс `finish_stop`.
    pub fn request_stop(&self) -> StopToken {
        let previous = self.generation.fetch_add(1, Ordering::SeqCst);
        self.stop_signal.notify_waiters();
        StopToken(previous)
    }

    /// Конец записи: снимает диалог, начатый до `token`. Диалог следующей записи,
    /// успевшей начаться, не трогает.
    pub async fn finish_stop(&self, token: StopToken) {
        let mut guard = self.inner.lock().await;
        if !guard.as_ref().is_some_and(|live| live.token <= token.0) {
            return;
        }
        let live = guard.take();
        self.publish(None);
        drop(guard);
        if let Some(dialog) = live.and_then(|live| live.dialog) {
            dialog.close().await;
        }
    }

    pub async fn stop(&self) {
        let token = self.request_stop();
        self.finish_stop(token).await;
    }

    pub async fn start(&self, setup: StartSetup, token: StartToken) {
        let stopped = self.stop_signal.notified();
        tokio::pin!(stopped);
        stopped.as_mut().enable();
        if self.generation.load(Ordering::SeqCst) != token.0 {
            // stop() пришёл, пока проверяли Claude Code.
            return;
        }
        let live = match setup {
            StartSetup::Unavailable(reason) => Live::idle(token.0, Some(reason), false),
            StartSetup::Deferred => Live::idle(token.0, None, true),
            StartSetup::Ready { config, shield } => {
                let mut live = Live::idle(token.0, None, false);
                live.config = Some(config);
                live.shield = shield;
                let config = live.config.as_ref().expect("just set");
                let launched = self.launch(config, token.0, stopped.as_mut()).await;
                match launched {
                    Launch::Ready(dialog) => live.dialog = Some(dialog),
                    Launch::Stopped => return,
                    Launch::SpawnFailed(error) => {
                        log::warn!("Ассистент записи не запустился: {error}");
                        live.disabled = Some(error);
                    }
                    Launch::WarmUpFailed(error) => {
                        // Лимит или вход — ошибка хода, не падение: диалог поднимется первым вопросом.
                        if error.is_crash() {
                            live.crash(error.into_message());
                        }
                    }
                }
                live
            }
        };
        let mut guard = self.inner.lock().await;
        if self.generation.load(Ordering::SeqCst) != token.0 {
            // Запись уже остановлена — диалог не ставим и закрываем.
            drop(guard);
            if let Some(dialog) = live.dialog {
                discard(dialog);
            }
            return;
        }
        let previous = guard.replace(live);
        self.publish(guard.as_ref());
        drop(guard);
        // Второй старт без стопа: прежний процесс не бросаем.
        if let Some(dialog) = previous.and_then(|previous| previous.dialog) {
            dialog.close().await;
        }
    }

    /// Не ждёт идущий ответ: читает снимок, а не `inner`.
    pub async fn status(&self) -> AssistantStatus {
        let snapshot = self.snapshot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
        match snapshot {
            Snapshot::NotRecording => {
                AssistantStatus { available: false, reason: Some("Запись не идёт".into()), model: None }
            }
            Snapshot::Deferred if crate::network_policy::local_only() => AssistantStatus {
                available: false,
                reason: Some(crate::claude_cli::LOCAL_ONLY_REFUSAL.into()),
                model: None,
            },
            Snapshot::Deferred => AssistantStatus { available: true, reason: None, model: None },
            Snapshot::Running { disabled, model } => {
                AssistantStatus { available: disabled.is_none(), reason: disabled, model }
            }
        }
    }

    /// Вопрос к диалогу записи. `setup` нужен, только если диалог отложен из-за
    /// «Только этот компьютер», а режим уже выключили: тогда он поднимается сейчас.
    pub async fn ask<F, Fut>(
        &self,
        input: AskInput<'_>,
        setup: F,
        mut on_text: impl FnMut(&str),
    ) -> Result<String, String>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = StartSetup>,
    {
        let generation = self.generation.load(Ordering::SeqCst);
        let stopped = self.stop_signal.notified();
        tokio::pin!(stopped);
        stopped.as_mut().enable();
        // Очередь: следующий вопрос ждёт здесь, пока предыдущий не отпустит `inner`.
        let mut guard = self.inner.lock().await;
        if self.generation.load(Ordering::SeqCst) != generation {
            // Вопрос ждал в очереди, а запись тем временем остановили.
            return Err(STOPPED.into());
        }
        let live = guard.as_mut().ok_or(NOT_RECORDING)?;
        if let Some(reason) = &live.disabled {
            return Err(shown(reason));
        }
        // «Только этот компьютер» могли включить посреди записи: проверяем на каждый вопрос.
        if crate::network_policy::local_only() {
            return Err(crate::claude_cli::LOCAL_ONLY_REFUSAL.to_string());
        }
        if live.deferred {
            // Режим выключили: поднимаем диалог этой записи так же, как на старте.
            let prepared = tokio::select! {
                prepared = setup() => Some(prepared),
                _ = &mut stopped => None,
            };
            match prepared {
                None => return Err(STOPPED.into()),
                Some(StartSetup::Deferred) => return Err(crate::claude_cli::LOCAL_ONLY_REFUSAL.to_string()),
                Some(StartSetup::Unavailable(reason)) => {
                    live.deferred = false;
                    live.disabled = Some(reason.clone());
                    self.publish(Some(&*live));
                    return Err(shown(&reason));
                }
                Some(StartSetup::Ready { config, shield }) => {
                    live.deferred = false;
                    live.config = Some(config);
                    live.shield = shield;
                    let config = live.config.as_ref().expect("just set");
                    let launched = self.launch(config, generation, stopped.as_mut()).await;
                    match launched {
                        Launch::Ready(dialog) => live.dialog = Some(dialog),
                        Launch::Stopped => return Err(STOPPED.into()),
                        Launch::SpawnFailed(error) => {
                            log::warn!("Ассистент записи не запустился: {error}");
                            live.disabled = Some(error.clone());
                            self.publish(Some(&*live));
                            return Err(error);
                        }
                        Launch::WarmUpFailed(error) if error.is_crash() => {
                            let reported = live.crash(error.into_message());
                            self.publish(Some(&*live));
                            return Err(reported);
                        }
                        Launch::WarmUpFailed(error) => {
                            self.publish(Some(&*live));
                            return Err(error.into_message());
                        }
                    }
                    self.publish(Some(&*live));
                }
            }
        }
        let Some(config) = live.config.clone() else {
            return Err(NOT_RECORDING.into());
        };
        let started = Instant::now();
        let mut restarted = false;
        for attempt in 0..2 {
            // Диалог вынут из `Live` на время хода: если этот future бросят посреди
            // ответа, процесс уйдёт вместе с ним (drop убивает дерево), а не
            // останется с недочитанным ответом для следующего вопроса.
            let mut dialog = match live.dialog.take() {
                Some(dialog) => dialog,
                None => match Dialog::spawn(&config).await {
                    Ok(dialog) => {
                        restarted = true;
                        dialog
                    }
                    Err(error) => {
                        log::warn!("Ассистент: Claude Code не перезапустился: {error}");
                        let reported = live.crash(error);
                        self.publish(Some(&*live));
                        return Err(reported);
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
                }) => Some(result),
                _ = &mut stopped => None,
            };
            match outcome {
                Some(Ok(answer)) => {
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
                None => {
                    // Конец записи посреди ответа: отвечаем сразу, процесс гасим в фоне.
                    discard(dialog);
                    return Err(STOPPED.into());
                }
                Some(Err(DialogError::Failed(error))) => {
                    // Claude жив и ответил ошибкой (лимит, вход) — её текст в панель.
                    // Процесс не переиспользуем: следующий вопрос поднимет новый с
                    // хвостом. Падением это не считается.
                    log::warn!("Ассистент: Claude Code ответил ошибкой: {}", brief(&error));
                    discard(dialog);
                    return Err(error);
                }
                Some(Err(crash)) => {
                    log::warn!("Ассистент: Claude Code упал (попытка {}): {crash}", attempt + 1);
                    discard(dialog);
                    let reported = live.crash(crash.into_message());
                    self.publish(Some(&*live));
                    if live.disabled.is_some() || attempt == 1 {
                        return Err(reported);
                    }
                    restarted = true;
                }
            }
        }
        Err("Claude Code не ответил".into())
    }

    /// Запустить процесс и разогреть его; остановка записи прерывает и то и другое.
    async fn launch(&self, config: &DialogConfig, generation: u64, stopped: Pin<&mut Notified<'_>>) -> Launch {
        let mut dialog = match Dialog::spawn(config).await {
            Ok(dialog) => dialog,
            Err(error) => return Launch::SpawnFailed(error),
        };
        if self.generation.load(Ordering::SeqCst) != generation {
            // stop() пришёл, пока процесс запускался.
            discard(dialog);
            return Launch::Stopped;
        }
        let started = Instant::now();
        let warmed = tokio::select! {
            result = dialog.ask(super::WARM_UP, |_| {}) => Some(result),
            _ = stopped => None,
        };
        match warmed {
            None => {
                // stop() пришёл посреди разогрева.
                discard(dialog);
                Launch::Stopped
            }
            Some(Ok(_)) => {
                log::info!("Ассистент записи готов за {} мс", started.elapsed().as_millis());
                Launch::Ready(dialog)
            }
            Some(Err(error)) => {
                log::warn!("Разогрев ассистента не удался: {}", brief(error.message()));
                discard(dialog);
                Launch::WarmUpFailed(error)
            }
        }
    }

    fn publish(&self, live: Option<&Live>) {
        let snapshot = match live {
            None => Snapshot::NotRecording,
            Some(live) if live.deferred => Snapshot::Deferred,
            Some(live) => Snapshot::Running {
                disabled: live.disabled.clone(),
                model: live.config.as_ref().map(|config| config.model.clone()).filter(|model| !model.is_empty()),
            },
        };
        *self.snapshot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = snapshot;
    }
}

impl Live {
    fn idle(token: u64, disabled: Option<String>, deferred: bool) -> Live {
        Live {
            token,
            config: None,
            deferred,
            dialog: None,
            cursor: Cursor::default(),
            crashes: Vec::new(),
            disabled,
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
    use crate::claude_session::fake_claude::{process_alive, FakeClaude};
    use std::sync::Arc;

    fn ready_with(fake: &FakeClaude, idle: Duration) -> StartSetup {
        StartSetup::Ready {
            config: DialogConfig {
                binary: fake.binary.clone(),
                model: "haiku".into(),
                system_prompt: "test".into(),
                idle_timeout: idle,
            },
            shield: None,
        }
    }

    fn ready(fake: &FakeClaude) -> StartSetup {
        ready_with(fake, Duration::from_secs(5))
    }

    /// Запись началась и диалог поднят (или нет) — как это делает слушатель.
    async fn start(assistant: &LiveAssistant, setup: StartSetup) {
        let token = assistant.begin();
        assistant.start(setup, token).await;
    }

    /// Диалог уже поднят: отложенная настройка не должна понадобиться.
    fn no_setup() -> impl FnOnce() -> std::future::Ready<StartSetup> {
        || std::future::ready(StartSetup::Unavailable("настройка не ожидалась".into()))
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

    fn line(sequence_id: u64, at: f64, text: &str) -> TranscriptSegment {
        TranscriptSegment {
            id: format!("s{sequence_id}"),
            text: text.into(),
            audio_start_time: at,
            audio_end_time: at + 2.0,
            duration: 2.0,
            display_time: String::new(),
            confidence: 1.0,
            sequence_id,
            speaker: Some("Speaker 1".into()),
        }
    }

    async fn gone(pid: u32, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            if !process_alive(pid) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        !process_alive(pid)
    }

    #[tokio::test]
    async fn start_without_cli_disables_with_reason() {
        let assistant = LiveAssistant::default();
        start(&assistant, StartSetup::Unavailable("Claude Code не найден".into())).await;
        let status = assistant.status().await;
        assert!(!status.available);
        assert_eq!(status.reason.as_deref(), Some("Claude Code не найден"));
        assert_eq!(assistant.ask(input("x"), no_setup(), |_| {}).await.unwrap_err(), "Claude Code не найден");
    }

    #[tokio::test]
    async fn a_provider_switched_mid_recording_gets_a_reason_not_an_empty_error() {
        let assistant = LiveAssistant::default();
        start(&assistant, StartSetup::Unavailable(String::new())).await;
        let error = assistant.ask(input("x"), no_setup(), |_| {}).await.unwrap_err();
        assert_eq!(error, PROVIDER_CHANGED);
        assert!(error.contains("Начните запись заново"));
    }

    #[tokio::test]
    async fn asking_without_a_recording_is_refused() {
        let assistant = LiveAssistant::default();
        assert!(assistant.ask(input("x"), no_setup(), |_| {}).await.unwrap_err().contains("Запись не идёт"));
    }

    #[tokio::test]
    async fn answers_stream_and_questions_share_one_process() {
        let _policy = online();
        let fake = FakeClaude::new("echo");
        let assistant = LiveAssistant::default();
        start(&assistant, ready(&fake)).await;
        let mut seen = Vec::new();
        let first = assistant.ask(input("Первый"), no_setup(), |text| seen.push(text.to_string())).await.unwrap();
        // turn 1 — разогрев, первый настоящий вопрос — turn 2.
        assert!(first.starts_with("turn 2"), "{first}");
        assert_eq!(seen.last().unwrap(), &first, "on_text получает весь текст на данный момент");
        let second = assistant.ask(input("Второй"), no_setup(), |_| {}).await.unwrap();
        assert!(second.starts_with("turn 3"), "{second}");
        assistant.stop().await;
    }

    #[tokio::test]
    async fn a_second_question_waits_for_the_first() {
        // Очередь: пока первый вопрос в работе, второй ждёт `inner` и уходит следующим ходом того же процесса.
        let _policy = online();
        let fake = FakeClaude::new("slow"); // каждый ответ — через 800 мс
        let assistant = Arc::new(LiveAssistant::default());
        start(&assistant, ready(&fake)).await; // разогрев — ход 1
        let first = {
            let assistant = assistant.clone();
            tokio::spawn(async move {
                let answer = assistant.ask(input("первый"), no_setup(), |_| {}).await;
                (answer, Instant::now())
            })
        };
        tokio::time::sleep(Duration::from_millis(200)).await;
        let second = {
            let assistant = assistant.clone();
            tokio::spawn(async move {
                let answer = assistant.ask(input("второй"), no_setup(), |_| {}).await;
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
    async fn status_answers_while_a_question_streams() {
        let _policy = online();
        let fake = FakeClaude::new("slow");
        let assistant = Arc::new(LiveAssistant::default());
        start(&assistant, ready(&fake)).await;
        let asking = {
            let assistant = assistant.clone();
            tokio::spawn(async move { assistant.ask(input("x"), no_setup(), |_| {}).await })
        };
        tokio::time::sleep(Duration::from_millis(200)).await;
        let status = tokio::time::timeout(Duration::from_millis(100), assistant.status())
            .await
            .expect("status не ждёт идущий ответ");
        assert!(status.available);
        assert_eq!(status.model.as_deref(), Some("haiku"));
        assert!(asking.await.unwrap().unwrap().starts_with("turn 2"));
        assistant.stop().await;
    }

    #[tokio::test]
    async fn local_only_turned_on_mid_recording_refuses_the_next_question() {
        let _policy = online();
        let fake = FakeClaude::new("echo");
        let assistant = LiveAssistant::default();
        start(&assistant, ready(&fake)).await; // разогрев — ход 1
        assert!(assistant.ask(input("до"), no_setup(), |_| {}).await.unwrap().starts_with("turn 2"));
        crate::network_policy::set_for_test(true);
        let refused = assistant.ask(input("после"), no_setup(), |_| {}).await.unwrap_err();
        assert_eq!(refused, crate::claude_cli::LOCAL_ONLY_REFUSAL);
        crate::network_policy::set_for_test(false);
        let next = assistant.ask(input("снова"), no_setup(), |_| {}).await.unwrap();
        assert!(next.starts_with("turn 3"), "отказанный вопрос в Claude не ушёл: {next}");
        assistant.stop().await;
    }

    #[tokio::test]
    async fn local_only_at_start_waits_until_it_is_switched_off() {
        let _policy = crate::network_policy::test_lock();
        crate::network_policy::set_for_test(true);
        let fake = FakeClaude::new("echo");
        let assistant = LiveAssistant::default();
        start(&assistant, StartSetup::Deferred).await;

        // Режим включён: отказ его текстом, Claude Code не трогаем, ассистент не выключен.
        let status = assistant.status().await;
        assert!(!status.available);
        assert_eq!(status.reason.as_deref(), Some(crate::claude_cli::LOCAL_ONLY_REFUSAL));
        let called = std::cell::Cell::new(false);
        let refused = assistant
            .ask(input("пока нельзя"), || { called.set(true); std::future::ready(ready(&fake)) }, |_| {})
            .await
            .unwrap_err();
        assert_eq!(refused, crate::claude_cli::LOCAL_ONLY_REFUSAL);
        assert!(!called.get(), "в local-only настройка не запускается");
        assert!(!fake.started(), "в local-only процесс не поднят");

        // Режим выключили, как просит текст: первый вопрос поднимает диалог этой записи.
        crate::network_policy::set_for_test(false);
        assert!(assistant.status().await.available);
        let answer = assistant
            .ask(input("теперь можно"), || std::future::ready(ready(&fake)), |_| {})
            .await
            .unwrap();
        assert!(answer.starts_with("turn 2"), "разогрев, затем вопрос: {answer}");
        let next = assistant.ask(input("и ещё"), no_setup(), |_| {}).await.unwrap();
        assert!(next.starts_with("turn 3"), "{next}");

        // И обратно: включили снова — снова отказ.
        crate::network_policy::set_for_test(true);
        let refused = assistant.ask(input("опять"), no_setup(), |_| {}).await.unwrap_err();
        assert_eq!(refused, crate::claude_cli::LOCAL_ONLY_REFUSAL);
        crate::network_policy::set_for_test(false);
        assistant.stop().await;
    }

    #[tokio::test]
    async fn a_crash_restarts_with_the_tail() {
        let _policy = online();
        let fake = FakeClaude::new("crash-second");
        let assistant = LiveAssistant::default();
        start(&assistant, ready(&fake)).await; // разогрев — ход 1, вопрос — ход 2 → падение
        let lines = [line(1, 10.0, "старая реплика"), line(2, 1500.0, "свежая реплика")];
        let asked = AskInput { question: "после падения", asked_at: 1600.0, lines: &lines, notes: &[] };
        let answer = assistant.ask(asked, no_setup(), |_| {}).await.unwrap();
        assert!(answer.starts_with("turn 1"), "новый процесс отвечает с первого хода: {answer}");
        let sent = fake.message(1).expect("новый процесс получил сообщение");
        assert!(sent.contains("перезапущен"), "{sent}");
        assert!(sent.contains("свежая реплика"), "хвост 15 минут: {sent}");
        assert!(!sent.contains("старая реплика"), "старше 15 минут не уходит: {sent}");
        assert!(sent.contains("после падения"), "{sent}");
        assistant.stop().await;
    }

    #[tokio::test]
    async fn names_are_hidden_from_claude_and_restored_in_the_answer() {
        use crate::privacy::anonymize::Vocabulary;
        let _policy = online();
        let fake = FakeClaude::new("parrot"); // отвечает присланным текстом
        let assistant = LiveAssistant::default();
        let shield = StdMutex::new(PrivacySession::new(Vocabulary { names: vec!["Анна".into()], terms: Vec::new() }));
        let StartSetup::Ready { config, .. } = ready(&fake) else { unreachable!() };
        start(&assistant, StartSetup::Ready { config, shield: Some(shield) }).await;
        let mut seen = Vec::new();
        let answer = assistant
            .ask(input("Что сказала Анна?"), no_setup(), |text| seen.push(text.to_string()))
            .await
            .unwrap();
        let sent = fake.message(2).expect("вопрос дошёл");
        assert!(!sent.contains("Анна"), "имя не уходит в Claude: {sent}");
        assert!(sent.contains("[Имя 1]"), "{sent}");
        assert!(answer.contains("Что сказала Анна?"), "имя вернулось в ответе: {answer}");
        assert!(!answer.contains("[Имя"), "{answer}");
        assert!(seen.iter().all(|text| !text.contains("[Имя")), "поток в окно — тоже с именами: {seen:?}");
        assert_eq!(seen.last(), Some(&answer));
        assistant.stop().await;
    }

    #[tokio::test]
    async fn a_failed_turn_is_shown_not_counted_as_a_crash() {
        // Лимит подписки: Claude жив и отвечает ошибкой — её текст уходит в панель,
        // ассистент не выключается, сколько бы таких ответов ни было. Разогрев тоже.
        let _policy = online();
        let fake = FakeClaude::new("limit");
        let assistant = LiveAssistant::default();
        start(&assistant, ready(&fake)).await;
        assert!(assistant.inner.lock().await.as_ref().unwrap().crashes.is_empty(), "ошибка разогрева — не падение");
        for _ in 0..4 {
            let error = assistant.ask(input("x"), no_setup(), |_| {}).await.unwrap_err();
            assert!(error.contains("usage limit reached"), "{error}");
        }
        assert!(assistant.inner.lock().await.as_ref().unwrap().crashes.is_empty());
        let status = assistant.status().await;
        assert!(status.available, "{:?}", status.reason);
        assistant.stop().await;
    }

    #[tokio::test]
    async fn three_crashes_in_five_minutes_switch_it_off() {
        let _policy = online();
        let fake = FakeClaude::new("silent");
        let assistant = LiveAssistant::default();
        start(&assistant, ready_with(&fake, Duration::from_secs(2))).await;
        for _ in 0..3 {
            let _ = assistant.ask(input("x"), no_setup(), |_| {}).await;
        }
        let status = assistant.status().await;
        assert!(!status.available);
        assert!(status.reason.unwrap().contains("выключен до конца записи"));
        assistant.stop().await;
    }

    #[tokio::test]
    async fn stop_during_answer_ends_the_question_at_once() {
        // `stuck` молчит и не выходит по закрытому stdin: штатное закрытие ждало бы 5 с.
        let _policy = online();
        let fake = FakeClaude::new("stuck");
        let assistant = Arc::new(LiveAssistant::default());
        start(&assistant, ready_with(&fake, Duration::from_secs(4))).await; // разогрев молчит → падение
        let warm_up_pid = fake.pid("fake").expect("процесс разогрева запускался");
        let asking = {
            let assistant = assistant.clone();
            tokio::spawn(async move { assistant.ask(input("x"), no_setup(), |_| {}).await })
        };
        // Вопрос поднимает новый процесс; ждём, пока он запишет свой PID и прочтёт вопрос.
        let mut pid = None;
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if let Some(now) = fake.pid("fake").filter(|now| *now != warm_up_pid) {
                if fake.message(1).is_some_and(|text| text.contains("перезапущен")) {
                    pid = Some(now);
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let pid = pid.expect("процесс вопроса запущен и получил вопрос");
        let started = Instant::now();
        assistant.stop().await;
        let result = tokio::time::timeout(Duration::from_secs(2), asking).await.expect("не повис").unwrap();
        assert!(result.unwrap_err().contains("остановлена"));
        assert!(started.elapsed() < Duration::from_secs(2), "ответ «остановлена» сразу: {:?}", started.elapsed());
        assert!(gone(pid, Duration::from_secs(8)).await, "процесс claude погашен после записи");
    }

    #[tokio::test]
    async fn a_question_queued_when_the_recording_stops_says_stopped() {
        let _policy = online();
        let fake = FakeClaude::new("slow");
        let assistant = Arc::new(LiveAssistant::default());
        start(&assistant, ready(&fake)).await;
        let ask = |question: &'static str| {
            let assistant = assistant.clone();
            tokio::spawn(async move { assistant.ask(input(question), no_setup(), |_| {}).await })
        };
        let first = ask("первый");
        tokio::time::sleep(Duration::from_millis(100)).await;
        let second = ask("второй");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assistant.stop().await;
        assert_eq!(first.await.unwrap().unwrap_err(), STOPPED);
        assert_eq!(second.await.unwrap().unwrap_err(), STOPPED, "не «Запись не идёт»");
    }

    #[tokio::test]
    async fn stop_during_warm_up_leaves_nothing_running() {
        // Конец записи, пока диалог ещё разогревается: процесс закрыт, после stop ничего не ставится.
        let fake = FakeClaude::new("slow-warmup"); // на разогрев отвечает через 3 с
        let assistant = Arc::new(LiveAssistant::default());
        let starting = {
            let assistant = assistant.clone();
            let setup = ready(&fake);
            tokio::spawn(async move { start(&assistant, setup).await })
        };
        assert!(fake.wait_for(FakeClaude::started, Duration::from_secs(15)).await, "подделка не получила разогрев");
        assistant.stop().await;
        tokio::time::timeout(Duration::from_secs(6), starting).await.expect("start не повис").unwrap();
        let status = assistant.status().await;
        assert_eq!(status.reason.as_deref(), Some("Запись не идёт"), "после stop диалог не поставлен");
        assert!(fake.wait_for(FakeClaude::exited, Duration::from_secs(6)).await, "процесс claude не закрыт");
    }

    #[tokio::test]
    async fn stop_during_setup_leaves_nothing_running() {
        // Запись кончилась, пока слушатель ещё проверял Claude Code (probe, 1–3 с).
        let fake = FakeClaude::new("echo");
        let assistant = LiveAssistant::default();
        let token = assistant.begin();
        assistant.stop().await;
        assistant.start(ready(&fake), token).await;
        assert_eq!(assistant.status().await.reason.as_deref(), Some("Запись не идёт"));
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(fake.pid("fake").is_none() && !fake.started(), "процесс claude не запускался");
    }

    #[tokio::test]
    async fn a_late_stop_leaves_the_next_recording_alone() {
        // Конец первой записи закрывается с опозданием, а вторая уже началась.
        let _policy = online();
        let (first, second) = (FakeClaude::new("echo"), FakeClaude::new("echo"));
        let assistant = LiveAssistant::default();
        start(&assistant, ready(&first)).await;
        let ended = assistant.request_stop();
        start(&assistant, ready(&second)).await;
        assistant.finish_stop(ended).await;
        assert!(assistant.status().await.available, "диалог второй записи на месте");
        let answer = assistant.ask(input("x"), no_setup(), |_| {}).await.unwrap();
        assert!(answer.starts_with("turn 2"), "{answer}");
        assert!(first.wait_for(FakeClaude::exited, Duration::from_secs(6)).await, "процесс первой записи закрыт");
        assistant.stop().await;
    }
}
