//! Character classes that must never reach a terminal or a pane border.

/// Unicode format characters (Cf) and the line and paragraph separators:
/// bidi overrides and isolates, zero-width marks, the BOM, interlinear
/// annotation marks and tag characters.
pub fn is_format_char(c: char) -> bool {
    matches!(
        c as u32,
        0xAD | 0x061C
            | 0x180E
            | 0x200B..=0x200F
            | 0x2028..=0x202E
            | 0x2060..=0x206F
            | 0xFEFF
            | 0xFFF9..=0xFFFB
            | 0xE0000..=0xE007F
    )
}

/// Drops every control character (tab and newline included) and every
/// [`is_format_char`], keeping all other text.
pub fn strip_control_and_format(s: &str) -> String {
    s.chars()
        .filter(|&c| !c.is_control() && !is_format_char(c))
        .collect()
}

/// Drops control characters other than tab and newline, and every
/// [`is_format_char`]: the plugin's `HERDR_LINEAR_SANITIZE_JQ_DEF` set.
pub fn strip_control_keep_lines(s: &str) -> String {
    s.chars()
        .filter(|&c| !((c.is_control() && c != '\t' && c != '\n') || is_format_char(c)))
        .collect()
}

/// Every string in `value`, object keys included, through
/// [`strip_control_keep_lines`]. It walks the value rather than naming fields,
/// so a string field added to a type is covered without a line here.
pub fn sanitise_json(value: serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::String(text) => Value::String(strip_control_keep_lines(&text)),
        Value::Array(items) => Value::Array(items.into_iter().map(sanitise_json).collect()),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, item)| (strip_control_keep_lines(&key), sanitise_json(item)))
                .collect(),
        ),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitise_json_walks_nested_strings_and_keys_and_keeps_tab_and_newline() {
        let value = serde_json::json!({
            "k\u{1b}ey": ["a\u{202E}b\tc\nd\u{7}", {"n": "x\u{200B}y", "num": 3, "t": true}]
        });
        assert_eq!(
            sanitise_json(value),
            serde_json::json!({"key": ["ab\tc\nd", {"n": "xy", "num": 3, "t": true}]})
        );
    }

    #[test]
    fn strips_escapes_newlines_and_bidi_but_keeps_brackets() {
        assert_eq!(
            strip_control_and_format("Board [a\u{1b}[31m\u{202E}\n\tb\u{FEFF}]"),
            "Board [a[31mb]"
        );
    }
}
