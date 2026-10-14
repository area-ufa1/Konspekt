//! Материалы в записи: предмет, выбранный при старте, и блок на всю запись.

use std::future::Future;
use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};
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
/// Замок держится на всю сборку: второй диалог ждёт и берёт готовое.
pub async fn block_for_recording(pool: &sqlx::SqlitePool) -> Option<CourseBlock> {
    cached_block(|client_id| async move {
        sync_unless_busy(pool, &client_id).await;
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

/// Обход папки перед записью — под `WORK`, но без ожидания: если окно уже читает
/// (обход, добавление, картинки), запись идёт с тем, что уже прочитано. Два обхода
/// разом завели бы по строке на один новый файл. Без Claude: картинки дочитает окно.
/// `true` — обход был.
async fn sync_unless_busy(pool: &SqlitePool, client_id: &str) -> bool {
    let Ok(_turn) = WORK.try_lock() else {
        log::info!("Материалы: идёт чтение файлов — запись берёт уже прочитанное, папку не обходит");
        return false;
    };
    if let Err(error) = super::ingest::sync_folder(pool, client_id).await {
        log::warn!("Материалы: папка предмета не обошлась: {}", first_line(&error));
    }
    true
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

/// Строка о материалах в Ассистенте: только если Ассистент их берёт (`in_assistant`).
/// Блок мог собраться ради подсказок — тогда Ассистенту хвалиться нечем.
#[tauri::command]
pub async fn course_live_status(state: State<'_, crate::state::AppState>) -> Result<Option<CourseBlock>, String> {
    let settings = store::get_settings(state.db_manager.pool()).await.unwrap_or_default();
    if !settings.in_assistant {
        return Ok(None);
    }
    Ok(live_block())
}

/// Блок текущей записи без текста, если он уже собран.
fn live_block() -> Option<CourseBlock> {
    // try_lock: пока блок собирается, опрос статуса не ждёт.
    let Ok(cached) = BLOCK.try_lock() else { return None };
    let generation = current_generation();
    match cached.as_ref() {
        Some((cached_generation, Some(block))) if *cached_generation == generation => {
            Some(CourseBlock { text: String::new(), ..block.clone() })
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Окно материалов
// ---------------------------------------------------------------------------

const ARCHIVE_LOCKED: &str = "Архив заперт — сначала откройте его";

/// Запертый архив: запись отказывает (`Encode`), чтение не расшифровывает (`Decode`).
///
/// Любой `Decode` здесь — «Архив заперт»: у полей курса расшифровка — единственный
/// источник `Decode`, а отличить запертый архив от испорченного значения при открытом
/// по ошибке sqlx нельзя. Испорченная запись при открытом архиве покажет тот же отказ.
fn store_error(error: sqlx::Error) -> String {
    if crate::database::fields::is_archive_locked(&error) || matches!(error, sqlx::Error::Decode(_)) {
        ARCHIVE_LOCKED.to_string()
    } else {
        format!("Не удалось обратиться к материалам: {error}")
    }
}

/// До постановки в очередь: запертый архив отвечает сразу, а не молча в лог.
fn ensure_writable() -> Result<(), String> {
    crate::database::fields::seal(crate::database::fields::COURSE_FILE_NAME, "").map(|_| ()).map_err(store_error)
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
    // Без потолка: полоса должна краснеть и тогда, когда выжимки в блок не влезли.
    let (summaries, summary_tokens) = context::summary_totals(pool, client_id).await?;
    let material_tokens = files.iter().filter(|file| file.use_flag).map(|file| file.tokens.max(0) as usize).sum();
    Ok(CourseOverview { files, folder, folder_unreachable, summaries, summary_tokens, material_tokens, cap: CAP_TOKENS })
}

/// Одна фоновая работа на приложение: чтение файлов и Claude идут по очереди.
static WORK: AsyncMutex<()> = AsyncMutex::const_new(());
/// Предметы, для которых дочитывание из списка уже стоит в очереди: второе не ставим.
/// По предмету, а не одно на всех: окно предмета B не ждёт конца задачи предмета A.
static WAITING_QUEUED: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// Место предмета в `WAITING_QUEUED`; освобождается в `Drop` — и когда задача
/// упала с паникой (pdfium, разбор картинки), иначе автодочитывание встало бы до перезапуска.
struct WaitingClaim(String);

impl Drop for WaitingClaim {
    fn drop(&mut self) {
        WAITING_QUEUED.lock().unwrap_or_else(|p| p.into_inner()).remove(&self.0);
    }
}

/// `None` — дочитывание этого предмета уже в очереди.
fn claim_waiting(client_id: &str) -> Option<WaitingClaim> {
    let mut queued = WAITING_QUEUED.lock().unwrap_or_else(|p| p.into_inner());
    queued.insert(client_id.to_string()).then(|| WaitingClaim(client_id.to_string()))
}

enum Job {
    Add(Vec<String>),
    Reread(String),
    /// Обход папки предмета, затем картинки.
    Sync,
    /// Только дочитать картинки (при открытии окна).
    Waiting,
    /// Папку отвязали: файлы из неё уходят из материалов. Под `WORK`, чтобы идущее
    /// чтение не дописало страницы или строки уже после удаления.
    Unbind,
}

fn notify<R: Runtime>(app: &AppHandle<R>, client_id: &str) {
    let _ = app.emit("course-changed", Changed { client_id: client_id.to_string() });
}

fn progress<R: Runtime>(app: &AppHandle<R>) -> impl Fn(Progress) + '_ {
    move |progress| {
        // `0 из 0` — строка только что записана как «читается»: окну пора её показать.
        let started = progress.total == 0;
        let client_id = progress.client_id.clone();
        let _ = app.emit("course-progress", progress);
        if started {
            notify(app, &client_id);
        }
    }
}

/// `claim` живёт до конца задачи (и освобождается при панике в ней).
fn queue<R: Runtime>(app: AppHandle<R>, pool: SqlitePool, client_id: String, job: Job, claim: Option<WaitingClaim>) {
    tauri::async_runtime::spawn(async move {
        let _claim = claim;
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
            match ingest::sync_folder_with(pool, client_id, progress(app)).await {
                Ok(report) => log::info!(
                    "Материалы: папка — добавлено {}, изменено {}, убрано {}, без изменений {}",
                    report.added, report.changed, report.removed, report.unchanged
                ),
                Err(error) => log::warn!("Материалы: папка не обошлась: {}", first_line(&error)),
            }
            notify(app, client_id);
        }
        Job::Waiting => {}
        Job::Unbind => {
            match store::delete_folder_files(pool, client_id).await {
                Ok(removed) => log::info!("Материалы: папку отвязали, файлов убрано {removed}"),
                Err(error) => log::warn!("Материалы: файлы папки не убраны: {}", first_line(&error.to_string())),
            }
            notify(app, client_id);
            return;
        }
    }
    let read = read_pictures(app, pool, client_id).await;
    // Из списка (`Waiting`) — только если что-то прочиталось: иначе отказ Claude
    // зациклил бы «изменилось → обновить список → дочитать».
    if explicit || read {
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

/// Что видно владельцу у ждущих файлов: если оно сменилось, окну есть что перерисовать.
fn waiting_signature(files: &[CourseFile]) -> Vec<(String, FileState, Option<String>, i64)> {
    files
        .iter()
        .filter(|file| file.state == FileState::WaitsClaude)
        .map(|file| (file.id.clone(), file.state, file.error.clone(), file.pages_waiting))
        .collect()
}

const ANONYMIZATION_HOLDS_PICTURES: &str = "обезличивание включено — картинки не отправляются";

/// Не слать картинки страниц: обезличивание включено и в словаре есть что скрывать.
/// Имя на скане в картинке не спрятать, а ответ восстанавливать нечего.
fn pictures_held(shield: crate::privacy::Shield) -> bool {
    shield.is_some_and(|session| session.lock().unwrap_or_else(|p| p.into_inner()).has_vocabulary())
}

/// Дочитывает страницы-картинки предмета через Claude; `true`, если что-то изменилось.
/// Отказ Claude (вход, лимит, «Только этот компьютер») останавливает чтение: файлы остаются в ожидании.
async fn read_pictures<R: Runtime>(app: &AppHandle<R>, pool: &SqlitePool, client_id: &str) -> bool {
    if crate::network_policy::local_only() {
        return false;
    }
    let all = match store::list_files(pool, client_id).await {
        Ok(files) => files,
        Err(_) => return false,
    };
    let before = waiting_signature(&all);
    let waiting: Vec<CourseFile> = all.into_iter().filter(|file| file.state == FileState::WaitsClaude).collect();
    if waiting.is_empty() {
        return false;
    }
    let cli_path = SettingsRepository::get_claude_cli_path(pool).await.ok().flatten();
    let model = match SettingsRepository::get_model_config(pool).await {
        Ok(Some(config)) => reading_model(&config.provider, &config.model),
        _ => "sonnet".to_string(),
    };
    let hidden = crate::privacy::vocabulary::shield_for(pool, None).await;
    if pictures_held(hidden.as_ref()) {
        // Картинку не обезличить: пока обезличивание включено, страницы ждут с причиной.
        for file in &waiting {
            if let Err(error) = ingest::hold_waiting(pool, &file.id, ANONYMIZATION_HOLDS_PICTURES).await {
                log::warn!("Материалы: причина ожидания не записалась: {}", first_line(&error));
            }
        }
        let after = store::list_files(pool, client_id).await.map(|files| waiting_signature(&files)).unwrap_or_default();
        return before != after;
    }
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
            // Первая строка ответа CLI может нести вывод модели (текст страницы): только выжимка.
            log::warn!("Материалы: чтение картинок остановлено: {}", crate::claude_cli::error_for_log(&error));
            break;
        }
    }
    // Сигнатура ловит и перемену без прочитанных страниц (оригинал пропал или изменился).
    let after = store::list_files(pool, client_id).await.map(|files| waiting_signature(&files)).unwrap_or_default();
    read > 0 || before != after
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
        if let Some(claim) = claim_waiting(&client_id) {
            queue(app, pool.clone(), client_id, Job::Waiting, Some(claim));
        }
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
    ensure_writable()?;
    if !paths.is_empty() {
        queue(app, state.db_manager.pool().clone(), client_id, Job::Add(paths), None);
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
    let Some(file) = file_by_id(pool, &file_id).await.map_err(store_error)? else {
        return Err("Файл не найден".to_string());
    };
    // Одним запросом и без очереди: окно не ждёт долгую работу и не затирает её результат.
    store::set_use(pool, &file_id, use_flag).await.map_err(store_error)?;
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
    if file.folder_path.is_none() {
        return Err("У файла нет пути на диске — добавьте его снова".to_string());
    }
    ensure_writable()?;
    queue(app, pool.clone(), file.client_id, Job::Reread(file_id), None);
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
    if path.is_none() {
        // Сразу и атомарно: ждать очереди нельзя — запись в это время собрала бы блок
        // из старых строк, а при закрытии приложения они остались бы навсегда.
        // Очередное `Unbind` — уборка после идущего чтения.
        store::delete_folder_files(pool, &client_id).await.map_err(store_error)?;
        notify(&app, &client_id);
    }
    let job = if path.is_some() { Job::Sync } else { Job::Unbind };
    queue(app, pool.clone(), client_id, job, None);
    Ok(())
}

#[tauri::command]
pub async fn course_refresh<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, crate::state::AppState>,
    client_id: String,
) -> Result<(), String> {
    ensure_writable()?;
    queue(app, state.db_manager.pool().clone(), client_id, Job::Sync, None);
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
        assert!(live_block().is_none());
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
    fn a_locked_archive_reads_and_writes_give_the_russian_refusal() {
        let decode = sqlx::Error::Decode("архив заперт".into());
        assert_eq!(store_error(decode), ARCHIVE_LOCKED);
        let encode = sqlx::Error::Encode(Box::new(crate::database::fields::ArchiveLocked { table: "t", column: "c" }));
        assert_eq!(store_error(encode), ARCHIVE_LOCKED);
        assert!(store_error(sqlx::Error::RowNotFound).starts_with("Не удалось"));
    }

    #[test]
    fn only_a_changed_waiting_file_counts_as_a_change() {
        let mut waiting = course_file("a", 10, true);
        waiting.state = FileState::WaitsClaude;
        waiting.pages_waiting = 2;
        let before = waiting_signature(std::slice::from_ref(&waiting));
        assert_eq!(before, waiting_signature(std::slice::from_ref(&waiting)));
        waiting.error = Some("оригинал недоступен".into());
        assert_ne!(before, waiting_signature(&[waiting]));
        assert!(waiting_signature(&[course_file("b", 1, true)]).is_empty());
    }

    #[test]
    fn reading_uses_the_summary_model_only_for_claude_cli() {
        assert_eq!(reading_model("claude-cli", "opus"), "opus");
        assert_eq!(reading_model("claude-cli", " "), "sonnet");
        assert_eq!(reading_model("ollama", "llama3"), "sonnet");
    }

    #[tokio::test]
    async fn a_recording_start_skips_the_folder_while_the_window_is_reading() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let pool = pool_with_client("c1").await;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "Индексы и ключи — основа запросов. ".repeat(5)).unwrap();
        store::set_folder(&pool, "c1", Some(dir.path().to_str().unwrap())).await.unwrap();
        {
            // Окно обходит ту же папку (`WORK` занят): второй обход завёл бы дубль строки.
            let _window = WORK.lock().await;
            assert!(!sync_unless_busy(&pool, "c1").await);
            assert!(store::list_files(&pool, "c1").await.unwrap().is_empty());
        }
        assert!(sync_unless_busy(&pool, "c1").await);
        assert_eq!(store::list_files(&pool, "c1").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn waiting_is_queued_once_per_subject_and_freed_even_on_panic() {
        let a = claim_waiting("guard-a").expect("первое место");
        assert!(claim_waiting("guard-a").is_none(), "второе дочитывание того же предмета не ставим");
        let b = claim_waiting("guard-b").expect("другой предмет не ждёт первого");
        drop(a);
        let again = claim_waiting("guard-a").expect("после конца задачи место свободно");
        drop((again, b));
        // Как в `queue`: место уходит в задачу, задача падает с паникой.
        let claim = claim_waiting("guard-p").unwrap();
        let task = tokio::spawn(async move {
            let _claim = claim;
            panic!("pdfium упал");
        });
        assert!(task.await.is_err());
        assert!(claim_waiting("guard-p").is_some(), "паника не заперла автодочитывание");
    }

    #[test]
    fn pictures_wait_while_anonymization_has_something_to_hide() {
        use crate::privacy::anonymize::{Session, Vocabulary};
        assert!(!pictures_held(None), "обезличивание выключено");
        let empty = Mutex::new(Session::new(Vocabulary::default()));
        assert!(!pictures_held(Some(&empty)), "скрывать нечего");
        let names = Mutex::new(Session::new(Vocabulary { names: vec!["Иванов".into()], terms: Vec::new() }));
        assert!(pictures_held(Some(&names)));
        let terms = Mutex::new(Session::new(Vocabulary { names: Vec::new(), terms: vec!["Простоквашино".into()] }));
        assert!(pictures_held(Some(&terms)));
    }
}
