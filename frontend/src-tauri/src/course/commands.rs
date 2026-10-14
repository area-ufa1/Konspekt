//! Материалы в записи: предмет, выбранный при старте, и блок на всю запись.

use std::future::Future;
use std::sync::Mutex;
use serde::Serialize;
use sqlx::SqlitePool;
use tauri::{AppHandle, Emitter, Runtime, State};
use tokio::sync::Mutex as AsyncMutex;

use super::context::{self, CourseBlock, CAP_TOKENS};
use super::ingest::{self, Incoming, Progress};
use super::store::{self, CourseFile, CourseSettings, FileState, Source, StoredPage};
use crate::database::repositories::setting::SettingsRepository;

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

// ---------------------------------------------------------------------------
// Окно материалов
// ---------------------------------------------------------------------------

const ARCHIVE_LOCKED: &str = "Архив заперт — сначала откройте его";

fn store_error(error: sqlx::Error) -> String {
    if crate::database::fields::is_archive_locked(&error) {
        ARCHIVE_LOCKED.to_string()
    } else {
        format!("Не удалось обратиться к материалам: {error}")
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CourseOverview {
    pub files: Vec<CourseFile>,
    pub folder: Option<String>,
    pub folder_unreachable: bool,
    pub summaries: usize,
    pub summary_tokens: usize,
    pub material_tokens: usize,
    pub cap: usize,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Changed {
    client_id: String,
}

/// Токены раздела прошлых пар внутри собранного блока.
fn lectures_tokens(text: &str) -> usize {
    const OPEN: &str = "<past_lectures>";
    const CLOSE: &str = "</past_lectures>";
    let Some(start) = text.find(OPEN) else { return 0 };
    let section = &text[start..];
    let end = section.find(CLOSE).map_or(section.len(), |at| at + CLOSE.len());
    super::quality::estimate_tokens(&section[..end])
}

/// Модель чтения картинок: модель выжимки, если провайдер — claude-cli, иначе `sonnet`.
fn reading_model(provider: &str, model: &str) -> String {
    if provider == "claude-cli" && !model.trim().is_empty() {
        model.trim().to_string()
    } else {
        "sonnet".to_string()
    }
}

/// Данные окна без Tauri: файлы, папка, числа для полосы.
pub async fn overview(pool: &SqlitePool, client_id: &str) -> Result<CourseOverview, String> {
    let files = store::list_files(pool, client_id).await.map_err(store_error)?;
    let folder = store::get_folder(pool, client_id).await.map_err(store_error)?;
    let folder_unreachable = match &folder {
        Some(path) => !tokio::fs::metadata(path).await.map(|m| m.is_dir()).unwrap_or(false),
        None => false,
    };
    let (summaries, summary_tokens) = match context::load(pool, client_id, None).await? {
        Some(block) => (block.summaries, lectures_tokens(&block.text)),
        None => (0, 0),
    };
    let material_tokens = files.iter().filter(|file| file.use_flag).map(|file| file.tokens.max(0) as usize).sum();
    Ok(CourseOverview { files, folder, folder_unreachable, summaries, summary_tokens, material_tokens, cap: CAP_TOKENS })
}

/// Одна фоновая работа на приложение: чтение файлов и Claude идут по очереди.
static WORK: AsyncMutex<()> = AsyncMutex::const_new(());

enum Job {
    Add(Vec<String>),
    Reread(String),
    /// Обход папки предмета, затем картинки.
    Sync,
    /// Только дочитать картинки (при открытии окна).
    Waiting,
}

fn notify<R: Runtime>(app: &AppHandle<R>, client_id: &str) {
    let _ = app.emit("course-changed", Changed { client_id: client_id.to_string() });
}

fn progress<R: Runtime>(app: &AppHandle<R>) -> impl Fn(Progress) + '_ {
    move |progress| {
        let _ = app.emit("course-progress", progress);
    }
}

fn queue<R: Runtime>(app: AppHandle<R>, pool: SqlitePool, client_id: String, job: Job) {
    tauri::async_runtime::spawn(async move {
        let _turn = WORK.lock().await;
        run(&app, &pool, &client_id, job).await;
    });
}

fn first_line(error: &str) -> &str {
    error.lines().next().unwrap_or_default()
}

async fn run<R: Runtime>(app: &AppHandle<R>, pool: &SqlitePool, client_id: &str, job: Job) {
    let explicit = !matches!(job, Job::Waiting);
    match job {
        Job::Add(paths) => {
            for path in paths {
                let existing = store::find_by_folder_path(pool, client_id, &path).await.ok().flatten();
                let incoming = Incoming { path: path.into(), source: Source::Added };
                if let Err(error) = ingest::ingest_file(pool, client_id, &incoming, existing.as_ref(), progress(app)).await {
                    log::warn!("Материалы: файл не добавлен: {}", first_line(&error));
                }
                notify(app, client_id);
            }
        }
        Job::Reread(file_id) => match file_by_id(pool, &file_id).await {
            Ok(Some(file)) => {
                if let Some(path) = file.folder_path.clone() {
                    let incoming = Incoming { path: path.into(), source: file.source };
                    if let Err(error) = ingest::ingest_file(pool, client_id, &incoming, Some(&file), progress(app)).await {
                        log::warn!("Материалы: файл не перечитан: {}", first_line(&error));
                    }
                }
                notify(app, client_id);
            }
            _ => log::warn!("Материалы: файл для перечитывания не найден"),
        },
        Job::Sync => {
            match ingest::sync_folder(pool, client_id).await {
                Ok(report) => log::info!(
                    "Материалы: папка — добавлено {}, изменено {}, убрано {}, без изменений {}",
                    report.added, report.changed, report.removed, report.unchanged
                ),
                Err(error) => log::warn!("Материалы: папка не обошлась: {}", first_line(&error)),
            }
            notify(app, client_id);
        }
        Job::Waiting => {}
    }
    let read = read_pictures(app, pool, client_id).await;
    // Из списка (`Waiting`) — только если что-то прочиталось: иначе отказ Claude
    // зациклил бы «изменилось → обновить список → дочитать».
    if explicit || read > 0 {
        notify(app, client_id);
    }
}

async fn file_by_id(pool: &SqlitePool, file_id: &str) -> Result<Option<CourseFile>, sqlx::Error> {
    let owner: Option<String> = sqlx::query_scalar("SELECT client_id FROM course_files WHERE id = ?")
        .bind(file_id)
        .fetch_optional(pool)
        .await?;
    let Some(client_id) = owner else { return Ok(None) };
    Ok(store::list_files(pool, &client_id).await?.into_iter().find(|file| file.id == file_id))
}

/// Дочитывает страницы-картинки предмета через Claude; число прочитанных страниц.
/// Отказ Claude (вход, лимит, «Только этот компьютер») останавливает чтение: файлы остаются в ожидании.
async fn read_pictures<R: Runtime>(app: &AppHandle<R>, pool: &SqlitePool, client_id: &str) -> usize {
    if crate::network_policy::local_only() {
        return 0;
    }
    let waiting: Vec<CourseFile> = match store::list_files(pool, client_id).await {
        Ok(files) => files.into_iter().filter(|file| file.state == FileState::WaitsClaude).collect(),
        Err(_) => return 0,
    };
    if waiting.is_empty() {
        return 0;
    }
    let cli_path = SettingsRepository::get_claude_cli_path(pool).await.ok().flatten();
    let model = match SettingsRepository::get_model_config(pool).await {
        Ok(Some(config)) => reading_model(&config.provider, &config.model),
        _ => "sonnet".to_string(),
    };
    let hidden = crate::privacy::vocabulary::shield_for(pool, None).await;
    let (cli, model, shield) = (cli_path.as_deref(), model.as_str(), hidden.as_ref());
    let mut read = 0usize;
    for file in waiting {
        let result = ingest::read_waiting_pages(
            pool,
            &file.id,
            |png| async move { super::reader::read_page(cli, model, &png, shield).await },
            |step| {
                read += 1;
                let _ = app.emit("course-progress", step);
            },
        )
        .await;
        if let Err(error) = result {
            log::warn!("Материалы: чтение картинок остановлено: {}", first_line(&error));
            break;
        }
    }
    read
}

#[tauri::command]
pub async fn course_list<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, crate::state::AppState>,
    client_id: String,
) -> Result<CourseOverview, String> {
    let pool = state.db_manager.pool();
    let view = overview(pool, &client_id).await?;
    if !crate::network_policy::local_only() && view.files.iter().any(|file| file.state == FileState::WaitsClaude) {
        queue(app, pool.clone(), client_id, Job::Waiting);
    }
    Ok(view)
}

#[tauri::command]
pub async fn course_add_files<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, crate::state::AppState>,
    client_id: String,
    paths: Vec<String>,
) -> Result<(), String> {
    if !paths.is_empty() {
        queue(app, state.db_manager.pool().clone(), client_id, Job::Add(paths));
    }
    Ok(())
}

