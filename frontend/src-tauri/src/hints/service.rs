//! Подсказки на время записи: слот кандидатов (свежий вытесняет старый,
//! хоткей не вытесняется), один запрос к диалогу одновременно, разбор ответа
//! по мере стрима, счётчики «проверено / показано» и последнее событие для
//! окна, которое открылось позже события.

use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex as StdMutex;

use serde::Serialize;

use super::reply::{self, Reply};
use super::{CANDIDATE_LABEL, HOTKEY_LABEL, HOTKEY_REQUEST};
use crate::audio::recording_saver::TranscriptSegment;
use crate::claude_session::manager::{AskInput, LiveAssistant, StartSetup};
use crate::claude_session::{AssistantTurn, TurnKind};
use crate::meeting_notes::Note;

#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    Candidate { text: String, at: f64 },
    Hotkey { at: f64 },
}

#[derive(Default)]
pub struct Slot {
    pending: Option<Request>,
}

impl Slot {
    pub fn put(&mut self, request: Request) {
        let hotkey_waits = matches!(self.pending, Some(Request::Hotkey { .. }));
        if hotkey_waits && matches!(request, Request::Candidate { .. }) {
            return;
        }
        self.pending = Some(request);
    }

    pub fn take(&mut self) -> Option<Request> {
        self.pending.take()
    }

