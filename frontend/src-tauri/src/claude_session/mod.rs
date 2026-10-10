//! Постоянный диалог с Claude Code на время записи (Konspekt).

pub mod commands;
pub mod context;
pub mod dialog;
#[cfg(all(test, windows))]
pub(crate) mod fake_claude;
pub mod manager;
pub mod protocol;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AssistantTurn {
    pub id: String,
    pub asked_at_sec: f64,
    pub question: String,
    pub answer: String,
    pub model: String,
    pub created_at: String,
}

pub const SYSTEM_PROMPT: &str = "Ты — ассистент студента на паре или созвоне. Тебе по частям приходит \
расшифровка речи: «Препод/собеседники» — преподаватель и другие участники, «Я» — сам студент. \
Расшифровка автоматическая и может искажать слова и термины — восстанавливай смысл. Отвечай \
по-русски, коротко и по делу, Markdown без заголовков. Опирайся на услышанное и указывай время \
реплик в квадратных скобках, например [12:34]. Если в расшифровке ответа нет — так и скажи одной \
фразой и дай общий ответ с пометкой «(не из пары)».";

pub const WARM_UP: &str = "Запись началась, вопросов пока нет. Ответь одним словом: готов.";
