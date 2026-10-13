//! Подсказки в приложении: диалог на время записи, приём строк расшифровки,
//! команды окна и вкладки.

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Runtime, State};

use super::service::{HintEvent, HintService, Request};
use super::{prefilter, HINT_SYSTEM_PROMPT};
use crate::audio::recording_commands::TranscriptUpdate;
use crate::claude_session::manager::StartSetup;
use crate::claude_session::{AssistantTurn, TurnKind};
use crate::database::repositories::setting::{HintSettings, SettingsRepository};

const OFF: &str = "Подсказки выключены: Настройки → Саммари → Claude Code.";
const MIC_SOURCE: &str = "You";

/// Какие строки расшифровки доходят до префильтра: только финальные, только
/// пока подсказки этой записи включены, с микрофона — только по переключателю.
pub fn wants(source: &str, is_partial: bool, text: &str, active: bool, mic: bool) -> bool {
    if is_partial || !active {
        return false;
    }
    if source.trim() == MIC_SOURCE && !mic {
        return false;
    }
    prefilter::is_candidate(text)
}

/// Диалог подсказок на запись: выключены в настройках — понятный отказ,
/// иначе тот же Claude Code, что у «Ассистента», со своим промптом.
pub async fn setup_for_hints(pool: &sqlx::SqlitePool, settings: &HintSettings) -> StartSetup {
    if !settings.enabled {
        return StartSetup::Unavailable(OFF.into());
    }
    crate::claude_session::commands::setup_with_prompt(pool, HINT_SYSTEM_PROMPT).await
}

/// Строка расшифровки из `transcript-update`: кандидат — в слот.
pub fn on_transcript<R: Runtime>(app: &AppHandle<R>, update: &TranscriptUpdate) {
    let service = app.state::<HintService>();
    if wants(&update.source, update.is_partial, &update.text, service.is_active(), service.listens_to_mic()) {
        submit(app, Request::Candidate { text: update.text.clone(), at: update.audio_end_time });
    }
}

/// Запрос в слот; если воркера нет — запустить его. Воркер разбирает слот,
/// пока тот не опустеет.
pub fn submit<R: Runtime>(app: &AppHandle<R>, request: Request) {
    if !app.state::<HintService>().offer(request) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let service = app.state::<HintService>();
        service.drain(|request| process(&app, request)).await;
    });
}

async fn process<R: Runtime>(app: &AppHandle<R>, request: Request) {
    let Some(state) = app.try_state::<crate::state::AppState>() else { return };
    let pool = state.db_manager.pool();
    if matches!(request, Request::Hotkey { .. }) {
        // Как вопрос «Ассистенту»: дождаться только что сказанного. В лог —
        // только исход и время.
        let started = std::time::Instant::now();
        let outcome = crate::audio::live_flush::flush_before_question().await;
        log::debug!("Подсказка по хоткею: сброс живой речи — {:?} за {} мс", outcome, started.elapsed().as_millis());
    }
    let lines = crate::audio::recording_commands::get_transcript_history().await.unwrap_or_default();
    let notes = crate::audio::transcript_journal::current_notes();
    let settings = SettingsRepository::get_hint_settings(pool).await.unwrap_or_default();
    let service = app.state::<HintService>();
    let turn = service
        .answer(&request, &lines, &notes, || setup_for_hints(pool, &settings), |event| {
            // Запись остановлена — окно спрятано; поздние события (в том числе
            // «остановлено» от самого диалога) его снова не открывают.
            if !service.is_active() {
                return;
            }
            // Кандидат, оказавшийся «НЕТ», окно не открывает (иначе мелькнёт
            // прошлая карточка); событие всё равно уходит — окно по нему прячется.
            let rejected_candidate =
                event.state == super::service::HintState::Empty && !matches!(event.kind, TurnKind::HintHotkey);
            if !rejected_candidate {
                crate::hint_window::show(app);
            }
            let _ = app.emit("hint-update", event);
        })
        .await;
    if let Some(turn) = turn {
        if let Err(error) = crate::audio::transcript_journal::add_turn(&turn) {
            log::warn!("Подсказка не попала в журнал записи: {error}");
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HintsStatus {
    pub enabled: bool,
    pub available: bool,
    pub reason: Option<String>,
    pub checked: u64,
    pub shown: u64,
    pub hotkey_error: Option<String>,
}

#[tauri::command]
pub async fn hints_status(service: State<'_, HintService>) -> Result<HintsStatus, String> {
    let status = service.dialog.status().await;
    let (checked, shown) = service.counts();
    Ok(HintsStatus {
        enabled: service.is_active(),
        available: status.available,
        reason: status.reason,
        checked,
        shown,
        hotkey_error: service.hotkey_error(),
    })
}

/// Подсказки текущей записи для вкладки (вопросы «Ассистенту» — не здесь).
#[tauri::command]
pub async fn hints_live() -> Result<Vec<AssistantTurn>, String> {
    Ok(crate::audio::transcript_journal::current_turns()
        .into_iter()
        .filter(|turn| turn.kind != TurnKind::Question)
        .collect())
}

/// Последнее событие — для окна, открывшегося позже него.
#[tauri::command]
pub async fn hints_current(service: State<'_, HintService>) -> Result<Option<HintEvent>, String> {
    Ok(service.last_event())
}

#[cfg(test)]
mod tests {
    use super::wants;

    #[test]
    fn only_final_call_lines_reach_the_prefilter() {
        let question = "назовите три свойства транзакции";
        assert!(wants("Speaker 1", false, question, true, false));
        assert!(wants("Guest", false, question, true, false));
        assert!(!wants("Speaker 1", true, question, true, false), "промежуточная строка");
        assert!(!wants("Speaker 1", false, question, false, false), "подсказки выключены");
        assert!(!wants("You", false, question, true, false), "свой микрофон без переключателя");
        assert!(wants("You", false, question, true, true), "очная пара");
        assert!(!wants("Speaker 1", false, "запишем это в таблицу", true, false), "не кандидат");
    }
}