    pub fn clear(&mut self) {
        self.pending = None;
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HintState {
    Writing,
    Done,
    /// Хоткей, а вопросов не было.
    Empty,
    Error,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HintEvent {
    pub id: String,
    pub kind: TurnKind,
    pub asked_at_sec: f64,
    pub question: String,
    pub short: String,
    pub details: String,
    pub state: HintState,
}

#[derive(Default)]
pub struct HintService {
    pub dialog: LiveAssistant,
    slot: StdMutex<Slot>,
    running: AtomicBool,
    active: AtomicBool,
    mic: AtomicBool,
    checked: AtomicU64,
    shown: AtomicU64,
    last: StdMutex<Option<HintEvent>>,
    hotkey_error: StdMutex<Option<String>>,
}

fn lock<T>(mutex: &StdMutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl HintService {
    pub fn begin_recording(&self, enabled: bool, mic: bool) {
        lock(&self.slot).clear();
        *lock(&self.last) = None;
        *lock(&self.hotkey_error) = None;
        self.checked.store(0, Ordering::SeqCst);
        self.shown.store(0, Ordering::SeqCst);
        self.mic.store(mic, Ordering::SeqCst);
        self.active.store(enabled, Ordering::SeqCst);
    }

    pub fn end_recording(&self) {
        self.active.store(false, Ordering::SeqCst);
        lock(&self.slot).clear();
    }

    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }

    pub fn listens_to_mic(&self) -> bool {
        self.mic.load(Ordering::SeqCst)
    }

    pub fn counts(&self) -> (u64, u64) {
        (self.checked.load(Ordering::SeqCst), self.shown.load(Ordering::SeqCst))
    }

    pub fn last_event(&self) -> Option<HintEvent> {
        lock(&self.last).clone()
    }

    pub fn set_hotkey_error(&self, error: Option<String>) {
        *lock(&self.hotkey_error) = error;
    }

    pub fn hotkey_error(&self) -> Option<String> {
        lock(&self.hotkey_error).clone()
    }

    pub fn offer(&self, request: Request) -> bool {
        lock(&self.slot).put(request);
        !self.running.swap(true, Ordering::SeqCst)
    }

    pub async fn drain<F, Fut>(&self, mut handle: F)
    where
        F: FnMut(Request) -> Fut,
        Fut: Future<Output = ()>,
    {
        // Паника обработчика или брошенный future не должны оставить «воркер
        // занят» навсегда. На обычном выходе флагом уже распорядились ниже
        // (он мог перейти к другому воркеру), поэтому сторож снимается.
        struct Running<'a> {
            flag: &'a AtomicBool,
            armed: bool,
        }
        impl Drop for Running<'_> {
            fn drop(&mut self) {
                if self.armed {
                    self.flag.store(false, Ordering::SeqCst);
                }
            }
        }
        let mut guard = Running { flag: &self.running, armed: true };
        loop {
            let next = lock(&self.slot).take();
            match next {
                Some(request) => handle(request).await,
                None => {
                    self.running.store(false, Ordering::SeqCst);
                    // Запрос мог лечь между `take` и сбросом флага: тогда его воркер — мы.
                    let waiting = lock(&self.slot).pending.is_some();
                    if waiting && !self.running.swap(true, Ordering::SeqCst) {
                        continue;
                    }
                    guard.armed = false;
                    return;
                }
            }
        }
    }

    pub async fn answer<S, SFut>(
        &self,
        request: &Request,
        lines: &[TranscriptSegment],
        notes: &[Note],
        setup: S,
        mut on_event: impl FnMut(&HintEvent),
    ) -> Option<AssistantTurn>
    where
        S: FnOnce() -> SFut,
        SFut: Future<Output = StartSetup>,
    {
        let (kind, label, text, at) = match request {
            Request::Candidate { text, at } => (TurnKind::Hint, CANDIDATE_LABEL, text.as_str(), *at),
            Request::Hotkey { at } => (TurnKind::HintHotkey, HOTKEY_LABEL, HOTKEY_REQUEST, *at),
        };
        let mut event = HintEvent {
            id: uuid::Uuid::new_v4().to_string(),
            kind,
            asked_at_sec: at,
            question: String::new(),
            short: String::new(),
            details: String::new(),
            state: HintState::Writing,
        };
        let mut shown = false;
        let mut emit = |event: &HintEvent, shown: &mut bool| {
            *shown = true;
            *lock(&self.last) = Some(event.clone());
            on_event(event);
        };
        if kind == TurnKind::HintHotkey {
            emit(&event, &mut shown);
        }
        let model = self.dialog.status().await.model.unwrap_or_default();
        let input = AskInput { label, question: text, asked_at: at, lines, notes };
        let result = self
            .dialog
            .ask(input, setup, |sofar| {
                if let Reply::Hint(hint) = reply::parse(sofar) {
                    if hint.question != event.question || hint.short != event.short || hint.details != event.details {
                        event.question = hint.question;
                        event.short = hint.short;
                        event.details = hint.details;
                        emit(&event, &mut shown);
                    }
                }
            })
            .await;
        if kind == TurnKind::Hint {
            self.checked.fetch_add(1, Ordering::SeqCst);
        }
        match result.map(|text| reply::parse(&text)) {
            Ok(Reply::Hint(hint)) => {
                event.question = if hint.question.is_empty() && kind == TurnKind::Hint { text.trim().to_string() } else { hint.question };
                event.short = hint.short;
                event.details = hint.details;
                event.state = HintState::Done;
                emit(&event, &mut shown);
                self.shown.fetch_add(1, Ordering::SeqCst);
                log::info!("Подсказка: коротко {} симв., подробно {} симв.", event.short.chars().count(), event.details.chars().count());
                Some(AssistantTurn {
                    id: event.id.clone(),
                    asked_at_sec: at,
                    question: event.question.clone(),
                    answer: serde_json::json!({ "short": event.short, "details": event.details }).to_string(),
                    model,
                    created_at: chrono::Utc::now().to_rfc3339(),
                    kind,
                })
            }
            Ok(_) => {
                // Окно хоткея открыто сразу; окно кандидата — если посреди стрима
                // текст выглядел подсказкой («*»), а итог — «НЕТ». Не оставляем «пишет».
                if kind == TurnKind::HintHotkey || shown {
                    event.state = HintState::Empty;
                    event.question.clear();
                    event.short.clear();
                    event.details.clear();
                    emit(&event, &mut shown);
                }
                None
            }
            Err(error) => {
                log::warn!("Подсказка не получена: {}", error.lines().next().unwrap_or("").chars().take(120).collect::<String>());
                if shown {
                    event.state = HintState::Error;
                    // Недописанное «Подробно» под ошибкой не показываем; вопрос оставляем.
                    event.details.clear();
                    event.short = error;
                    emit(&event, &mut shown);
                }
                None
            }
        }
    }
}

#[cfg(test)]
mod slot_tests {
    use super::*;

    fn candidate(text: &str) -> Request {
        Request::Candidate { text: text.into(), at: 1.0 }
    }

    #[test]
    fn slot_keeps_only_the_latest_candidate() {
        let mut slot = Slot::default();
        slot.put(candidate("первый"));
        slot.put(candidate("второй"));
        assert_eq!(slot.take(), Some(candidate("второй")));
        assert_eq!(slot.take(), None);
    }