#[tauri::command]
pub async fn course_set_use<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, crate::state::AppState>,
    file_id: String,
    use_flag: bool,
) -> Result<(), String> {
    let pool = state.db_manager.pool();
    let Some(mut file) = file_by_id(pool, &file_id).await.map_err(store_error)? else {
        return Err("Файл не найден".to_string());
    };
    file.use_flag = use_flag;
    store::update_file(pool, &file).await.map_err(store_error)?;
    notify(&app, &file.client_id);
    Ok(())
}

#[tauri::command]
pub async fn course_remove<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, crate::state::AppState>,
    file_id: String,
) -> Result<(), String> {
    let pool = state.db_manager.pool();
    let file = file_by_id(pool, &file_id).await.map_err(store_error)?;
    store::delete_file(pool, &file_id).await.map_err(store_error)?;
    if let Some(file) = file {
        notify(&app, &file.client_id);
    }
    Ok(())
}

#[tauri::command]
pub async fn course_reread<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, crate::state::AppState>,
    file_id: String,
) -> Result<(), String> {
    let pool = state.db_manager.pool();
    let Some(file) = file_by_id(pool, &file_id).await.map_err(store_error)? else {
        return Err("Файл не найден".to_string());
    };
    queue(app, pool.clone(), file.client_id, Job::Reread(file_id));
    Ok(())
}

