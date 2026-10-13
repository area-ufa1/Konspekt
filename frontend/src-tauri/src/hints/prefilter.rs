//! Префильтр подсказок: дешёвое правило «похоже на вопрос или задание
//! преподавателя». Ошибается в сторону лишних срабатываний — окончательно
//! решает Claude. Знакам препинания не доверяем: расшифровка может идти без них.

const QUESTION_WORDS: &[&str] = &[
    "что", "чем", "чему", "чего", "как", "каков", "какой", "какая", "какое", "какие", "каким", "какую",
    "почему", "зачем", "сколько", "кто", "кого", "кому", "где", "когда", "откуда", "куда",
];
/// Слова, после которых вопросительное слово всё ещё начинает вопрос: «а что…», «ну и как…».
const LEAD_INS: &[&str] = &["а", "и", "ну", "так", "вот", "ребята", "коллеги", "скажите"];
const TASK_WORDS: &[&str] = &[
    "назовите", "скажите", "объясните", "посчитайте", "подсчитайте", "вычислите", "найдите",
    "приведите", "определите", "ответьте", "подумайте", "попробуйте", "напишите", "вспомните",
    "решите", "докажите", "сформулируйте", "перечислите", "предложите",
];
const GROUP_PHRASES: &[&str] = &[
    "кто может", "кто знает", "кто помнит", "кто скажет", "кто готов", "есть идеи", "ваши варианты",
    "как вы думаете", "как думаете", "что скажете",
];
/// Хвост-переспрос: «…, правильно?» без знака вопроса.
const TAG_ENDINGS: &[&str] = &["правильно", "верно", "понятно", "согласны"];
const MIN_WORDS: usize = 3;

pub fn is_candidate(text: &str) -> bool {
    if text.contains('?') {
        return true;
    }
    let lower = text.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric() && c != '-')
        .filter(|word| !word.is_empty())
        .collect();
    if words.len() < MIN_WORDS {
        return false;
    }
    if words.iter().any(|word| *word == "ли") {
        return true;
    }
    if words.iter().any(|word| TASK_WORDS.contains(word)) {
        return true;
    }
    let joined = words.join(" ");
    if GROUP_PHRASES.iter().any(|phrase| joined.contains(phrase)) {
        return true;
    }
    // Вопросительное слово в начале фразы, после вводных «а», «ну и» и т. п.
    let first_real = words.iter().position(|word| !LEAD_INS.contains(word)).unwrap_or(words.len());
    if first_real < words.len() && first_real <= 2 && QUESTION_WORDS.contains(&words[first_real]) {
        return true;
    }
    words.last().is_some_and(|last| TAG_ENDINGS.contains(last))
}

#[cfg(test)]
mod tests {
    use super::is_candidate;

    #[test]
    fn questions_and_tasks_are_candidates() {
        for text in [
            "чем третья нормальная форма отличается от второй",
            "Как вы думаете почему так происходит",
            "кто может привести пример",
            "назовите три свойства транзакции",
            "посчитайте определитель этой матрицы",
            "есть ли у этой функции производная",
            "а сколько здесь степеней свободы",
            "это линейное уравнение правильно",
            "Зачем нужен индекс?",
            "ну и какой из этих вариантов верный",
            "ваши варианты коллеги",
        ] {
            assert!(is_candidate(text), "должно быть кандидатом: {text}");
        }
    }

    #[test]
    fn explanations_and_short_lines_are_not() {
        for text in [
            "итак мы видим то что функция возрастает на всём промежутке",
            "запишем это в виде таблицы",
            "хорошо",
            "да",
            "так",
            "на прошлой лекции мы разобрали нормальные формы",
            "",
        ] {
            assert!(!is_candidate(text), "не должно быть кандидатом: {text}");
        }
    }

    #[test]
    fn a_question_mark_wins_even_on_short_lines() {
        assert!(is_candidate("да?"));
        assert!(is_candidate("Понятно?"));
    }
}
