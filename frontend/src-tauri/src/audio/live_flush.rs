//! Сброс живой речи перед вопросом ассистенту.
//!
//! Живой конвейер закрывает фразу после паузы (или по лимиту длины), и до
//! этого её текста нет. Вопрос ассистенту не ждёт паузы: он просит конвейер
//! разрезать идущие фразы обоих источников сейчас и ждёт, пока их строки
//! дойдут до истории записи, откуда ассистент берёт текст, — но не дольше
//! [`FLUSH_BUDGET`]. Не успели (распознавание медленное, его нет) — вопрос
//! уходит с тем, что уже есть.
//!
//! Счёт простой: конвейер нумерует отправленные на распознавание куски
//! подряд, распознавание обрабатывает их по одному и по порядку. Разрезав
//! фразы по запросу, конвейер сообщает, сколько кусков отправлено к этому
//! моменту; вопрос ждёт, пока столько же будет обработано.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::Notify;

/// Метка запроса в аудиоканале конвейера. Вне диапазона сигнала остановки
/// (`chunk_id >= u64::MAX - 10`): тот сбрасывает остаток кольцевого буфера с
/// дополнением тишиной и годится только для конца записи.
pub const LIVE_CUT_CHUNK_ID: u64 = u64::MAX - 11;

/// Сколько вопрос ждёт строки до отправки.
pub const FLUSH_BUDGET: Duration = Duration::from_secs(4);

/// Где резать по запросу: самое тихое место последних стольких мс фразы.
pub const FLUSH_SEARCH_MS: u32 = 600;

/// Фразу короче не режем: в ней нечего ждать, а кусок короче 50 мс конвейер
/// и вовсе выбросил бы.
pub const FLUSH_MIN_MS: u32 = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlushOutcome {
    /// Записи с живым текстом нет — ждать нечего.
    Skipped,
    /// Строки всех разрезанных кусков в истории.
    Delivered,
    /// Бюджет вышел раньше.
    TimedOut,
}

/// Ход распознавания одной записи.
#[derive(Default)]
pub struct LiveProgress {
    /// Номер последнего запроса на разрез.
    requested: AtomicU64,
    /// (номер последнего обработанного конвейером запроса, кусков отправлено к нему).
    handled: Mutex<(u64, u64)>,
    /// Кусков обработано распознаванием.
    completed: AtomicU64,
    changed: Notify,
}

/// Ход распознавания идущей записи.
pub static PROGRESS: once_cell::sync::Lazy<LiveProgress> = once_cell::sync::Lazy::new(LiveProgress::default);

impl LiveProgress {
    /// Новая запись: её конвейер и распознавание считают с нуля.
    pub fn reset(&self) {
        {
            let mut handled = self.handled.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            // Запросы прошлой записи считаются обслуженными: новая их не ждёт.
            *handled = (self.requested.load(Ordering::SeqCst), 0);
            self.completed.store(0, Ordering::SeqCst);
        }
        self.changed.notify_waiters();
    }

    /// Распознавание закончило с очередным куском (с текстом или без).
    pub fn segment_done(&self) {
        self.completed.fetch_add(1, Ordering::SeqCst);
        self.changed.notify_waiters();
    }

    /// Номер запроса, который конвейер сейчас обслуживает.
    pub fn requested(&self) -> u64 {
        self.requested.load(Ordering::SeqCst)
    }

    /// Конвейер разрезал фразы по запросам до `request` включительно;
    /// на распознавание к этому моменту отправлено `sent` кусков.
    pub fn cut_handled(&self, request: u64, sent: u64) {
        {
            let mut handled = self.handled.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if request >= handled.0 {
                *handled = (request, sent);
            }
        }
        self.changed.notify_waiters();
    }

    /// Обслужен ли запрос `request` и обработано ли всё, что к нему отправлено.
    fn delivered(&self, request: u64) -> bool {
        let handled = *self.handled.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        handled.0 >= request && self.completed.load(Ordering::SeqCst) >= handled.1
    }

    /// Попросить разрез (`send` кладёт запрос в канал конвейера; `false` —
    /// конвейера нет) и ждать строк. Без границы по времени: её ставит
    /// [`flush_within`].
    pub async fn flush(&self, send: impl FnOnce() -> bool) -> FlushOutcome {
        let request = self.requested.fetch_add(1, Ordering::SeqCst) + 1;
        if !send() {
            return FlushOutcome::Skipped;
        }
        loop {
            // Подписка до проверки: изменение между ними не потеряется.
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.delivered(request) {
                return FlushOutcome::Delivered;
            }
            changed.await;
        }
    }
}

/// Перед вопросом ассистенту: разрезать идущие фразы записи и дождаться их
/// строк, не дольше [`FLUSH_BUDGET`]. Без записи с живым текстом — сразу.
pub async fn flush_before_question() -> FlushOutcome {
    if !crate::audio::recording_commands::recording_live_transcription() {
        return FlushOutcome::Skipped;
    }
    flush_within(&PROGRESS, crate::audio::recording_commands::request_live_cut, FLUSH_BUDGET).await
}

