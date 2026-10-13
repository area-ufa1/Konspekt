//! Ответ диалога подсказок, разобранный по тексту на данный момент: ещё не
//! ясно / «НЕТ» (не вопрос) / подсказка. Вызывается на каждую дельту стрима.

pub const QUESTION: &str = "ВОПРОС:";
pub const SHORT: &str = "КОРОТКО:";
pub const DETAILS: &str = "ПОДРОБНО:";
pub const NO: &str = "НЕТ";

#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    Undecided,
    No,
    Hint(Hint),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Hint {
    pub question: String,
    pub short: String,
    pub details: String,
}

/// Разбор ответа по тексту на данный момент. `No` посреди стрима
/// предварительный: «НЕТ» может вырасти в «Нет, это не так…», поэтому решает
/// только итоговый полный текст; вызывающий действует по дельтам лишь для `Hint`.
pub fn parse(text: &str) -> Reply {
    // Модель иногда оборачивает маркеры в **…**: убираем звёздочки целиком.
    let clean = text.replace("**", "");
    let trimmed = clean.trim();
    if trimmed.is_empty() {
        return Reply::Undecided;
    }
    let upper = trimmed.to_uppercase();
    let bare = upper.trim_end_matches(|c: char| c == '.' || c == '!' || c.is_whitespace());
    if bare == NO {
        return Reply::No;
    }
    // Начало, из которого ещё может вырасти «НЕТ» или «ВОПРОС:».
    if NO.starts_with(bare) || QUESTION.starts_with(upper.as_str()) {
        return Reply::Undecided;
    }
    let markers = [QUESTION, SHORT, DETAILS];
    if !markers.iter().any(|m| upper.contains(m)) {
        return Reply::Hint(Hint { short: trimmed.to_string(), ..Hint::default() });
    }
    // Недописанный маркер в последней строке не должен попасть в раздел.
    let mut body = trimmed;
    if let Some(nl) = trimmed.rfind('\n') {
        let last = upper[nl + 1..].trim();
        if !last.is_empty() && markers.iter().any(|m| m.starts_with(last)) && markers.iter().any(|m| upper[..nl].contains(m)) {
            body = trimmed[..nl].trim_end();
        }
    }
    let upper = body.to_uppercase();
    let question_at = find(&upper, QUESTION);
    let short_at = find(&upper, SHORT);
    let details_at = find(&upper, DETAILS);
    let question = question_at
        .map(|at| section(body, at + QUESTION.len(), short_at.or(details_at).unwrap_or(body.len())))
        .unwrap_or_default();
    let short = short_at
        .map(|at| section(body, at + SHORT.len(), details_at.filter(|d| *d > at).unwrap_or(body.len())))
        .unwrap_or_default();
    let details = details_at.map(|at| section(body, at + DETAILS.len(), body.len())).unwrap_or_default();
    Reply::Hint(Hint { question, short, details })
}

/// Байтовая позиция маркера в тексте. `to_uppercase` у кириллицы и ASCII не
/// меняет длину в байтах, поэтому позиция в `upper` годится и для исходника.
fn find(upper: &str, marker: &str) -> Option<usize> {
    upper.find(marker)
}

fn section(text: &str, from: usize, to: usize) -> String {
    text.get(from..to.max(from)).unwrap_or("").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hint(question: &str, short: &str, details: &str) -> Reply {
        Reply::Hint(Hint { question: question.into(), short: short.into(), details: details.into() })
    }

    #[test]
    fn full_reply_is_split_by_markers() {
        let text = "ВОПРОС: Чем 3НФ отличается от 2НФ?\nКОРОТКО: Нет транзитивных зависимостей.\nПОДРОБНО:\n- ключ → A → B запрещено\n- пример";
        assert_eq!(
            parse(text),
            hint("Чем 3НФ отличается от 2НФ?", "Нет транзитивных зависимостей.", "- ключ → A → B запрещено\n- пример")
        );
    }

    #[test]
    fn no_is_only_the_whole_answer() {
        assert_eq!(parse("НЕТ"), Reply::No);
        assert_eq!(parse("  нет.\n"), Reply::No);
    }

    #[test]
    fn beginnings_are_undecided() {
        for text in ["", "  ", "Н", "НЕ", "ВОП", "ВОПРОС"] {
            assert_eq!(parse(text), Reply::Undecided, "{text:?}");
        }
    }

    #[test]
    fn partial_reply_shows_what_has_arrived() {
        assert_eq!(parse("ВОПРОС: Что такое индекс?\nКОРОТКО: Структура для"), hint("Что такое индекс?", "Структура для", ""));
        assert_eq!(parse("ВОПРОС: Что такое ин"), hint("Что такое ин", "", ""));
    }

    #[test]
    fn prose_starting_with_net_is_a_hint() {
        assert_eq!(parse("Нет, это не так: функция убывает."), hint("", "Нет, это не так: функция убывает.", ""));
    }

    #[test]
    fn unmarked_reply_is_shown_whole() {
        assert_eq!(parse("Сумма углов треугольника — 180°."), hint("", "Сумма углов треугольника — 180°.", ""));
    }

    #[test]
    fn markers_are_case_insensitive_and_tolerate_markdown() {
        assert_eq!(parse("**Вопрос:** Как?\n**Коротко:** Так.\n**Подробно:**\nВот."), hint("Как?", "Так.", "Вот."));
    }

    #[test]
    fn markers_without_question_are_still_split() {
        assert_eq!(parse("КОРОТКО: За последние минуты вопросов не было."), hint("", "За последние минуты вопросов не было.", ""));
        assert_eq!(parse("КОРОТКО: Так.\nПОДРОБНО:\nВот."), hint("", "Так.", "Вот."));
    }

    #[test]
    fn unfinished_marker_does_not_leak() {
        assert_eq!(parse("ВОПРОС: Что такое индекс?\nКОРО"), hint("Что такое индекс?", "", ""));
        assert_eq!(parse("ВОПРОС: Что?\nКОРОТКО: Так.\nПОДРО"), hint("Что?", "Так.", ""));
    }
}
