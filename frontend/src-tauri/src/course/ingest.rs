//! Конвейер файла материала и синхронизация папки предмета.
//!
//! Текст извлекается сразу; страницы-картинки ждут Claude (`waits_claude`) и
//! дочитываются `read_waiting_pages`. PNG ждущих страниц не хранится: оригинал
//! читается с диска заново. В `error` лежит только причина, без имён и путей.

use std::future::Future;
use std::path::{Path, PathBuf};

use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

use super::quality::estimate_tokens;
use super::store::{self, CourseFile, FileState, Source, StoredPage};
use super::{FileKind, MAX_FILE_BYTES};

const ORIGINAL_UNAVAILABLE: &str = "оригинал недоступен — добавьте файл снова";
const ORIGINAL_CHANGED: &str = "оригинал изменился — перечитайте файл";

/// Что прочитать: путь на диске и откуда файл.
pub struct Incoming {
    pub path: PathBuf,
    pub source: Source,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub client_id: String,
    pub file_id: String,
    pub done: u32,
    pub total: u32,
}

#[derive(Debug, Default, PartialEq)]
pub struct SyncReport {
    pub added: usize,
    pub changed: usize,
    pub removed: usize,
    pub unchanged: usize,
    pub unreachable: bool,
}

fn db_error(error: sqlx::Error) -> String {
    error.to_string()
}

fn hash_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}

fn modified_rfc3339(metadata: &std::fs::Metadata) -> String {
    metadata
        .modified()
        .map(|time| {
            chrono::DateTime::<chrono::Utc>::from(time).to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        })
        .unwrap_or_default()
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn tokens_of(pages: &[StoredPage]) -> i64 {
    pages.iter().filter(|page| !page.waits_claude).map(|page| estimate_tokens(&page.text) as i64).sum()
}

/// Прочитать/перечитать файл: текст сразу, картинки — в `waits_claude`. Не зовёт Claude.
pub async fn ingest_file(
    pool: &SqlitePool,
    client_id: &str,
    incoming: &Incoming,
    existing: Option<&CourseFile>,
    mut on_progress: impl FnMut(Progress),
) -> Result<CourseFile, String> {
    let name = incoming.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut file = match existing {
        Some(old) => CourseFile { name: name.clone(), source: incoming.source, ..old.clone() },
        None => CourseFile {
            id: uuid::Uuid::new_v4().to_string(),
            client_id: client_id.to_string(),
            name: name.clone(),
            kind: FileKind::Txt,
            source: incoming.source,
            folder_path: None,
            size: 0,
            modified: String::new(),
            hash: String::new(),
            use_flag: true,
            state: FileState::Reading,
            error: None,
            pages: 0,
            pages_waiting: 0,
            pages_by_claude: 0,
            tokens: 0,
            added_at: chrono::Utc::now().to_rfc3339(),
        },
    };
    file.folder_path = Some(path_string(&incoming.path));
    file.error = None;

    let mut stored: Vec<StoredPage> = Vec::new();
    match read_and_extract(&incoming.path, &name, &mut file).await {
        Ok(pages) => {
            stored = pages
                .into_iter()
                .map(|page| {
                    let waits = page.image_png.is_some();
                    StoredPage {
                        number: page.number,
                        text: if waits { String::new() } else { page.text },
                        by_claude: false,
                        waits_claude: waits,
                    }
                })
                .collect();
            file.tokens = tokens_of(&stored);
            file.state =
                if stored.iter().any(|page| page.waits_claude) { FileState::WaitsClaude } else { FileState::Ready };
        }
        Err(reason) => {
            file.tokens = 0;
            file.state = FileState::Error;
            file.error = Some(reason);
        }
    }

    if existing.is_some() {
        store::update_file(pool, &file).await.map_err(db_error)?;
    } else {
        store::insert_file(pool, &file).await.map_err(db_error)?;
    }
    store::replace_pages(pool, &file.id, &stored).await.map_err(db_error)?;
    let total = stored.len() as u32;
    on_progress(Progress { client_id: client_id.to_string(), file_id: file.id.clone(), done: total, total });
    // Счётчики страниц считает запрос; возвращаем актуальную строку.
    let refreshed = store::list_files(pool, client_id)
        .await
        .map_err(db_error)?
        .into_iter()
        .find(|row| row.id == file.id);
    Ok(refreshed.unwrap_or(file))
}

/// Заполняет `kind`, `size`, `modified`, `hash` и извлекает страницы; `Err` — причина для строки ошибки.
async fn read_and_extract(path: &Path, name: &str, file: &mut CourseFile) -> Result<Vec<super::PageOut>, String> {
    let metadata = tokio::fs::metadata(path).await.map_err(|e| format!("Не удалось прочитать файл: {e}"))?;
    file.size = metadata.len() as i64;
    file.modified = modified_rfc3339(&metadata);
    file.hash = String::new();
    let kind = FileKind::from_name(name)?;
    file.kind = kind;
    if metadata.len() > MAX_FILE_BYTES {
        return Err("Файл больше 50 МБ".to_string());
    }
    let bytes = tokio::fs::read(path).await.map_err(|e| format!("Не удалось прочитать файл: {e}"))?;
    file.hash = hash_hex(&bytes);
    tokio::task::spawn_blocking(move || super::extract(kind, &bytes))
        .await
        .map_err(|_| "Чтение файла прервалось".to_string())?
}

fn is_hidden(name: &str, metadata: &std::fs::Metadata) -> bool {
    if name.starts_with('.') || name.starts_with("~$") {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x2 != 0 {
            return true;
        }
    }
    let _ = metadata;
    false
}

/// Файлы папки (рекурсивно); `None` — корень не читается.
fn walk(root: &Path) -> Option<Vec<(PathBuf, std::fs::Metadata)>> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    let mut first = true;
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) if first => return None,
            Err(_) => continue,
        };
        first = false;
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else { continue };
            if file_type.is_symlink() {
                continue;
            }
            let Ok(metadata) = entry.metadata() else { continue };
            if is_hidden(&entry.file_name().to_string_lossy(), &metadata) {
                continue;
            }
            if file_type.is_dir() {
                stack.push(entry.path());
            } else if file_type.is_file() {
                found.push((entry.path(), metadata));
            }
        }
    }
    Some(found)
}

