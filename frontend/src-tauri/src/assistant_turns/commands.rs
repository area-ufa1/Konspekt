use tauri::State;

use crate::claude_session::AssistantTurn;

#[tauri::command]
pub async fn api_get_assistant_turns(state: State<'_, crate::state::AppState>, meeting_id: String) -> Result<Vec<AssistantTurn>, String> {
    super::load(state.db_manager.pool(), &meeting_id).await.map_err(|error| {
        if crate::database::fields::is_archive_locked(&error) {
            "Архив заперт — сначала откройте его".to_string()
        } else {
            format!("Не удалось прочитать вопросы ассистенту: {error}")
        }
    })
}
