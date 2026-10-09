//! Команды ассистента записи для окна: вопрос, статус, ходы текущей записи.

use serde::Serialize;
use tauri::{AppHandle, Emitter, Runtime, State};

use super::manager::{AskInput, AssistantStatus, LiveAssistant, StartSetup, IDLE_TIMEOUT};
use super::{AssistantTurn, SYSTEM_PROMPT};
use crate::database::repositories::setting::SettingsRepository;
use crate::summary::llm_client::LLMProvider;

const NEEDS_CLAUDE: &str =
    "Ассистенту нужен Claude: выберите «Claude Code (подписка)» или «Claude» в Настройки → Саммари.";

#[derive(Serialize, Clone)]
struct AnswerEvent<'a> {
    id: &'a str,
    text: &'a str,
    done: bool,
}

/// Что делать с ассистентом при старте записи — по провайдеру из настроек.
pub async fn setup_for_recording(pool: &sqlx::SqlitePool) -> StartSetup {
    let Ok(Some(config)) = SettingsRepository::get_model_config(pool).await else {
        return StartSetup::Unavailable("Модель ИИ не выбрана: Настройки → Саммари".into());
    };
    if config.provider != "claude-cli" {
        // Claude по ключу отвечает разовыми вызовами — диалог не нужен; остальным ассистент недоступен.
        return StartSetup::Unavailable(String::new());
    }
    if crate::network_policy::local_only() {
        // Не выключаем ассистента на всю запись: отказ — пока режим включён,
        // первый вопрос после выключения поднимет диалог (manager::ask).
        return StartSetup::Deferred;
    }
    let path = SettingsRepository::get_claude_cli_path(pool).await.ok().flatten();
    let status = crate::claude_cli::probe(path.as_deref()).await;
    if !status.installed {
        return StartSetup::Unavailable(
            "Claude Code не найден. Установите его и выполните вход, затем начните запись заново.".into(),
        );
    }
    if !status.auth.as_ref().map(|auth| auth.logged_in).unwrap_or(false) {
        return StartSetup::Unavailable(
            "Claude Code: вход не выполнен. Выполните в терминале claude auth login и начните запись заново.".into(),
        );
    }
    let binary = match crate::claude_cli::resolve_binary(path.as_deref()) {
        Ok(binary) => binary,
        Err(error) => return StartSetup::Unavailable(error),
    };
    let model = SettingsRepository::get_claude_live_model(pool)
        .await
        .unwrap_or_else(|_| crate::claude_cli::DEFAULT_LIVE_MODEL.into());
    let shield = crate::privacy::vocabulary::shield_for(pool, None).await;
    StartSetup::Ready {
        config: super::dialog::DialogConfig {
            binary,
            model,
            system_prompt: SYSTEM_PROMPT.into(),
            idle_timeout: IDLE_TIMEOUT,
        },
        shield,
    }
}

#[tauri::command]
pub async fn assistant_status(
    assistant: State<'_, LiveAssistant>,
    state: State<'_, crate::state::AppState>,
) -> Result<AssistantStatus, String> {
    let status = assistant.status().await;
    if status.reason.as_deref() == Some("") {
        // Провайдер не claude-cli: доступен ли разовый Claude по ключу?
        let provider = SettingsRepository::get_model_config(state.db_manager.pool())
            .await
            .ok()
            .flatten()
            .map(|config| config.provider);
        return Ok(match provider.as_deref() {
            Some("claude") => AssistantStatus { available: true, reason: None, model: None },
            _ => AssistantStatus { available: false, reason: Some(NEEDS_CLAUDE.into()), model: None },
        });
    }
    Ok(status)
}