/// [`LiveProgress::flush`], но не дольше `budget`.
pub async fn flush_within(progress: &LiveProgress, send: impl FnOnce() -> bool, budget: Duration) -> FlushOutcome {
    match tokio::time::timeout(budget, progress.flush(send)).await {
        Ok(outcome) => outcome,
        Err(_) => FlushOutcome::TimedOut,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Instant;

    /// Конвейер и распознавание понарошку: на запрос режет `pieces` кусков,
    /// распознавание тратит на каждый `per_piece`.
    fn fake_pipeline(progress: Arc<LiveProgress>, already_sent: u64, pieces: u64, per_piece: Duration) {
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            let request = progress.requested();
            progress.cut_handled(request, already_sent + pieces);
            for _ in 0..already_sent + pieces {
                tokio::time::sleep(per_piece).await;
                progress.segment_done();
            }
        });
    }

    #[tokio::test]
    async fn no_live_pipeline_means_no_wait() {
        let progress = LiveProgress::default();
        let started = Instant::now();
        let outcome = flush_within(&progress, || false, Duration::from_secs(4)).await;
        assert_eq!(outcome, FlushOutcome::Skipped);
        assert!(started.elapsed() < Duration::from_millis(100));
    }

    #[tokio::test]
    async fn waits_until_the_cut_pieces_are_recognised() {
        let progress = Arc::new(LiveProgress::default());
        let sent = std::cell::Cell::new(false);
        // Два куска до вопроса ещё в очереди, разрез даёт два новых.
        fake_pipeline(progress.clone(), 2, 2, Duration::from_millis(50));
        let started = Instant::now();
        let outcome = flush_within(&progress, || { sent.set(true); true }, Duration::from_secs(4)).await;
        assert!(sent.get(), "запрос ушёл в конвейер");
        assert_eq!(outcome, FlushOutcome::Delivered);
        assert_eq!(progress.completed.load(Ordering::SeqCst), 4, "дождались всех четырёх, не раньше");
        assert!(started.elapsed() < Duration::from_secs(1), "{:?}", started.elapsed());
    }

    #[tokio::test]
    async fn slow_recognition_is_cut_off_by_the_budget() {
        let progress = Arc::new(LiveProgress::default());
        fake_pipeline(progress.clone(), 0, 3, Duration::from_secs(10));
        let started = Instant::now();
        let outcome = flush_within(&progress, || true, Duration::from_millis(300)).await;
        assert_eq!(outcome, FlushOutcome::TimedOut);
        let waited = started.elapsed();
        assert!(waited >= Duration::from_millis(290) && waited < Duration::from_millis(800), "{waited:?}");
    }

    #[tokio::test]
    async fn a_pipeline_that_never_answers_is_cut_off_too() {
        let progress = LiveProgress::default();
        let outcome = flush_within(&progress, || true, Duration::from_millis(200)).await;
        assert_eq!(outcome, FlushOutcome::TimedOut);
    }

    #[tokio::test]
    async fn nothing_to_cut_and_nothing_queued_is_delivered_at_once() {
        let progress = Arc::new(LiveProgress::default());
        fake_pipeline(progress.clone(), 0, 0, Duration::ZERO);
        let outcome = flush_within(&progress, || true, Duration::from_secs(4)).await;
        assert_eq!(outcome, FlushOutcome::Delivered);
    }

    #[tokio::test]
    async fn an_old_answer_does_not_release_a_new_question() {
        let progress = Arc::new(LiveProgress::default());
        // Прошлый вопрос: запрос 1 обслужен, отправлено 0.
        fake_pipeline(progress.clone(), 0, 0, Duration::ZERO);
        assert_eq!(flush_within(&progress, || true, Duration::from_secs(1)).await, FlushOutcome::Delivered);
        // Новый вопрос — конвейер молчит: старый ответ не считается.
        let outcome = flush_within(&progress, || true, Duration::from_millis(200)).await;
        assert_eq!(outcome, FlushOutcome::TimedOut);
    }

    #[tokio::test]
    async fn reset_starts_a_new_recording_from_zero() {
        let progress = Arc::new(LiveProgress::default());
        fake_pipeline(progress.clone(), 0, 2, Duration::ZERO);
        assert_eq!(flush_within(&progress, || true, Duration::from_secs(1)).await, FlushOutcome::Delivered);
        progress.reset();
        assert_eq!(progress.completed.load(Ordering::SeqCst), 0);
        // Новая запись: её первый кусок ещё не обработан — ждём его, а не берём счёт прошлой.
        fake_pipeline(progress.clone(), 0, 1, Duration::from_millis(100));
        let started = Instant::now();
        assert_eq!(flush_within(&progress, || true, Duration::from_secs(1)).await, FlushOutcome::Delivered);
        assert!(started.elapsed() >= Duration::from_millis(100), "{:?}", started.elapsed());
    }
}
