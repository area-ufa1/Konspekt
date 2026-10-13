-- Konspekt: в assistant_turns лежат и вопросы владельца, и подсказки на вопросы
-- преподавателя. kind: question | hint | hint-hotkey.
ALTER TABLE assistant_turns ADD COLUMN kind TEXT NOT NULL DEFAULT 'question';
