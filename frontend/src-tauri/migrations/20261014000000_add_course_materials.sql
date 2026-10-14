-- Konspekt: материалы курса. Предмет — клиент библиотеки.
-- name, folder_path, course_pages.text, clients.course_folder — зашифрованы (database::fields).
CREATE TABLE course_files (
    id TEXT PRIMARY KEY,
    client_id TEXT NOT NULL REFERENCES clients(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    source TEXT NOT NULL,
    folder_path TEXT,
    size INTEGER NOT NULL,
    modified TEXT NOT NULL,
    hash TEXT NOT NULL,
    use_flag INTEGER NOT NULL DEFAULT 1,
    state TEXT NOT NULL,
    error TEXT,
    tokens INTEGER NOT NULL DEFAULT 0,
    added_at TEXT NOT NULL
);
CREATE INDEX idx_course_files_client ON course_files(client_id, added_at);

CREATE TABLE course_pages (
    file_id TEXT NOT NULL REFERENCES course_files(id) ON DELETE CASCADE,
    number INTEGER NOT NULL,
    text TEXT NOT NULL,
    by_claude INTEGER NOT NULL DEFAULT 0,
    waits_claude INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (file_id, number)
);

ALTER TABLE clients ADD COLUMN course_folder TEXT;

-- NULL — включено (по умолчанию материалы видят все).
ALTER TABLE settings ADD COLUMN courseInAssistant INTEGER;
ALTER TABLE settings ADD COLUMN courseInHints INTEGER;
ALTER TABLE settings ADD COLUMN courseInSummary INTEGER;
