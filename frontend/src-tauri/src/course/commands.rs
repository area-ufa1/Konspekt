//! Материалы в записи: предмет, выбранный при старте, и блок на всю запись.

use std::sync::Mutex;
use tokio::sync::Mutex as AsyncMutex;

use super::context::{self, CourseBlock};

static SUBJECT: Mutex<Option<String>> = Mutex::new(None);
/// Блок текущей записи: строится первым из диалогов, второй берёт готовый.
/// Внешний `None` — ещё не строили; внутренний — строили, материалов нет.
static BLOCK: AsyncMutex<Option<Option<CourseBlock>>> = AsyncMutex::const_new(None);

#[tauri::command]
pub async fn course_set_recording_subject(client_id: Option<String>) -> Result<(), String> {
    *SUBJECT.lock().unwrap_or_else(|p| p.into_inner()) = client_id.filter(|id| !id.is_empty());
    *BLOCK.lock().await = None;
    Ok(())
}

pub fn recording_subject() -> Option<String> {
    SUBJECT.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

/// Блок для записи, собранный один раз и общий для Ассистента и подсказок.
/// Замок держится на всю сборку (включая обход папки): второй диалог ждёт и берёт готовое.
pub async fn block_for_recording(pool: &sqlx::SqlitePool) -> Option<CourseBlock> {
    let mut cached = BLOCK.lock().await;
    if let Some(block) = cached.as_ref() {
        return block.clone();
    }
    let block = match recording_subject() {
        None => None,
        Some(client_id) => {
            // Папка — до сборки, но без Claude: картинки дочитает окно материалов.
            if let Err(error) = super::ingest::sync_folder(pool, &client_id).await {
                log::warn!("Материалы: папка предмета не обошлась: {}", error.lines().next().unwrap_or_default());
            }
            context::load(pool, &client_id, None).await.unwrap_or_else(|error| {
                log::warn!("Материалы не собрались: {}", error.lines().next().unwrap_or_default());
                None
            })
        }
    };
    if let Some(block) = &block {
        log::info!(
            "Материалы записи: {} файлов, {} выжимок, ~{} токенов, не вошло {}+{}",
            block.files, block.summaries, block.tokens, block.dropped_files, block.dropped_summaries
        );
    }
    *cached = Some(block.clone());
    block
}

pub async fn clear_recording_block() {
    *BLOCK.lock().await = None;
    *SUBJECT.lock().unwrap_or_else(|p| p.into_inner()) = None;
}

#[tauri::command]
pub async fn course_live_status() -> Result<Option<CourseBlock>, String> {
    // try_lock: пока блок собирается (обход папки может идти минуты), опрос статуса не ждёт.
    let Ok(cached) = BLOCK.try_lock() else { return Ok(None) };
    Ok(cached.clone().flatten().map(|block| CourseBlock { text: String::new(), ..block }))
}

#[cfg(test)]
pub(crate) fn set_subject_for_test(client_id: Option<String>) {
    *SUBJECT.lock().unwrap_or_else(|p| p.into_inner()) = client_id;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_subject_lives_until_cleared() {
        set_subject_for_test(Some("c1".into()));
        assert_eq!(recording_subject().as_deref(), Some("c1"));
        set_subject_for_test(None);
        assert_eq!(recording_subject(), None);
    }
}