/// Обойти папку: новые/изменённые — `ingest_file`, исчезнувшие — `delete_file`.
pub async fn sync_folder(pool: &SqlitePool, client_id: &str) -> Result<SyncReport, String> {
    let mut report = SyncReport::default();
    let Some(folder) = store::get_folder(pool, client_id).await.map_err(db_error)? else {
        return Ok(report);
    };
    let root = PathBuf::from(&folder);
    let walked = tokio::task::spawn_blocking(move || if root.is_dir() { walk(&root) } else { None })
        .await
        .map_err(|_| "Обход папки прервался".to_string())?;
    let Some(walked) = walked else {
        report.unreachable = true;
        return Ok(report);
    };

    let known = store::list_files(pool, client_id).await.map_err(db_error)?;
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (path, metadata) in walked {
        let path_text = path_string(&path);
        seen.insert(path_text.clone());
        let incoming = Incoming { path: path.clone(), source: Source::Folder };
        let existing = known.iter().find(|file| file.folder_path.as_deref() == Some(path_text.as_str()));
        let Some(existing) = existing else {
            ingest_file(pool, client_id, &incoming, None, |_| {}).await?;
            report.added += 1;
            continue;
        };
        let modified = modified_rfc3339(&metadata);
        if existing.size == metadata.len() as i64 && existing.modified == modified {
            report.unchanged += 1;
            continue;
        }
        let same_content = match tokio::fs::read(&path).await {
            Ok(bytes) => metadata.len() <= MAX_FILE_BYTES && hash_hex(&bytes) == existing.hash,
            Err(_) => false,
        };
        if same_content {
            let mut touched = existing.clone();
            touched.size = metadata.len() as i64;
            touched.modified = modified;
            store::update_file(pool, &touched).await.map_err(db_error)?;
            report.unchanged += 1;
        } else {
            ingest_file(pool, client_id, &incoming, Some(existing), |_| {}).await?;
            report.changed += 1;
        }
    }
    for file in known.iter().filter(|file| file.source == Source::Folder) {
        let gone = file.folder_path.as_deref().is_none_or(|path| !seen.contains(path));
        if gone {
            store::delete_file(pool, &file.id).await.map_err(db_error)?;
            report.removed += 1;
        }
    }
    Ok(report)
}