#[tauri::command]
pub async fn course_pages(state: State<'_, crate::state::AppState>, file_id: String) -> Result<Vec<StoredPage>, String> {
    store::pages(state.db_manager.pool(), &file_id).await.map_err(store_error)
}

#[tauri::command]
pub async fn course_set_folder<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, crate::state::AppState>,
    client_id: String,
    path: Option<String>,
) -> Result<(), String> {
    let pool = state.db_manager.pool();
    let path = path.map(|p| p.trim().to_string()).filter(|p| !p.is_empty());
    store::set_folder(pool, &client_id, path.as_deref()).await.map_err(store_error)?;
    notify(&app, &client_id);
    if path.is_some() {
        queue(app, pool.clone(), client_id, Job::Sync);
    }
    Ok(())
}

#[tauri::command]
pub async fn course_refresh<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, crate::state::AppState>,
    client_id: String,
) -> Result<(), String> {
    queue(app, state.db_manager.pool().clone(), client_id, Job::Sync);
    Ok(())
}

#[tauri::command]
pub async fn course_get_settings(state: State<'_, crate::state::AppState>) -> Result<CourseSettings, String> {
    store::get_settings(state.db_manager.pool()).await.map_err(store_error)
}

#[tauri::command]
pub async fn course_save_settings(
    state: State<'_, crate::state::AppState>,
    settings: CourseSettings,
) -> Result<(), String> {
    store::save_settings(state.db_manager.pool(), &settings).await.map_err(store_error)
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

    async fn pool_with_client(client_id: &str) -> SqlitePool {
        use crate::database::fields::{self, CLIENT_DISPLAY_NAME};
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        crate::database::manager::MIGRATOR.run(&pool).await.unwrap();
        let name = fields::seal(CLIENT_DISPLAY_NAME, "Базы данных").unwrap();
        sqlx::query(
            "INSERT INTO clients (id, display_name, normalized_name, notes, created_at, updated_at) \
             VALUES (?, ?, 'базы данных', NULL, '2026-10-14T10:00:00Z', '2026-10-14T10:00:00Z')",
        )
        .bind(client_id)
        .bind(name)
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    fn course_file(id: &str, tokens: i64, use_flag: bool) -> CourseFile {
        CourseFile {
            id: id.into(),
            client_id: "c1".into(),
            name: format!("{id}.txt"),
            kind: crate::course::FileKind::Txt,
            source: Source::Added,
            folder_path: None,
            size: 1,
            modified: String::new(),
            hash: "h".into(),
            use_flag,
            state: FileState::Ready,
            error: None,
            pages: 1,
            pages_waiting: 0,
            pages_by_claude: 0,
            tokens,
            added_at: format!("2026-10-14T10:00:0{}Z", if id == "a" { 1 } else { 2 }),
        }
    }

    #[tokio::test]
    async fn overview_counts_only_used_files() {
        let pool = pool_with_client("c1").await;
        store::insert_file(&pool, &course_file("a", 1000, true)).await.unwrap();
        store::insert_file(&pool, &course_file("b", 500, false)).await.unwrap();
        let view = overview(&pool, "c1").await.unwrap();
        assert_eq!(view.files.len(), 2);
        assert_eq!(view.material_tokens, 1000);
        assert_eq!(view.cap, 80_000);
        assert_eq!((view.summaries, view.summary_tokens), (0, 0));
        assert_eq!(view.folder, None);
        assert!(!view.folder_unreachable);
    }

    #[tokio::test]
    async fn overview_flags_a_missing_folder() {
        let pool = pool_with_client("c1").await;
        let gone = std::env::temp_dir().join("konspekt-no-such-folder-9f3a");
        store::set_folder(&pool, "c1", Some(gone.to_str().unwrap())).await.unwrap();
        let view = overview(&pool, "c1").await.unwrap();
        assert!(view.folder.is_some());
        assert!(view.folder_unreachable);
    }

    #[test]
    fn lectures_tokens_counts_only_the_lectures_section() {
        assert_eq!(lectures_tokens("<course_materials>x</course_materials>"), 0);
        let text = "<course_materials>слово слово</course_materials>\n\n<past_lectures>\n<lecture date=\"d\">итог</lecture>\n</past_lectures>\n\nинструкция";
        let n = lectures_tokens(text);
        assert!(n > 0);
        assert!(n < super::super::quality::estimate_tokens(text));
    }

    #[test]
    fn reading_uses_the_summary_model_only_for_claude_cli() {
        assert_eq!(reading_model("claude-cli", "opus"), "opus");
        assert_eq!(reading_model("claude-cli", " "), "sonnet");
        assert_eq!(reading_model("ollama", "llama3"), "sonnet");
    }
}