    #[test]
    fn slot_keeps_a_hotkey_over_candidates() {
        let mut slot = Slot::default();
        slot.put(Request::Hotkey { at: 5.0 });
        slot.put(candidate("позже"));
        assert_eq!(slot.take(), Some(Request::Hotkey { at: 5.0 }));
        slot.put(candidate("раньше"));
        slot.put(Request::Hotkey { at: 9.0 });
        assert_eq!(slot.take(), Some(Request::Hotkey { at: 9.0 }), "хоткей вытесняет кандидата");
    }

    #[tokio::test]
    async fn a_panicking_handler_does_not_leave_the_worker_running() {
        let service = std::sync::Arc::new(HintService::default());
        assert!(service.offer(candidate("первый")));
        let worker = {
            let service = service.clone();
            tokio::spawn(async move { service.drain(|_| async { panic!("обработчик упал") }).await })
        };
        assert!(worker.await.is_err(), "паника дошла до задачи");
        assert!(service.offer(candidate("второй")), "воркера нет — запускать снова");
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::claude_session::dialog::DialogConfig;
    use crate::claude_session::fake_claude::FakeClaude;
    use std::sync::Arc;
    use std::time::Duration;

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

    fn no_setup() -> impl FnOnce() -> std::future::Ready<StartSetup> {
        || std::future::ready(StartSetup::Unavailable("настройка не ожидалась".into()))
    }

    fn online() -> std::sync::MutexGuard<'static, ()> {
        let guard = crate::network_policy::test_lock();
        crate::network_policy::set_for_test(false);
        guard
    }

    async fn started(service: &HintService, setup: StartSetup) {
        service.begin_recording(true, false);
        let token = service.dialog.begin();
        service.dialog.start(setup, token).await;
    }

    fn candidate(text: &str) -> Request {
        Request::Candidate { text: text.into(), at: 42.0 }
    }

    #[tokio::test]
    async fn a_question_becomes_a_shown_hint_with_a_turn() {
        let _policy = online();
        let fake = FakeClaude::new("hint");
        let service = HintService::default();
        started(&service, ready(&fake)).await;
        let mut events = Vec::new();
        let turn = service
            .answer(&candidate("чем 3нф отличается от 2нф"), &[], &[], no_setup(), |e| events.push(e.clone()))
            .await
            .expect("подсказка сохраняется");
        assert_eq!(turn.kind, TurnKind::Hint);
        assert_eq!(turn.question, "Чем 3НФ отличается от 2НФ?");
        let answer: serde_json::Value = serde_json::from_str(&turn.answer).unwrap();
        assert_eq!(answer["short"], "В 3НФ нет транзитивных зависимостей.");
        assert_eq!(answer["details"], "Ключ -> A -> B запрещено.");
        assert_eq!(turn.asked_at_sec, 42.0);
        let last = events.last().unwrap();
        assert_eq!(last.state, HintState::Done);
        assert_eq!(last.id, turn.id);
        assert_eq!(service.counts(), (1, 1));
        assert_eq!(service.last_event().as_ref(), Some(last));
        // Сообщение подписано меткой кандидата.
        assert!(fake.message(2).unwrap().contains("Фраза-кандидат (на 0:42): чем 3нф"));
        service.dialog.stop().await;
    }

    #[tokio::test]
    async fn not_a_question_is_dropped_silently() {
        let _policy = online();
        let fake = FakeClaude::new("hint");
        let service = HintService::default();
        started(&service, ready(&fake)).await;
        let mut events = Vec::new();
        let turn = service.answer(&candidate("НЕ_ВОПРОС то что мы видим"), &[], &[], no_setup(), |e| events.push(e.clone())).await;
        assert_eq!(turn, None);
        assert!(events.is_empty(), "окно не трогаем: {events:?}");
        assert_eq!(service.counts(), (1, 0));
        service.dialog.stop().await;
    }

    #[tokio::test]
    async fn hotkey_shows_writing_at_once_and_saves_the_hint() {
        let _policy = online();
        let fake = FakeClaude::new("hint");
        let service = HintService::default();
        started(&service, ready(&fake)).await;
        let mut events = Vec::new();
        // Запрос по хоткею без НЕ_ВОПРОС — подделка отвечает подсказкой.
        let turn = service.answer(&Request::Hotkey { at: 7.0 }, &[], &[], no_setup(), |e| events.push(e.clone())).await.unwrap();
        assert_eq!(turn.kind, TurnKind::HintHotkey);
        assert_eq!(events.first().unwrap().state, HintState::Writing, "окно открывается сразу");
        assert_eq!(service.counts(), (0, 1), "хоткей — не проверка кандидата");
        service.dialog.stop().await;
    }

