-- Konspekt: Claude Code (подписка) как провайдер.
-- claudeCliPath: явный путь к claude.exe; NULL — искать автоматически.
-- claudeLiveModel: модель живой панели для провайдера claude-cli; NULL — haiku.
ALTER TABLE settings ADD COLUMN claudeCliPath TEXT;
ALTER TABLE settings ADD COLUMN claudeLiveModel TEXT;