#[tauri::command]
pub async fn assistant_ask<R: Runtime>(
    app: AppHandle<R>,
    assistant: State<'_, LiveAssistant>,
    state: State<'_, crate::state::AppState>,
    id: String,
    question: String,
) -> Result<AssistantTurn, String> {
    let question = question.trim().to_string();
    if question.is_empty() {
        return Err("Пустой вопрос".into());
    }
    let asked_at = crate::audio::recording_commands::recorded_seconds().unwrap_or(0.0);
    let lines = crate::audio::recording_commands::get_transcript_history().await.unwrap_or_default();
    let notes = crate::audio::transcript_journal::current_notes();
    let pool = state.db_manager.pool();
    let provider = SettingsRepository::get_model_config(pool)
        .await
        .ok()
        .flatten()
        .map(|config| config.provider)
        .unwrap_or_default();

    let emit = |text: &str, done: bool| {
        let _ = app.emit("assistant-answer", AnswerEvent { id: &id, text, done });
    };
    let (answer, model) = if provider == "claude-cli" {
        let model = assistant.status().await.model.unwrap_or_default();
        let input = AskInput { question: &question, asked_at, lines: &lines, notes: &notes };
        let answer = assistant.ask(input, || setup_for_recording(pool), |text| emit(text, false)).await?;
        // Отложенный диалог узнаёт модель только при первом вопросе.
        let model = if model.is_empty() { assistant.status().await.model.unwrap_or_default() } else { model };
        (answer, model)
    } else if LLMProvider::from_str(&provider) == Ok(LLMProvider::Claude) {
        let resolved = crate::live_assistant::resolve_assistant_model(pool).await?;
        let mut cursor = super::context::Cursor::default();
        let user_prompt = super::context::next_message(&mut cursor, &lines, &notes, &question, asked_at);
        let hidden = crate::privacy::vocabulary::shield_for(pool, None).await;
        let answer = crate::live_assistant::generate_assistant_answer(
            &resolved,
            SYSTEM_PROMPT,
            &user_prompt,
            1024,
            0.4,
            hidden.as_ref(),
        )
        .await?;
        (crate::privacy::restore(hidden.as_ref(), &answer), resolved.model_name.clone())
    } else {
        return Err(NEEDS_CLAUDE.into());
    };
    emit(&answer, true);

    let turn = AssistantTurn {
        id: id.clone(),
        asked_at_sec: asked_at,
        question,
        answer,
        model,
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    log::info!(
        "Ассистент ответил: вопрос {} симв., ответ {} симв.",
        turn.question.chars().count(),
        turn.answer.chars().count()
    );
    let (turn, _warning) = keep_turn(turn);
    Ok(turn)
}

/// Журналирует ход и отдаёт его вызывающему. Сбой журнала (записи нет, архив
/// заперт, диск) не отменяет ответ — он уже показан; теряется только его
/// сохранение, в лог — предупреждение. Второе значение — текст предупреждения.
pub(crate) fn keep_turn(turn: AssistantTurn) -> (AssistantTurn, Option<String>) {
    match crate::audio::transcript_journal::add_turn(&turn) {
        Ok(()) => (turn, None),
        Err(error) => {
            let warning = format!("Ответ ассистента не попал в журнал записи: {error}");
            log::warn!("{warning}");
            (turn, Some(warning))
        }
    }
}

#[tauri::command]
pub async fn assistant_live_turns() -> Result<Vec<AssistantTurn>, String> {
    Ok(crate::audio::transcript_journal::current_turns())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::transcript_journal;

    fn turn() -> AssistantTurn {
        AssistantTurn {
            id: "t".into(),
            asked_at_sec: 1.0,
            question: "Что такое p-значение?".into(),
            answer: "Вероятность **такого же** результата при верной H0.".into(),
            model: "haiku".into(),
            created_at: "2026-10-10T09:00:00Z".into(),
        }
    }

    #[test]
    fn answer_survives_journal_failure() {
        let _journal = transcript_journal::test_lock();

        // Журнала нет (запись без журнала): ответ отдан целиком, сбой — только предупреждением.
        let (kept, warning) = keep_turn(turn());
        assert_eq!(kept, turn(), "ответ возвращается вызывающему без потерь");
        let warning = warning.expect("сбой журнала виден предупреждением");
        assert!(warning.contains("не попал в журнал"), "{warning}");
        assert!(warning.contains("Запись не идёт"), "{warning}");
        assert!(transcript_journal::current_turns().is_empty());

        // Журнал есть: тот же ответ, без предупреждения, и ход виден окну после перезагрузки.
        let dir = tempfile::tempdir().unwrap();
        transcript_journal::begin(dir.path(), "пара", "2026-10-10T09:00:00Z");
        let (kept, warning) = keep_turn(turn());
        assert_eq!(kept, turn());
        assert_eq!(warning, None);
        assert_eq!(transcript_journal::current_turns(), vec![turn()]);
        transcript_journal::end();
    }
}