    #[tokio::test]
    async fn hotkey_without_questions_ends_empty() {
        let _policy = online();
        let fake = FakeClaude::new("hint-none");
        let service = HintService::default();
        started(&service, ready(&fake)).await;
        let mut events = Vec::new();
        let turn = service.answer(&Request::Hotkey { at: 7.0 }, &[], &[], no_setup(), |e| events.push(e.clone())).await;
        assert_eq!(turn, None);
        assert_eq!(events.first().unwrap().state, HintState::Writing, "окно открывается сразу");
        let last = events.last().unwrap();
        assert_eq!(last.state, HintState::Empty, "{events:?}");
        assert_eq!(service.last_event().as_ref(), Some(last));
        assert_eq!(service.counts(), (0, 0));
        service.dialog.stop().await;
    }

    #[tokio::test]
    async fn a_candidate_shown_mid_stream_does_not_stay_writing_after_no() {
        let _policy = online();
        // Первая дельта «*» разбирается как подсказка, итог «**НЕТ**» — отказ.
        let fake = FakeClaude::new("hint-none");
        let service = HintService::default();
        started(&service, ready(&fake)).await;
        let mut events = Vec::new();
        let turn = service.answer(&candidate("ну что скажете"), &[], &[], no_setup(), |e| events.push(e.clone())).await;
        assert_eq!(turn, None);
        assert_eq!(events.first().map(|e| e.state), Some(HintState::Writing), "окно показалось посреди стрима");
        let last = events.last().unwrap();
        assert_eq!(last.state, HintState::Empty, "окно не залипает в «пишет»: {events:?}");
        assert!(last.short.is_empty() && last.details.is_empty(), "{last:?}");
        assert_eq!(service.counts(), (1, 0));
        service.dialog.stop().await;
    }

    #[tokio::test]
    async fn hotkey_on_unavailable_dialog_shows_the_reason() {
        let service = HintService::default();
        started(&service, StartSetup::Unavailable("Claude Code: вход не выполнен".into())).await;
        let mut events = Vec::new();
        let turn = service.answer(&Request::Hotkey { at: 7.0 }, &[], &[], no_setup(), |e| events.push(e.clone())).await;
        assert_eq!(turn, None);
        let last = events.last().unwrap();
        assert_eq!(last.state, HintState::Error);
        assert!(last.short.contains("вход не выполнен"), "{last:?}");
    }

    #[tokio::test]
    async fn stop_mid_hint_saves_nothing() {
        let _policy = online();
        let fake = FakeClaude::new("slow"); // ответ через 800 мс
        let service = Arc::new(HintService::default());
        started(&service, ready(&fake)).await;
        let asking = {
            let service = service.clone();
            tokio::spawn(async move {
                service.answer(&Request::Hotkey { at: 1.0 }, &[], &[], no_setup(), |_| {}).await
            })
        };
        tokio::time::sleep(Duration::from_millis(200)).await;
        service.dialog.stop().await;
        service.end_recording();
        let turn = tokio::time::timeout(Duration::from_secs(5), asking).await.unwrap().unwrap();
        assert_eq!(turn, None);
        assert_eq!(service.last_event().map(|e| e.state), Some(HintState::Error));
    }

    #[tokio::test]
    async fn a_burst_while_busy_asks_once_more_with_the_latest() {
        let service = Arc::new(HintService::default());
        service.begin_recording(true, false);
        let seen = Arc::new(std::sync::Mutex::new(Vec::<Request>::new()));
        assert!(service.offer(candidate("первый")), "воркера нет — запускать");
        assert!(!service.offer(candidate("второй")), "воркер уже есть");
        let worker = {
            let service = service.clone();
            let seen = seen.clone();
            tokio::spawn(async move {
                let inner = service.clone();
                service
                    .drain(move |request| {
                        let seen = seen.clone();
                        let inner = inner.clone();
                        async move {
                            if seen.lock().unwrap().is_empty() {
                                // Пока «Claude» отвечает на первый, приходят ещё два.
                                assert!(!inner.offer(candidate("третий")));
                                assert!(!inner.offer(candidate("четвёртый")));
                            }
                            seen.lock().unwrap().push(request);
                        }
                    })
                    .await
            })
        };
        worker.await.unwrap();
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen, vec![candidate("второй"), candidate("четвёртый")]);
        assert!(service.offer(candidate("пятый")), "воркер закончился — снова запускать");
    }
}