/// Дочитать страницы-картинки файла через Claude (по одной, сохраняя после каждой).
pub async fn read_waiting_pages<R, Fut, F>(
    pool: &SqlitePool,
    file_id: &str,
    mut read: R,
    mut on_progress: F,
) -> Result<(), String>
where
    R: FnMut(Vec<u8>) -> Fut,
    Fut: Future<Output = Result<String, String>>,
    F: FnMut(Progress),
{
    let Some(mut file) = find_file(pool, file_id).await? else {
        return Err("Файл не найден".to_string());
    };
    let stored = store::pages(pool, file_id).await.map_err(db_error)?;
    let waiting: Vec<u32> = stored.iter().filter(|page| page.waits_claude).map(|page| page.number).collect();
    if waiting.is_empty() {
        return refresh_file(pool, &mut file, None).await;
    }

    // Оригинал: тот же файл, что читали (по хэшу), иначе номера страниц не сойдутся.
    let original = match file.folder_path.clone() {
        Some(path) => load_original(&path, file.kind, &file.hash).await,
        None => Err(ORIGINAL_UNAVAILABLE),
    };
    let pages = match original {
        Ok(pages) => pages,
        Err(reason) => return refresh_file(pool, &mut file, Some(reason)).await,
    };

    let total = waiting.len() as u32;
    let mut done = 0u32;
    let mut failure: Option<String> = None;
    let mut missing = false;
    for number in waiting {
        let Some(png) = pages.iter().find(|page| page.number == number).and_then(|page| page.image_png.clone())
        else {
            missing = true;
            continue;
        };
        match read(png).await {
            Ok(text) => {
                store::set_page_text(pool, file_id, number, &text, true).await.map_err(db_error)?;
                done += 1;
                on_progress(Progress { client_id: file.client_id.clone(), file_id: file.id.clone(), done, total });
            }
            Err(reason) => {
                failure = Some(reason);
                break;
            }
        }
    }
    refresh_file(pool, &mut file, if missing { Some(ORIGINAL_CHANGED) } else { None }).await?;
    match failure {
        Some(reason) => Err(reason),
        None => Ok(()),
    }
}

async fn find_file(pool: &SqlitePool, file_id: &str) -> Result<Option<CourseFile>, String> {
    let ids: Vec<String> = sqlx::query_scalar("SELECT client_id FROM course_files WHERE id = ?")
        .bind(file_id)
        .fetch_all(pool)
        .await
        .map_err(db_error)?;
    let Some(client_id) = ids.into_iter().next() else { return Ok(None) };
    Ok(store::list_files(pool, &client_id).await.map_err(db_error)?.into_iter().find(|f| f.id == file_id))
}

async fn load_original(path: &str, kind: FileKind, hash: &str) -> Result<Vec<super::PageOut>, &'static str> {
    let bytes = tokio::fs::read(path).await.map_err(|_| ORIGINAL_UNAVAILABLE)?;
    if hash_hex(&bytes) != hash {
        return Err(ORIGINAL_CHANGED);
    }
    tokio::task::spawn_blocking(move || super::extract(kind, &bytes))
        .await
        .map_err(|_| ORIGINAL_UNAVAILABLE)?
        .map_err(|_| ORIGINAL_UNAVAILABLE)
}

