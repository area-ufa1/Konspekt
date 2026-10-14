//! Материалы в записи: предмет, выбранный при старте, и блок на всю запись.

use std::future::Future;
use std::sync::Mutex;
use tokio::sync::Mutex as AsyncMutex;

use super::context::{self, CourseBlock};

/// Предмет текущей записи и её поколение: поколение растёт при каждой смене предмета
/// и при сбросе, так что запоздалый сброс прошлой записи не трогает следующую.
struct Recording {
    subject: Option<String>,
    generation: u64,
}

static RECORDING: Mutex<Recording> = Mutex::new(Recording { subject: None, generation: 0 });
/// Блок текущей записи с поколением, для которого собран: строится первым из диалогов,
/// второй берёт готовый. Блок старого поколения считается отсутствующим.
static BLOCK: AsyncMutex<Option<(u64, Option<CourseBlock>)>> = AsyncMutex::const_new(None);

fn recording() -> std::sync::MutexGuard<'static, Recording> {
    RECORDING.lock().unwrap_or_else(|p| p.into_inner())
}

/// Без ожидания `BLOCK`: старт записи не должен ждать чужую сборку блока.
#[tauri::command]
pub async fn course_set_recording_subject(client_id: Option<String>) -> Result<(), String> {
    let mut state = recording();
    state.subject = client_id.filter(|id| !id.is_empty());
    state.generation += 1;
    Ok(())
}

pub fn recording_subject() -> Option<String> {
    recording().subject.clone()
}

/// Поколение записи на сейчас; слушатель конца записи снимает его до `spawn`.
pub fn current_generation() -> u64 {
    recording().generation
}

/// Блок для записи, собранный один раз и общий для Ассистента и подсказок.
/// Замок держится на всю сборку (включая обход папки): второй диалог ждёт и берёт готовое.
pub async fn block_for_recording(pool: &sqlx::SqlitePool) -> Option<CourseBlock> {
    cached_block(|client_id| async move {
        // Папка — до сборки, но без Claude: картинки дочитает окно материалов.
        if let Err(error) = super::ingest::sync_folder(pool, &client_id).await {
            log::warn!("Материалы: папка предмета не обошлась: {}", error.lines().next().unwrap_or_default());
        }
        let block = context::load(pool, &client_id, None).await.unwrap_or_else(|error| {
            log::warn!("Материалы не собрались: {}", error.lines().next().unwrap_or_default());
            None
        });
        if let Some(block) = &block {
            log::info!(
                "Материалы записи: {} файлов, {} выжимок, ~{} токенов, не вошло {}+{}",
                block.files, block.summaries, block.tokens, block.dropped_files, block.dropped_summaries
            );
        }
        block
    })
    .await
}

async fn cached_block<F, Fut>(build: F) -> Option<CourseBlock>
where
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = Option<CourseBlock>>,
{
    let mut cached = BLOCK.lock().await;
    let (subject, generation) = {
        let state = recording();
        (state.subject.clone(), state.generation)
    };
    if let Some((cached_generation, block)) = cached.as_ref() {
        if *cached_generation == generation {
            return block.clone();
        }
    }
    let block = match subject {
        None => None,
        Some(client_id) => build(client_id).await,
    };
    // Запись сменилась или кончилась, пока строили: результат отдаём, но не кэшируем.
    if current_generation() == generation {
        *cached = Some((generation, block.clone()));
    }
    block
}

/// Сброс по концу записи с поколением, снятым при её остановке: если уже началась
/// следующая запись, ничего не трогает.
pub async fn clear_recording_block(generation: u64) {
    {
        let mut state = recording();
        if state.generation != generation {
            return;
        }
        state.subject = None;
        state.generation += 1;
    }
    let mut cached = BLOCK.lock().await;
    if matches!(cached.as_ref(), Some((cached_generation, _)) if *cached_generation == generation) {
        *cached = None;
    }
}

#[tauri::command]
pub async fn course_live_status() -> Result<Option<CourseBlock>, String> {
    // try_lock: пока блок собирается (обход папки может идти минуты), опрос статуса не ждёт.
    let Ok(cached) = BLOCK.try_lock() else { return Ok(None) };
    let generation = current_generation();
    Ok(match cached.as_ref() {
        Some((cached_generation, Some(block))) if *cached_generation == generation => {
            Some(CourseBlock { text: String::new(), ..block.clone() })
        }
        _ => None,
    })
}

#[cfg(test)]
pub(crate) fn set_subject_for_test(client_id: Option<String>) {
    let mut state = recording();
    state.subject = client_id;
    state.generation += 1;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Состояние общее на процесс: тесты этого модуля идут по одному.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn block(subject: &str) -> CourseBlock {
        CourseBlock { subject: subject.into(), text: "t".into(), files: 1, ..CourseBlock::default() }
    }

    #[test]
    fn the_subject_lives_until_cleared() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        set_subject_for_test(Some("c1".into()));
        assert_eq!(recording_subject().as_deref(), Some("c1"));
        set_subject_for_test(None);
        assert_eq!(recording_subject(), None);
    }

    #[tokio::test]
    async fn the_block_is_built_once_and_a_new_subject_invalidates_it() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let builds = AtomicUsize::new(0);
        let build = |client_id: String| {
            builds.fetch_add(1, Ordering::SeqCst);
            async move { Some(block(&client_id)) }
        };
        course_set_recording_subject(Some("c1".into())).await.unwrap();
        assert_eq!(cached_block(&build).await.unwrap().subject, "c1");
        assert_eq!(cached_block(&build).await.unwrap().subject, "c1");
        assert_eq!(builds.load(Ordering::SeqCst), 1);
        course_set_recording_subject(Some("c2".into())).await.unwrap();
        assert_eq!(cached_block(&build).await.unwrap().subject, "c2");
        assert_eq!(builds.load(Ordering::SeqCst), 2);
        clear_recording_block(current_generation()).await;
    }

    #[tokio::test]
    async fn a_stale_clear_does_not_wipe_the_next_recording() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        course_set_recording_subject(Some("c1".into())).await.unwrap();
        let stopped = current_generation();
        // Началась следующая запись, пока сброс прошлой стоял в очереди.
        course_set_recording_subject(Some("c2".into())).await.unwrap();
        clear_recording_block(stopped).await;
        assert_eq!(recording_subject().as_deref(), Some("c2"));
        // Сброс текущей записи работает.
        clear_recording_block(current_generation()).await;
        assert_eq!(recording_subject(), None);
    }

    #[tokio::test]
    async fn a_block_built_during_a_restart_is_not_cached() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        course_set_recording_subject(Some("c1".into())).await.unwrap();
        let got = cached_block(|client_id| async move {
            // Пока строим, началась другая запись.
            course_set_recording_subject(Some("c2".into())).await.unwrap();
            Some(block(&client_id))
        })
        .await;
        assert_eq!(got.unwrap().subject, "c1");
        assert!(course_live_status().await.unwrap().is_none());
        let next = cached_block(|client_id| async move { Some(block(&client_id)) }).await;
        assert_eq!(next.unwrap().subject, "c2");
        clear_recording_block(current_generation()).await;
    }
}
