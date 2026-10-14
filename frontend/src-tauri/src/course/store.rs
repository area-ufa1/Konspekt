//! Хранение материалов курса: файлы предмета, их страницы, папка и настройки.
//!
//! Имя файла, путь папки и текст страниц лежат в базе зашифрованными вместе с
//! архивом (`database::fields`); запертый архив отказывает в записи ошибкой,
//! по которой `fields::is_archive_locked` даёт `true`.

use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};

use super::FileKind;
use crate::database::fields::{self, COURSE_FILE_NAME, COURSE_FILE_PATH, COURSE_PAGE_TEXT, CLIENT_COURSE_FOLDER};

/// Откуда файл: добавлен вручную или подхвачен из папки предмета.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Added,
    Folder,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Added => "added",
            Source::Folder => "folder",
        }
    }

    /// Незнакомое значение читается как «добавлен вручную»: такие файлы
    /// синхронизация папки не удаляет.
    fn from_db(value: &str) -> Source {
        match value {
            "folder" => Source::Folder,
            _ => Source::Added,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FileState {
    Reading,
    WaitsClaude,
    Ready,
    Error,
}

impl FileState {
    pub fn as_str(self) -> &'static str {
        match self {
            FileState::Reading => "reading",
            FileState::WaitsClaude => "waits-claude",
            FileState::Ready => "ready",
            FileState::Error => "error",
        }
    }

    /// Незнакомое значение читается как ошибка.
    fn from_db(value: &str) -> FileState {
        match value {
            "reading" => FileState::Reading,
            "waits-claude" => FileState::WaitsClaude,
            "ready" => FileState::Ready,
            _ => FileState::Error,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CourseFile {
    pub id: String,
    pub client_id: String,
    pub name: String,
    pub kind: FileKind,
    pub source: Source,
    pub folder_path: Option<String>,
    pub size: i64,
    pub modified: String,
    pub hash: String,
    pub use_flag: bool,
    pub state: FileState,
    pub error: Option<String>,
    /// Считаются запросом из `course_pages`, не хранятся.
    pub pages: i64,
    pub pages_waiting: i64,
    pub pages_by_claude: i64,
    pub tokens: i64,
    pub added_at: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StoredPage {
    pub number: u32,
    pub text: String,
    pub by_claude: bool,
    pub waits_claude: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CourseSettings {
    pub in_assistant: bool,
    pub in_hints: bool,
    pub in_summary: bool,
}

impl Default for CourseSettings {
    fn default() -> Self {
        CourseSettings { in_assistant: true, in_hints: true, in_summary: true }
    }
}

/// Незнакомый вид файла читается как простой текст: он не трогает PDFium.
fn kind_from_db(value: &str) -> FileKind {
    match value {
        "pdf" => FileKind::Pdf,
        "pptx" => FileKind::Pptx,
        "docx" => FileKind::Docx,
        "md" => FileKind::Md,
        _ => FileKind::Txt,
    }
}

const SELECT_FILES: &str = "SELECT f.id, f.client_id, f.name, f.kind, f.source, f.folder_path, \
     f.size, f.modified, f.hash, f.use_flag, f.state, f.error, f.tokens, f.added_at, \
     (SELECT COUNT(*) FROM course_pages p WHERE p.file_id = f.id) AS pages, \
     (SELECT COALESCE(SUM(p.waits_claude), 0) FROM course_pages p WHERE p.file_id = f.id) AS pages_waiting, \
     (SELECT COALESCE(SUM(p.by_claude), 0) FROM course_pages p WHERE p.file_id = f.id) AS pages_by_claude \
     FROM course_files f";

fn file_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<CourseFile, sqlx::Error> {
    let name: String = row.try_get("name")?;
    let kind: String = row.try_get("kind")?;
    let source: String = row.try_get("source")?;
    let state: String = row.try_get("state")?;
    Ok(CourseFile {
        id: row.try_get("id")?,
        client_id: row.try_get("client_id")?,
        name: fields::open(COURSE_FILE_NAME, &name)?,
        kind: kind_from_db(&kind),
        source: Source::from_db(&source),
        folder_path: fields::open_opt(COURSE_FILE_PATH, row.try_get("folder_path")?)?,
        size: row.try_get("size")?,
        modified: row.try_get("modified")?,
        hash: row.try_get("hash")?,
        use_flag: row.try_get::<i64, _>("use_flag")? != 0,
        state: FileState::from_db(&state),
        error: row.try_get("error")?,
        pages: row.try_get("pages")?,
        pages_waiting: row.try_get("pages_waiting")?,
        pages_by_claude: row.try_get("pages_by_claude")?,
        tokens: row.try_get("tokens")?,
        added_at: row.try_get("added_at")?,
    })
}

fn files_from_rows(rows: &[sqlx::sqlite::SqliteRow]) -> Result<Vec<CourseFile>, sqlx::Error> {
    rows.iter().map(file_from_row).collect()
}

/// Файлы предмета в порядке добавления.
pub async fn list_files(pool: &SqlitePool, client_id: &str) -> Result<Vec<CourseFile>, sqlx::Error> {
    let rows = sqlx::query(&format!("{SELECT_FILES} WHERE f.client_id = ? ORDER BY f.added_at, f.id"))
        .bind(client_id)
        .fetch_all(pool)
        .await?;
    files_from_rows(&rows)
}

/// Путь зашифрован, поэтому ищем по расшифрованным значениям.
pub async fn find_by_folder_path(
    pool: &SqlitePool,
    client_id: &str,
    path: &str,
) -> Result<Option<CourseFile>, sqlx::Error> {
    Ok(list_files(pool, client_id)
        .await?
        .into_iter()
        .find(|file| file.folder_path.as_deref() == Some(path)))
}

pub async fn insert_file(pool: &SqlitePool, file: &CourseFile) -> Result<(), sqlx::Error> {
    // Запечатываем до любого SQL: запертый архив ничего не записывает.
    let name = fields::seal(COURSE_FILE_NAME, &file.name)?;
    let folder_path = fields::seal_opt(COURSE_FILE_PATH, file.folder_path.as_deref())?;
    sqlx::query(
        "INSERT INTO course_files (id, client_id, name, kind, source, folder_path, size, modified, \
         hash, use_flag, state, error, tokens, added_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&file.id)
    .bind(&file.client_id)
    .bind(name)
    .bind(file.kind.as_str())
    .bind(file.source.as_str())
    .bind(folder_path)
    .bind(file.size)
    .bind(&file.modified)
    .bind(&file.hash)
    .bind(file.use_flag as i64)
    .bind(file.state.as_str())
    .bind(&file.error)
    .bind(file.tokens)
    .bind(&file.added_at)
    .execute(pool)
    .await?;
    Ok(())
}

/// Все поля, кроме `id` и `client_id`.
pub async fn update_file(pool: &SqlitePool, file: &CourseFile) -> Result<(), sqlx::Error> {
    let name = fields::seal(COURSE_FILE_NAME, &file.name)?;
    let folder_path = fields::seal_opt(COURSE_FILE_PATH, file.folder_path.as_deref())?;
    sqlx::query(
        "UPDATE course_files SET name = ?, kind = ?, source = ?, folder_path = ?, size = ?, \
         modified = ?, hash = ?, use_flag = ?, state = ?, error = ?, tokens = ?, added_at = ? \
         WHERE id = ?",
    )
    .bind(name)
    .bind(file.kind.as_str())
    .bind(file.source.as_str())
    .bind(folder_path)
    .bind(file.size)
    .bind(&file.modified)
    .bind(&file.hash)
    .bind(file.use_flag as i64)
    .bind(file.state.as_str())
    .bind(&file.error)
    .bind(file.tokens)
    .bind(&file.added_at)
    .bind(&file.id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Файл и его страницы: внешние ключи включены не на всех соединениях.
pub async fn delete_file(pool: &SqlitePool, file_id: &str) -> Result<(), sqlx::Error> {
    let mut transaction = pool.begin().await?;
    sqlx::query("DELETE FROM course_pages WHERE file_id = ?")
        .bind(file_id)
        .execute(&mut *transaction)
        .await?;
    sqlx::query("DELETE FROM course_files WHERE id = ?")
        .bind(file_id)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await
}

/// Заменяет все страницы файла в одной транзакции.
pub async fn replace_pages(
    pool: &SqlitePool,
    file_id: &str,
    pages: &[StoredPage],
) -> Result<(), sqlx::Error> {
    let sealed = pages
        .iter()
        .map(|page| fields::seal(COURSE_PAGE_TEXT, &page.text))
        .collect::<Result<Vec<_>, _>>()?;
    let mut transaction = pool.begin().await?;
    sqlx::query("DELETE FROM course_pages WHERE file_id = ?")
        .bind(file_id)
        .execute(&mut *transaction)
        .await?;
    for (page, text) in pages.iter().zip(sealed) {
        sqlx::query(
            "INSERT INTO course_pages (file_id, number, text, by_claude, waits_claude) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(file_id)
        .bind(page.number as i64)
        .bind(text)
        .bind(page.by_claude as i64)
        .bind(page.waits_claude as i64)
        .execute(&mut *transaction)
        .await?;
    }
    transaction.commit().await
}

/// Текст страницы, прочитанный Claude или поправленный; снимает «ждёт Claude».
pub async fn set_page_text(
    pool: &SqlitePool,
    file_id: &str,
    number: u32,
    text: &str,
    by_claude: bool,
) -> Result<(), sqlx::Error> {
    let sealed = fields::seal(COURSE_PAGE_TEXT, text)?;
    sqlx::query(
        "UPDATE course_pages SET text = ?, by_claude = ?, waits_claude = 0 \
         WHERE file_id = ? AND number = ?",
    )
    .bind(sealed)
    .bind(by_claude as i64)
    .bind(file_id)
    .bind(number as i64)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn pages(pool: &SqlitePool, file_id: &str) -> Result<Vec<StoredPage>, sqlx::Error> {
    let rows: Vec<(i64, String, i64, i64)> = sqlx::query_as(
        "SELECT number, text, by_claude, waits_claude FROM course_pages \
         WHERE file_id = ? ORDER BY number",
    )
    .bind(file_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|(number, text, by_claude, waits_claude)| {
            Ok(StoredPage {
                number: number as u32,
                text: fields::open(COURSE_PAGE_TEXT, &text)?,
                by_claude: by_claude != 0,
                waits_claude: waits_claude != 0,
            })
        })
        .collect()
}

/// Файлы всех предметов, у которых остались страницы для Claude.
pub async fn files_waiting_claude(pool: &SqlitePool) -> Result<Vec<CourseFile>, sqlx::Error> {
    let rows = sqlx::query(&format!(
        "{SELECT_FILES} WHERE f.state = ? ORDER BY f.added_at, f.id"
    ))
    .bind(FileState::WaitsClaude.as_str())
    .fetch_all(pool)
    .await?;
    files_from_rows(&rows)
}

pub async fn get_folder(pool: &SqlitePool, client_id: &str) -> Result<Option<String>, sqlx::Error> {
    let stored: Option<Option<String>> =
        sqlx::query_scalar("SELECT course_folder FROM clients WHERE id = ?")
            .bind(client_id)
            .fetch_optional(pool)
            .await?;
    fields::open_opt(CLIENT_COURSE_FOLDER, stored.flatten())
}

pub async fn set_folder(
    pool: &SqlitePool,
    client_id: &str,
    folder: Option<&str>,
) -> Result<(), sqlx::Error> {
    let sealed = fields::seal_opt(CLIENT_COURSE_FOLDER, folder)?;
    sqlx::query("UPDATE clients SET course_folder = ? WHERE id = ?")
        .bind(sealed)
        .bind(client_id)
        .execute(pool)
        .await?;
    Ok(())
}

const SAVE_SETTINGS_SQL: &str = "INSERT INTO settings (id, provider, model, whisperModel, \
     courseInAssistant, courseInHints, courseInSummary) \
     VALUES ('1', 'claude-cli', 'sonnet', 'large-v3', ?, ?, ?) \
     ON CONFLICT(id) DO UPDATE SET courseInAssistant = excluded.courseInAssistant, \
     courseInHints = excluded.courseInHints, courseInSummary = excluded.courseInSummary";

/// Пустое значение в базе — «включено».
pub async fn get_settings(pool: &SqlitePool) -> Result<CourseSettings, sqlx::Error> {
    let row = sqlx::query_as::<_, (Option<i64>, Option<i64>, Option<i64>)>(
        "SELECT courseInAssistant, courseInHints, courseInSummary FROM settings WHERE id = '1'",
    )
    .fetch_optional(pool)
    .await?;
    Ok(match row {
        Some((assistant, hints, summary)) => CourseSettings {
            in_assistant: assistant.map_or(true, |v| v != 0),
            in_hints: hints.map_or(true, |v| v != 0),
            in_summary: summary.map_or(true, |v| v != 0),
        },
        None => CourseSettings::default(),
    })
}

pub async fn save_settings(pool: &SqlitePool, settings: &CourseSettings) -> Result<(), sqlx::Error> {
    sqlx::query(SAVE_SETTINGS_SQL)
        .bind(settings.in_assistant as i64)
        .bind(settings.in_hints as i64)
        .bind(settings.in_summary as i64)
        .execute(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::fields::CLIENT_DISPLAY_NAME;

    async fn pool_with_client(client_id: &str) -> SqlitePool {
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

    fn sample_file(id: &str, client_id: &str, name: &str) -> CourseFile {
        CourseFile {
            id: id.into(),
            client_id: client_id.into(),
            name: name.into(),
            kind: FileKind::Pdf,
            source: Source::Folder,
            folder_path: Some(format!("D:\\Учёба\\{name}")),
            size: 1234,
            modified: "2026-10-14T09:00:00Z".into(),
            hash: "abc".into(),
            use_flag: true,
            state: FileState::Ready,
            error: None,
            pages: 0,
            pages_waiting: 0,
            pages_by_claude: 0,
            tokens: 10,
            added_at: format!("2026-10-14T10:00:0{}Z", id.len() % 10),
        }
    }

    #[tokio::test]
    async fn files_and_pages_round_trip_sealed() {
        let pool = pool_with_client("c1").await;
        let file = sample_file("f1", "c1", "Лекция 3. Нормализация.pdf");
        insert_file(&pool, &file).await.unwrap();
        replace_pages(
            &pool,
            "f1",
            &[
                StoredPage { number: 1, text: "Вторая нормальная форма".into(), by_claude: false, waits_claude: false },
                StoredPage { number: 2, text: String::new(), by_claude: false, waits_claude: true },
            ],
        )
        .await
        .unwrap();
        let listed = list_files(&pool, "c1").await.unwrap();
        assert_eq!(listed[0].name, "Лекция 3. Нормализация.pdf");
        assert_eq!((listed[0].pages, listed[0].pages_waiting), (2, 1));
        // Что в самой базе зашифровано, проверяет tests/sealed_archive.rs: в юнит-тестах ключа нет.
        set_page_text(&pool, "f1", 2, "$A \\to B$", true).await.unwrap();
        let read = pages(&pool, "f1").await.unwrap();
        assert!(read[1].by_claude && !read[1].waits_claude);
        assert_eq!(read[1].text, "$A \\to B$");
        delete_file(&pool, "f1").await.unwrap();
        assert!(pages(&pool, "f1").await.unwrap().is_empty());
        assert!(list_files(&pool, "c1").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn settings_default_on_and_folder_round_trip() {
        let pool = pool_with_client("c1").await;
        assert_eq!(
            get_settings(&pool).await.unwrap(),
            CourseSettings { in_assistant: true, in_hints: true, in_summary: true }
        );
        save_settings(&pool, &CourseSettings { in_assistant: true, in_hints: false, in_summary: true })
            .await
            .unwrap();
        assert!(!get_settings(&pool).await.unwrap().in_hints);
        set_folder(&pool, "c1", Some("D:\\Учёба\\Базы данных")).await.unwrap();
        assert_eq!(get_folder(&pool, "c1").await.unwrap().as_deref(), Some("D:\\Учёба\\Базы данных"));
        set_folder(&pool, "c1", None).await.unwrap();
        assert_eq!(get_folder(&pool, "c1").await.unwrap(), None);
    }

    #[tokio::test]
    async fn find_update_and_waiting_files() {
        let pool = pool_with_client("c1").await;
        let mut file = sample_file("f1", "c1", "a.pdf");
        insert_file(&pool, &file).await.unwrap();
        let found = find_by_folder_path(&pool, "c1", "D:\\Учёба\\a.pdf").await.unwrap();
        assert_eq!(found.map(|f| f.id), Some("f1".to_string()));
        assert!(find_by_folder_path(&pool, "c1", "D:\\нет").await.unwrap().is_none());
        assert!(files_waiting_claude(&pool).await.unwrap().is_empty());

        file.state = FileState::WaitsClaude;
        file.use_flag = false;
        file.name = "b.pdf".into();
        file.tokens = 77;
        update_file(&pool, &file).await.unwrap();
        let waiting = files_waiting_claude(&pool).await.unwrap();
        assert_eq!(waiting.len(), 1);
        assert_eq!((waiting[0].name.as_str(), waiting[0].tokens, waiting[0].use_flag), ("b.pdf", 77, false));
    }

    #[tokio::test]
    async fn unknown_stored_values_read_safely() {
        let pool = pool_with_client("c1").await;
        insert_file(&pool, &sample_file("f1", "c1", "a.pdf")).await.unwrap();
        sqlx::query("UPDATE course_files SET state = 'weird', kind = 'weird', source = 'weird'")
            .execute(&pool)
            .await
            .unwrap();
        let file = &list_files(&pool, "c1").await.unwrap()[0];
        assert_eq!((file.state, file.kind, file.source), (FileState::Error, FileKind::Txt, Source::Added));
    }

    #[test]
    fn course_settings_sql_has_single_spaces() {
        assert!(!SAVE_SETTINGS_SQL.contains("  "), "{SAVE_SETTINGS_SQL}");
    }
}
