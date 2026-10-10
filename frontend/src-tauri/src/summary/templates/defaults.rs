/// Embedded default templates using compile-time inclusion
///
/// These templates are bundled into the binary and serve as fallbacks
/// when custom templates are not available.

/// Daily standup template for engineering/product teams
pub const DAILY_STANDUP: &str = include_str!("../../../templates/daily_standup.json");

/// Standard meeting notes template
pub const STANDARD_MEETING: &str = include_str!("../../../templates/standard_meeting.json");

/// Lecture notes template (default for Konspekt)
pub const LECTURE: &str = include_str!("../../../templates/lecture.json");

/// Identifier of the template used by default for new summaries
pub const DEFAULT_TEMPLATE_ID: &str = "lecture";

/// Registry of all built-in templates
///
/// Maps template identifiers to their embedded JSON content
pub fn get_builtin_templates() -> Vec<(&'static str, &'static str)> {
    vec![
        ("daily_standup", DAILY_STANDUP),
        ("standard_meeting", STANDARD_MEETING),
        ("lecture", LECTURE),
    ]
}

/// Get a built-in template by identifier
///
/// # Arguments
/// * `id` - Template identifier (e.g., "daily_standup", "standard_meeting")
///
/// # Returns
/// The template JSON content if found, None otherwise
pub fn get_builtin_template(id: &str) -> Option<&'static str> {
    match id {
        "daily_standup" => Some(DAILY_STANDUP),
        "standard_meeting" => Some(STANDARD_MEETING),
        "lecture" => Some(LECTURE),
        _ => None,
    }
}

/// List all built-in template identifiers
pub fn list_builtin_template_ids() -> Vec<&'static str> {
    vec!["daily_standup", "standard_meeting", "lecture"]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_templates_valid_json() {
        for (id, content) in get_builtin_templates() {
            let result = serde_json::from_str::<serde_json::Value>(content);
            assert!(
                result.is_ok(),
                "Built-in template '{}' contains invalid JSON: {:?}",
                id,
                result.err()
            );
        }
    }

    #[test]
    fn test_get_builtin_template() {
        assert!(get_builtin_template("daily_standup").is_some());
        assert!(get_builtin_template("standard_meeting").is_some());
        assert!(get_builtin_template("lecture").is_some());
        assert!(get_builtin_template("nonexistent").is_none());
    }

    #[test]
    fn test_default_template_is_lecture_and_valid() {
        assert_eq!(DEFAULT_TEMPLATE_ID, "lecture");
        let content = get_builtin_template(DEFAULT_TEMPLATE_ID).expect("default template is built in");
        let template: crate::summary::templates::Template =
            serde_json::from_str(content).expect("lecture template parses");
        template.validate().expect("lecture template is valid");
        assert_eq!(template.name, "Лекция");
        let titles: Vec<&str> = template.sections.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(
            titles,
            vec![
                "Кратко",
                "Тезисы по порядку",
                "Термины и определения",
                "Формулы и примеры",
                "Экзамен, ДЗ, сроки"
            ]
        );
    }

    #[test]
    fn test_all_builtin_templates_pass_validation() {
        for (id, content) in get_builtin_templates() {
            let template: crate::summary::templates::Template =
                serde_json::from_str(content).unwrap_or_else(|e| panic!("{id}: {e}"));
            template.validate().unwrap_or_else(|e| panic!("{id}: {e}"));
        }
    }
}
