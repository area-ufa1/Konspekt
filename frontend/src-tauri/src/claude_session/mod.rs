//! Постоянный диалог с Claude Code на время записи (Konspekt).

pub mod commands;
pub mod context;
pub mod dialog;
#[cfg(all(test, windows))]
pub(crate) mod fake_claude;
pub mod manager;
pub mod protocol;

use serde::{Deserialize, Serialize};

/// Что за ход: вопрос владельца во вкладке «Ассистент» или подсказка на вопрос
/// преподавателя (найденная детектором или по горячей клавише).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum TurnKind {
    #[default]
    Question,
    Hint,
    HintHotkey,
}

impl TurnKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TurnKind::Question => "question",
            TurnKind::Hint => "hint",
            TurnKind::HintHotkey => "hint-hotkey",
        }
    }

    pub fn from_db(text: &str) -> TurnKind {
        match text {
            "hint" => TurnKind::Hint,
            "hint-hotkey" => TurnKind::HintHotkey,
            _ => TurnKind::Question,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AssistantTurn {
    pub id: String,
    pub asked_at_sec: f64,
    pub question: String,
    pub answer: String,
    pub model: String,
    pub created_at: String,
    /// Нет в журналах и экспорте до подпроекта 2 — значит, вопрос.
    #[serde(default)]
    pub kind: TurnKind,
}

pub const SYSTEM_PROMPT: &str = "Ты — ассистент студента на паре или созвоне. Тебе по частям приходит \
расшифровка речи: «Препод/собеседники» — преподаватель и другие участники, «Я» — сам студент. \
Расшифровка автоматическая и может искажать слова и термины — восстанавливай смысл. Отвечай \
по-русски, не длиннее 2–3 предложений: студент читает ответ посреди пары. Сразу суть — без \
вступлений, пересказа вопроса и рассуждений о том, чего препод не сказал. Опирайся на \
услышанное; если в расшифровке ответа нет, отвечай из общих знаний без оговорок. Вопросы вроде \
«че там», «о чём сейчас» — про блок «Сейчас», а не про всё с прошлого вопроса. «Что ответить», \
«ответь» — готовая фраза от первого лица на последний вопрос препода, которую можно сказать вслух. Время реплик \
[12:34] указывай, только если спросили, когда это было. Списки — только если о них просят. \
Просьбы о длине и манере ответа действуют до конца записи.";

pub const WARM_UP: &str = "Запись началась, вопросов пока нет. Ответь одним словом: готов.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_lines_without_kind_are_questions() {
        let old = r#"{"id":"t","askedAtSec":1.0,"question":"q","answer":"a","model":"haiku","createdAt":"x"}"#;
        let turn: AssistantTurn = serde_json::from_str(old).unwrap();
        assert_eq!(turn.kind, TurnKind::Question);
        let hint = AssistantTurn { kind: TurnKind::HintHotkey, ..turn };
        assert!(serde_json::to_string(&hint).unwrap().contains(r#""kind":"hint-hotkey""#));
    }
}
