-- Konspekt: вопросы ассистенту во время записи и ответы на них.
CREATE TABLE assistant_turns (
    id TEXT PRIMARY KEY,
    meeting_id TEXT NOT NULL,
    asked_at_sec REAL NOT NULL,
    question TEXT NOT NULL,
    answer TEXT NOT NULL,
    model TEXT NOT NULL,
    created_at TEXT NOT NULL,
    FOREIGN KEY (meeting_id) REFERENCES meetings(id) ON DELETE CASCADE
);
CREATE INDEX idx_assistant_turns_meeting ON assistant_turns(meeting_id, asked_at_sec);
