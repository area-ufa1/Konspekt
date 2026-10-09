//! Строки `claude -p --input-format stream-json --output-format stream-json
//! --include-partial-messages`: что пишем в stdin и что читаем из stdout.

use serde_json::Value;

/// Что значит одна строка stdout для текущего вопроса.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Кусок текста ответа (частичное сообщение).
    Delta(String),
    /// Ответ закончен, полный текст.
    Done(String),
    /// Ход закончился ошибкой (лимит, вход, внутренняя ошибка) — её текст.
    Failed(String),
    /// Служебная строка (init, сообщение целиком, мусор) — пропускаем.
    Other,
}

pub fn parse_line(line: &str) -> Event {
    let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
        return Event::Other;
    };
    match value["type"].as_str() {
        Some("stream_event") => {
            let delta = &value["event"]["delta"];
            if value["event"]["type"] == "content_block_delta" && delta["type"] == "text_delta" {
                if let Some(text) = delta["text"].as_str() {
                    return Event::Delta(text.to_string());
                }
            }
            Event::Other
        }
        Some("result") => {
            let text = value["result"].as_str().unwrap_or_default().to_string();
            if value["is_error"].as_bool().unwrap_or(false) || value["subtype"] != "success" {
                let reason = if text.is_empty() {
                    value["subtype"].as_str().unwrap_or("error").to_string()
                } else {
                    text
                };
                Event::Failed(reason)
            } else {
                Event::Done(text)
            }
        }
        _ => Event::Other,
    }
}

pub fn user_message(text: &str) -> String {
    let message = serde_json::json!({
        "type": "user",
        "message": { "role": "user", "content": text },
    });
    format!("{message}\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_delta_is_a_delta() {
        let line = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"При"}}}"#;
        assert_eq!(parse_line(line), Event::Delta("При".into()));
    }

    #[test]
    fn other_deltas_and_service_lines_are_ignored() {
        let thinking = r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"hm"}}}"#;
        assert_eq!(parse_line(thinking), Event::Other);
        assert_eq!(parse_line(r#"{"type":"system","subtype":"init","model":"claude-haiku-5-5"}"#), Event::Other);
        assert_eq!(parse_line(r#"{"type":"assistant","message":{"content":[]}}"#), Event::Other);
        assert_eq!(parse_line("not json at all"), Event::Other);
        assert_eq!(parse_line(""), Event::Other);
    }

    #[test]
    fn successful_result_is_done() {
        let line = r#"{"type":"result","subtype":"success","is_error":false,"result":"Привет, мир","duration_ms":900}"#;
        assert_eq!(parse_line(line), Event::Done("Привет, мир".into()));
    }

    #[test]
    fn error_result_is_failed_with_its_text() {
        let limit = r#"{"type":"result","subtype":"success","is_error":true,"result":"Claude AI usage limit reached"}"#;
        assert_eq!(parse_line(limit), Event::Failed("Claude AI usage limit reached".into()));
        let crashed = r#"{"type":"result","subtype":"error_during_execution","is_error":true}"#;
        assert_eq!(parse_line(crashed), Event::Failed("error_during_execution".into()));
    }

    #[test]
    fn a_line_cut_mid_json_is_ignored() {
        // Обрыв посреди строки (процесс упал, буфер не дописан) — не событие и не паника.
        let cut_delta = r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"При"#;
        assert_eq!(parse_line(cut_delta), Event::Other);
        let cut_result = r#"{"type":"result","subtype":"success","is_er"#;
        assert_eq!(parse_line(cut_result), Event::Other);
    }

    #[test]
    fn user_message_is_one_line_and_roundtrips() {
        let text = "Вопрос: что такое \"p-значение\"?\nВторая строка\t\\ конец";
        let line = user_message(text);
        assert!(line.ends_with('\n'));
        assert_eq!(line.matches('\n').count(), 1, "ровно один перевод строки — в конце");
        let value: Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(value["type"], "user");
        assert_eq!(value["message"]["role"], "user");
        assert_eq!(value["message"]["content"], text);
    }
}
