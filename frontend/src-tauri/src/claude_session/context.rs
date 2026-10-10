//! Текст одного сообщения в диалог: новые строки транскрипта с прошлого раза,
//! изменившиеся заметки владельца и сам вопрос. Остальная лекция уже в диалоге.

use std::collections::HashMap;

use crate::audio::recording_saver::TranscriptSegment;
use crate::meeting_notes::Note;

pub const RESTART_TAIL_SECONDS: f64 = 900.0;

#[derive(Debug, Default, Clone)]
pub struct Cursor {
    pub last_sequence: Option<u64>,
    pub notes_sent: HashMap<String, String>,
}

pub fn clock(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    let (hours, minutes, secs) = (total / 3600, (total % 3600) / 60, total % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{secs:02}")
    } else {
        format!("{minutes}:{secs:02}")
    }
}

pub fn label(speaker: Option<&str>) -> Option<&'static str> {
    match speaker.map(str::trim) {
        None | Some("") => None,
        Some("You") => Some("Я"),
        Some(_) => Some("Препод/собеседники"),
    }
}

fn render_line(line: &TranscriptSegment) -> String {
    match label(line.speaker.as_deref()) {
        Some(who) => format!("[{}] {}: {}", clock(line.audio_start_time), who, line.text.trim()),
        None => format!("[{}] {}", clock(line.audio_start_time), line.text.trim()),
    }
}

fn notes_block(cursor: &mut Cursor, notes: &[Note], all: bool) -> String {
    let mut changed = Vec::new();
    for note in notes {
        let text = note.text.trim();
        if text.is_empty() {
            continue;
        }
        if all || cursor.notes_sent.get(&note.id).map(String::as_str) != Some(text) {
            let at = note.at.map(clock).unwrap_or_default();
            changed.push(if at.is_empty() { format!("- {text}") } else { format!("- [{at}] {text}") });
        }
        cursor.notes_sent.insert(note.id.clone(), text.to_string());
    }
    if changed.is_empty() {
        String::new()
    } else {
        format!("\n\nЗаметки владельца (новые или изменённые):\n{}", changed.join("\n"))
    }
}

fn compose(heading: &str, rendered: Vec<String>, notes: String, question: &str, asked_at: f64) -> String {
    let transcript = if rendered.is_empty() {
        "Новых реплик с прошлого вопроса нет (или транскрипт ещё пуст).".to_string()
    } else {
        rendered.join("\n")
    };
    format!(
        "{heading}\n{transcript}{notes}\n\nВопрос владельца (на {}): {}",
        clock(asked_at),
        question.trim()
    )
}

pub fn next_message(cursor: &mut Cursor, lines: &[TranscriptSegment], notes: &[Note], question: &str, asked_at: f64) -> String {
    let fresh: Vec<&TranscriptSegment> = lines
        .iter()
        .filter(|line| cursor.last_sequence.map_or(true, |last| line.sequence_id > last))
        .filter(|line| !line.text.trim().is_empty())
        .collect();
    if let Some(last) = fresh.iter().map(|line| line.sequence_id).max() {
        cursor.last_sequence = Some(last);
    }
    let rendered = fresh.into_iter().map(render_line).collect();
    let notes = notes_block(cursor, notes, false);
    compose("Новое в транскрипте с прошлого вопроса:", rendered, notes, question, asked_at)
}