/// Пересчитывает `tokens` и `state` по сохранённым страницам; `problem` остаётся в `error` для ждущих.
async fn refresh_file(pool: &SqlitePool, file: &mut CourseFile, problem: Option<&str>) -> Result<(), String> {
    let stored = store::pages(pool, &file.id).await.map_err(db_error)?;
    file.tokens = tokens_of(&stored);
    if stored.iter().any(|page| page.waits_claude) {
        file.state = FileState::WaitsClaude;
        file.error = problem.map(str::to_string);
    } else {
        file.state = FileState::Ready;
        file.error = None;
    }
    store::update_file(pool, file).await.map_err(db_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::fields::{self, CLIENT_DISPLAY_NAME};

    async fn setup(client_id: &str) -> (SqlitePool, tempfile::TempDir) {
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
        (pool, tempfile::tempdir().unwrap())
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn pdf_text_is_ready_and_scan_waits_for_claude() {
        let (pool, dir) = setup("c1").await;
        let path = dir.path().join("Лекция.pdf");
        std::fs::write(&path, crate::course::pdf::tests::pdf_text_then_scan()).unwrap();
        let file = ingest_file(&pool, "c1", &Incoming { path, source: Source::Added }, None, |_| {}).await.unwrap();
        assert_eq!(file.state, FileState::WaitsClaude);
        assert_eq!((file.pages, file.pages_waiting), (2, 1));
        assert!(file.tokens > 0);
        let mut asked = 0;
        read_waiting_pages(
            &pool,
            &file.id,
            |png| {
                asked += 1;
                assert_eq!(&png[1..4], b"PNG");
                async { Ok("$A \\to B$".to_string()) }
            },
            |_| {},
        )
        .await
        .unwrap();
        assert_eq!(asked, 1);
        let after = store::list_files(&pool, "c1").await.unwrap().remove(0);
        assert_eq!((after.state, after.pages_waiting, after.pages_by_claude), (FileState::Ready, 0, 1));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn waiting_pages_stay_when_original_is_gone_or_changed() {
        let (pool, dir) = setup("c1").await;
        let path = dir.path().join("Скан.pdf");
        std::fs::write(&path, crate::course::pdf::tests::pdf_text_then_scan()).unwrap();
        let file =
            ingest_file(&pool, "c1", &Incoming { path: path.clone(), source: Source::Added }, None, |_| {})
                .await
                .unwrap();
        std::fs::write(&path, b"%PDF-1.7 other").unwrap();
        read_waiting_pages(&pool, &file.id, |_| async { panic!("Claude не должен вызываться") }, |_| {})
            .await
            .unwrap();
        let changed = store::list_files(&pool, "c1").await.unwrap().remove(0);
        assert_eq!((changed.state, changed.error.as_deref()), (FileState::WaitsClaude, Some(ORIGINAL_CHANGED)));
        std::fs::remove_file(&path).unwrap();
        read_waiting_pages(&pool, &file.id, |_| async { panic!("Claude не должен вызываться") }, |_| {})
            .await
            .unwrap();
        let after = store::list_files(&pool, "c1").await.unwrap().remove(0);
        assert_eq!((after.state, after.pages_waiting), (FileState::WaitsClaude, 1));
        assert_eq!(after.error.as_deref(), Some(ORIGINAL_UNAVAILABLE));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn a_broken_file_is_an_error_row_not_a_failure() {
        let (pool, dir) = setup("c1").await;
        let path = dir.path().join("битый.pdf");
        std::fs::write(&path, b"%PDF-1.7 broken").unwrap();
        let file = ingest_file(&pool, "c1", &Incoming { path, source: Source::Added }, None, |_| {}).await.unwrap();
        assert_eq!(file.state, FileState::Error);
        let error = file.error.unwrap();
        assert!(error.contains("PDF"));
        assert!(!error.contains("битый"));
    }

    #[tokio::test]
    async fn read_error_does_not_leak_the_file_name() {
        let (pool, dir) = setup("c1").await;
        let path = dir.path().join("секретный.txt");
        let file = ingest_file(&pool, "c1", &Incoming { path, source: Source::Added }, None, |_| {}).await.unwrap();
        assert_eq!(file.state, FileState::Error);
        assert!(!file.error.unwrap().contains("секретный"));
    }

    #[tokio::test]
    async fn folder_sync_reads_only_what_changed() {
        let (pool, dir) = setup("c1").await;
        std::fs::create_dir(dir.path().join("Лабы")).unwrap();
        std::fs::write(dir.path().join("a.txt"), "Индексы и ключи — основа производительности запросов. ".repeat(5)).unwrap();
        std::fs::write(dir.path().join("Лабы").join("b.md"), "# Лаба 1\nСоздание таблиц и ограничений целостности. ".repeat(5)).unwrap();
        store::set_folder(&pool, "c1", Some(dir.path().to_str().unwrap())).await.unwrap();
        assert_eq!(sync_folder(&pool, "c1").await.unwrap(), SyncReport { added: 2, ..Default::default() });
        // Только дата — не перечитывается; содержимое — перечитывается; удалённый — исчезает.
        let a = dir.path().join("a.txt");
        let same = std::fs::read(&a).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(&a, &same).unwrap();
        std::fs::write(dir.path().join("Лабы").join("b.md"), "# Лаба 1\nДругой текст про внешние ключи и каскадное удаление. ".repeat(5)).unwrap();
        std::fs::write(dir.path().join("c.doc"), b"old").unwrap();
        let report = sync_folder(&pool, "c1").await.unwrap();
        assert_eq!((report.added, report.changed, report.unchanged), (1, 1, 1), "c.doc — новый (строка с ошибкой формата)");
        let files = store::list_files(&pool, "c1").await.unwrap();
        let doc = files.iter().find(|f| f.name == "c.doc").unwrap();
        assert_eq!(doc.state, FileState::Error);
        assert!(!doc.error.as_ref().unwrap().contains("c.doc"));
        std::fs::remove_file(&a).unwrap();
        assert_eq!(sync_folder(&pool, "c1").await.unwrap().removed, 1);
        let names: Vec<String> = store::list_files(&pool, "c1").await.unwrap().into_iter().map(|f| f.name).collect();
        assert!(!names.contains(&"a.txt".to_string()));
    }

    #[tokio::test]
    async fn rename_is_old_removed_and_new_added() {
        let (pool, dir) = setup("c1").await;
        std::fs::write(dir.path().join("old.txt"), "Нормальные формы отношений и функциональные зависимости. ".repeat(5)).unwrap();
        store::set_folder(&pool, "c1", Some(dir.path().to_str().unwrap())).await.unwrap();
        sync_folder(&pool, "c1").await.unwrap();
        std::fs::rename(dir.path().join("old.txt"), dir.path().join("new.txt")).unwrap();
        let report = sync_folder(&pool, "c1").await.unwrap();
        assert_eq!((report.added, report.removed, report.changed), (1, 1, 0));
        let names: Vec<String> = store::list_files(&pool, "c1").await.unwrap().into_iter().map(|f| f.name).collect();
        assert_eq!(names, vec!["new.txt".to_string()]);
    }

    #[tokio::test]
    async fn hidden_and_office_temp_files_are_skipped_and_added_files_survive() {
        let (pool, dir) = setup("c1").await;
        std::fs::write(dir.path().join(".скрытый.txt"), "x").unwrap();
        std::fs::write(dir.path().join("~$temp.docx"), "x").unwrap();
        std::fs::write(dir.path().join("real.txt"), "Транзакции и уровни изоляции. ".repeat(5)).unwrap();
        let manual = tempfile::tempdir().unwrap();
        let manual_path = manual.path().join("manual.txt");
        std::fs::write(&manual_path, "Ручной файл про блокировки. ".repeat(5)).unwrap();
        ingest_file(&pool, "c1", &Incoming { path: manual_path, source: Source::Added }, None, |_| {}).await.unwrap();
        store::set_folder(&pool, "c1", Some(dir.path().to_str().unwrap())).await.unwrap();
        let report = sync_folder(&pool, "c1").await.unwrap();
        assert_eq!((report.added, report.removed), (1, 0));
        assert_eq!(store::list_files(&pool, "c1").await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn unreachable_folder_keeps_old_materials() {
        let (pool, dir) = setup("c1").await;
        let path = dir.path().join("keep.txt");
        std::fs::write(&path, "Материал, который не должен пропасть. ".repeat(5)).unwrap();
        ingest_file(&pool, "c1", &Incoming { path, source: Source::Folder }, None, |_| {}).await.unwrap();
        store::set_folder(&pool, "c1", Some("Z:\\нет такой папки")).await.unwrap();
        let report = sync_folder(&pool, "c1").await.unwrap();
        assert!(report.unreachable);
        assert_eq!(store::list_files(&pool, "c1").await.unwrap().len(), 1);
    }
}
