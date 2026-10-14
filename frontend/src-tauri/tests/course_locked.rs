//! What a locked archive does with course material: refuses the write.
//!
//! Its own binary because the key session is process-wide and set once — here
//! it is installed with a password and no key, the state the idle lock leaves.

use std::sync::Arc;

use app_lib::course::store::{self, CourseFile, FileState, Source, StoredPage};
use app_lib::course::FileKind;
use app_lib::database::fields;
use app_lib::security::session::{self, KeySession};
use sqlx::SqlitePool;

fn lock_the_archive() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let key_session = Arc::new(KeySession::new());
        // A password exists; the key is not loaded.
        key_session.set_configured(true);
        session::install(key_session);
    });
}

async fn archive() -> SqlitePool {
    lock_the_archive();
    let pool = SqlitePool::connect(":memory:").await.unwrap();
    sqlx::raw_sql(
        "CREATE TABLE clients (id TEXT PRIMARY KEY, \
             display_name TEXT NOT NULL CHECK (length(trim(display_name)) > 0), \
             normalized_name TEXT NOT NULL CHECK (length(normalized_name) > 0), \
             notes TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL); \
         CREATE TABLE settings (id TEXT PRIMARY KEY, provider TEXT NOT NULL, \
             model TEXT NOT NULL, whisperModel TEXT NOT NULL); \
         INSERT INTO clients VALUES ('c1', 'plain', 'plain', NULL, 'now', 'now');",
    )
    .execute(&pool)
    .await
    .unwrap();
    // Taken from the migration itself, so this table cannot fall behind.
    sqlx::raw_sql(include_str!("../migrations/20261014000000_add_course_materials.sql"))
        .execute(&pool)
        .await
        .unwrap();
    pool
}

fn file() -> CourseFile {
    CourseFile {
        id: "f1".into(),
        client_id: "c1".into(),
        name: "Лекция 3.pdf".into(),
        kind: FileKind::Pdf,
        source: Source::Added,
        folder_path: None,
        size: 1,
        modified: "now".into(),
        hash: "h".into(),
        use_flag: true,
        state: FileState::Ready,
        error: None,
        pages: 0,
        pages_waiting: 0,
        pages_by_claude: 0,
        tokens: 0,
        added_at: "now".into(),
    }
}

async fn count(pool: &SqlitePool, table: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}")).fetch_one(pool).await.unwrap()
}

#[tokio::test]
async fn locked_archive_refuses_course_writes() {
    let pool = archive().await;

    let error = store::insert_file(&pool, &file()).await.unwrap_err();
    assert!(fields::is_archive_locked(&error), "{error}");
    assert_eq!(count(&pool, "course_files").await, 0);

    // Pages and the folder are refused too, and nothing reaches the tables.
    let page = StoredPage { number: 1, text: "секрет".into(), by_claude: false, waits_claude: false };
    let error = store::replace_pages(&pool, "f1", &[page]).await.unwrap_err();
    assert!(fields::is_archive_locked(&error), "{error}");
    let error = store::set_folder(&pool, "c1", Some("D:/Учёба")).await.unwrap_err();
    assert!(fields::is_archive_locked(&error), "{error}");
    assert_eq!(count(&pool, "course_pages").await, 0);
    let folder: Option<String> = sqlx::query_scalar("SELECT course_folder FROM clients WHERE id = 'c1'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(folder, None);

    // The settings carry no archive text and stay usable.
    store::save_settings(&pool, &store::CourseSettings { in_assistant: false, ..Default::default() })
        .await
        .unwrap();
    assert!(!store::get_settings(&pool).await.unwrap().in_assistant);
}