pub fn restart_message(cursor: &mut Cursor, lines: &[TranscriptSegment], notes: &[Note], question: &str, asked_at: f64) -> String {
    let from = asked_at - RESTART_TAIL_SECONDS;
    let rendered = lines
        .iter()
        .filter(|line| line.audio_start_time >= from && !line.text.trim().is_empty())
        .map(render_line)
        .collect();
    cursor.last_sequence = lines.iter().map(|line| line.sequence_id).max();
    cursor.notes_sent.clear();
    let notes = notes_block(cursor, notes, true);
    compose(
        "Диалог перезапущен после сбоя. Последние 15 минут транскрипта:",
        rendered,
        notes,
        question,
        asked_at,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(seq: u64, start: f64, speaker: Option<&str>, text: &str) -> TranscriptSegment {
        TranscriptSegment {
            id: format!("l{seq}"),
            text: text.into(),
            audio_start_time: start,
            audio_end_time: start + 2.0,
            duration: 2.0,
            display_time: String::new(),
            confidence: 1.0,
            sequence_id: seq,
            speaker: speaker.map(Into::into),
        }
    }

    fn note(id: &str, text: &str) -> Note {
        Note { id: id.into(), at: Some(1.0), text: text.into(), written_at: "2026-10-10T09:00:00Z".into() }
    }

    #[test]
    fn clock_formats_like_the_transcript() {
        assert_eq!(clock(5.4), "0:05");
        assert_eq!(clock(754.0), "12:34");
        assert_eq!(clock(3723.0), "1:02:03");
        assert_eq!(clock(-3.0), "0:00");
    }

    #[test]
    fn speakers_become_me_or_the_lecturer() {
        assert_eq!(label(Some("You")), Some("Я"));
        assert_eq!(label(Some("Speaker 1")), Some("Препод/собеседники"));
        assert_eq!(label(Some("Guest")), Some("Препод/собеседники"));
        assert_eq!(label(Some("Анна")), Some("Препод/собеседники"));
        assert_eq!(label(None), None);
        assert_eq!(label(Some("  ")), None);
    }

    #[test]
    fn only_new_lines_and_changed_notes_go_out() {
        let mut cursor = Cursor::default();
        let lines = vec![line(1, 5.0, Some("Speaker 1"), "Начнём с гипотез"), line(2, 9.0, Some("You"), "Понял")];
        let first = next_message(&mut cursor, &lines, &[note("n1", "гипотеза H0")], "О чём речь?", 10.0);
        assert!(first.contains("[0:05] Препод/собеседники: Начнём с гипотез"));
        assert!(first.contains("[0:09] Я: Понял"));
        assert!(first.contains("гипотеза H0"));
        assert!(first.contains("О чём речь?"));
        assert_eq!(cursor.last_sequence, Some(2));

        let more = vec![lines[0].clone(), lines[1].clone(), line(3, 30.0, None, "без спикера")];
        let second = next_message(&mut cursor, &more, &[note("n1", "гипотеза H0")], "А дальше?", 31.0);
        assert!(!second.contains("Начнём с гипотез"), "старые строки не повторяются");
        assert!(second.contains("[0:30] без спикера"));
        assert!(!second.contains("гипотеза H0"), "неизменённая заметка не повторяется");

        let third = next_message(&mut cursor, &more, &[note("n1", "гипотеза H1")], "Ещё?", 40.0);
        assert!(third.contains("гипотеза H1"), "изменённая заметка уходит снова");
    }

    #[test]
    fn empty_delta_says_nothing_heard_yet() {
        let mut cursor = Cursor::default();
        let message = next_message(&mut cursor, &[], &[], "Что сейчас происходит?", 3.0);
        assert!(message.contains("Новых реплик с прошлого вопроса нет"));
        assert!(message.contains("Что сейчас происходит?"));
    }

    #[test]
    fn restart_sends_only_the_last_fifteen_minutes() {
        let mut cursor = Cursor::default();
        let lines = vec![
            line(1, 100.0, Some("Speaker 1"), "очень давно"),
            line(2, 2000.0, Some("Speaker 1"), "недавно"),
        ];
        let message = restart_message(&mut cursor, &lines, &[note("n1", "заметка")], "Вопрос", 2100.0);
        assert!(!message.contains("очень давно"));
        assert!(message.contains("недавно"));
        assert!(message.contains("заметка"));
        assert!(message.contains("перезапущен"));
        assert_eq!(cursor.last_sequence, Some(2));
    }
}
