-- Konspekt: подсказки на вопросы преподавателя.
-- claudeHintsEnabled: 1 — подсказки включены; NULL/0 — выключены.
-- claudeHintsMic: 1 — слушать и микрофон (очные пары); NULL/0 — только звонок.
-- claudeHintsHotkey: сочетание «ответить на последний вопрос»; NULL — Ctrl+Shift+Space.
ALTER TABLE settings ADD COLUMN claudeHintsEnabled INTEGER;
ALTER TABLE settings ADD COLUMN claudeHintsMic INTEGER;
ALTER TABLE settings ADD COLUMN claudeHintsHotkey TEXT;
