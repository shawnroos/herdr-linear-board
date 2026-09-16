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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_escapes_newlines_and_bidi_but_keeps_brackets() {
        assert_eq!(
            strip_control_and_format("Board [a\u{1b}[31m\u{202E}\n\tb\u{FEFF}]"),
            "Board [a[31mb]"
        );
    }
}
