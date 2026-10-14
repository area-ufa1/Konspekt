//! Есть ли на странице годный текст и сколько он стоит в токенах.

const MIN_ALNUM: usize = 80;
const MAX_GARBAGE_SHARE: f64 = 0.30;
const MIN_ALNUM_SHARE: f64 = 0.30;

fn is_garbage(c: char) -> bool {
    c == '\u{fffd}' || ('\u{e000}'..='\u{f8ff}').contains(&c) || (c.is_control() && !c.is_whitespace())
        // Латиница-1 с диакритикой подряд — типичная кракозябра из UTF-8, прочитанного как cp1252.
        || matches!(c, 'Ð' | 'Ñ' | 'Ã' | 'Â')
}

pub fn needs_reading(text: &str) -> bool {
    let visible: Vec<char> = text.chars().filter(|c| !c.is_whitespace()).collect();
    if visible.is_empty() {
        return true;
    }
    let alnum = visible.iter().filter(|c| c.is_alphanumeric() && !is_garbage(**c)).count();
    let garbage = visible.iter().filter(|c| is_garbage(**c)).count();
    alnum < MIN_ALNUM
        || garbage as f64 / visible.len() as f64 > MAX_GARBAGE_SHARE
        || (alnum as f64) / (visible.len() as f64) < MIN_ALNUM_SHARE
}

pub fn estimate_tokens(text: &str) -> usize {
    let cyrillic = text.chars().filter(|c| ('\u{0400}'..='\u{04ff}').contains(c)).count();
    let other = text.chars().count() - cyrillic;
    cyrillic.div_ceil(3) + other.div_ceil(4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_text_does_not_need_reading() {
        let page = "Вторая нормальная форма: отношение находится в 1НФ, и каждый неключевой атрибут полностью зависит от первичного ключа.";
        assert!(!needs_reading(page));
    }

    #[test]
    fn short_or_empty_page_needs_reading() {
        assert!(needs_reading(""));
        assert!(needs_reading("Рис. 3"));
        assert!(needs_reading("  12  \n\n"));
    }

    #[test]
    fn garbage_text_counts_as_no_text() {
        let mojibake = "ÐÐ¾ÑÐ¼Ð°Ð»ÑÐ½Ð°Ñ ÑÐ¾ÑÐ¼Ð° ".to_string()
            + &"\u{fffd}".repeat(40)
            + " ÐÐ¾ÑÐ¼Ð°";
        assert!(needs_reading(&mojibake));
        let private_use: String = std::iter::repeat('\u{e000}').take(120).collect();
        assert!(needs_reading(&private_use));
        let symbols = "∑ ∫ ∂ → ← ↔ ∀ ∃ ∈ ∉ ⊂ ⊃ ∪ ∩ ≤ ≥ ≠ ≈ ".repeat(8);
        assert!(needs_reading(&symbols), "формулы значками без букв");
    }

    #[test]
    fn tokens_are_cheaper_in_latin() {
        assert_eq!(estimate_tokens("абвгде"), 2);
        assert_eq!(estimate_tokens("abcdefgh"), 2);
        assert_eq!(estimate_tokens("абв abcd"), 3);
        assert_eq!(estimate_tokens(""), 0);
    }
}
