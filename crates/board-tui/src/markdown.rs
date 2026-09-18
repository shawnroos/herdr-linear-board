//! Markdown for the issue page, in the subset a terminal can honestly draw.
//!
//! Linear's descriptions and comments are Markdown, and reading them raw means
//! reading the markup. This renders the structure that survives in a terminal —
//! headings, emphasis, code, lists, checkboxes, quotes and links — and refuses
//! to pretend about the rest: an image, a table and an embed each become one
//! line naming what it is and where it lives, because a terminal that draws a
//! half-table is worse than one that says there is a table.
//!
//! It is written here rather than taken from a crate because the subset is
//! bounded and the output is ratatui spans. A parser would still need the whole
//! span-mapping layer below it, which is the part with the decisions in it.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn code_style() -> Style {
    Style::default().fg(Color::Cyan)
}

/// Render `text` to lines that fit `width`.
pub fn render(text: &str, width: usize) -> Vec<Line<'static>> {
    if width == 0 {
        return vec![];
    }
    let mut out = vec![];
    let mut fence: Option<String> = None;
    for raw in text.split('\n') {
        let trimmed = raw.trim_end();
        // A fence closes on its own marker, so a ``` inside a ~~~ block is
        // content rather than the end of the block.
        if let Some(marker) = fence.clone() {
            if trimmed.trim_start().starts_with(&marker) {
                fence = None;
            } else {
                out.push(Line::from(Span::styled(
                    truncate_to(trimmed, width),
                    code_style(),
                )));
            }
            continue;
        }
        if let Some(marker) = fence_marker(trimmed) {
            fence = Some(marker);
            continue;
        }
        out.extend(block(trimmed, width));
    }
    out
}

/// The opening marker of a fenced block, if this line is one. Kept verbatim so
/// the block closes on its own marker.
fn fence_marker(line: &str) -> Option<String> {
    let t = line.trim_start();
    for marker in ["```", "~~~"] {
        if t.starts_with(marker) {
            return Some(marker.to_string());
        }
    }
    None
}

/// One non-fenced line: its block shape, then its inline spans, wrapped.
fn block(line: &str, width: usize) -> Vec<Line<'static>> {
    let t = line.trim_start();
    if t.is_empty() {
        return vec![Line::from("")];
    }

    // Media this board will not draw. Named rather than rendered: a reader who
    // knows there is an image can open it, and one shown nothing cannot.
    if let Some(placeholder) = media_placeholder(t) {
        return vec![Line::from(Span::styled(
            truncate_to(&placeholder, width),
            dim(),
        ))];
    }

    let indent = line.len() - t.len();
    let pad = " ".repeat(indent.min(width.saturating_sub(1)));

    if let Some(rest) = heading(t) {
        let (level, text) = rest;
        let prefix = if level == 1 { "" } else { "  " };
        return wrap(
            inline(text, Style::default().add_modifier(Modifier::BOLD)),
            width,
            &format!("{pad}{prefix}"),
            &format!("{pad}{prefix}"),
        );
    }

    if let Some(rest) = t.strip_prefix("> ").or_else(|| t.strip_prefix(">")) {
        return wrap(
            inline(rest.trim_start(), dim()),
            width,
            &format!("{pad}│ "),
            &format!("{pad}│ "),
        );
    }

    if let Some((marker, rest)) = list_item(t) {
        let continuation = format!("{pad}{}", " ".repeat(marker.width()));
        return wrap(
            inline(rest, Style::default()),
            width,
            &format!("{pad}{marker}"),
            &continuation,
        );
    }

    wrap(inline(t, Style::default()), width, &pad, &pad)
}

fn heading(t: &str) -> Option<(usize, &str)> {
    let hashes = t.chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = t[hashes..].strip_prefix(' ')?;
    Some((hashes, rest))
}

/// A bullet, a number, or a checkbox, returned as the marker to draw and the
/// text after it. The marker is what the continuation lines indent past, so a
/// wrapped item stays visually inside its bullet.
fn list_item(t: &str) -> Option<(String, &str)> {
    for bullet in ["- ", "* ", "+ "] {
        if let Some(rest) = t.strip_prefix(bullet) {
            if let Some(task) = rest.strip_prefix("[ ] ") {
                return Some(("☐ ".into(), task));
            }
            if let Some(task) = rest
                .strip_prefix("[x] ")
                .or_else(|| rest.strip_prefix("[X] "))
            {
                return Some(("☑ ".into(), task));
            }
            return Some(("• ".into(), rest));
        }
    }
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 {
        if let Some(rest) = t[digits..].strip_prefix(". ") {
            return Some((format!("{}. ", &t[..digits]), rest));
        }
    }
    None
}

/// An image, a table row or an embed as one line naming it and its link.
fn media_placeholder(t: &str) -> Option<String> {
    if let Some(rest) = t.strip_prefix("![") {
        let alt = rest.split(']').next().unwrap_or("");
        let url = rest
            .split_once("](")
            .and_then(|(_, tail)| tail.split(')').next())
            .unwrap_or("");
        let alt = if alt.trim().is_empty() { "image" } else { alt };
        return Some(format!("[{alt}] {url}"));
    }
    // A table renders as a row of pipes or nothing; either way the board cannot
    // lay one out in a column this narrow.
    if t.starts_with('|') && t.ends_with('|') && t.matches('|').count() >= 3 {
        return Some("[table] open in Linear".into());
    }
    if t.starts_with("<iframe") || t.starts_with("<video") || t.starts_with("<embed") {
        return Some("[embed] open in Linear".into());
    }
    None
}

/// Inline emphasis, code and links, as styled spans over `base`.
fn inline(text: &str, base: Style) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = vec![];
    let mut buf = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;

    let flush = |buf: &mut String, out: &mut Vec<Span<'static>>| {
        if !buf.is_empty() {
            out.push(Span::styled(std::mem::take(buf), base));
        }
    };

    while i < chars.len() {
        // `code`
        if chars[i] == '`' {
            if let Some(end) = find(&chars, i + 1, '`') {
                flush(&mut buf, &mut out);
                out.push(Span::styled(
                    chars[i + 1..end].iter().collect::<String>(),
                    code_style(),
                ));
                i = end + 1;
                continue;
            }
        }
        // **bold**
        if chars[i] == '*' && chars.get(i + 1) == Some(&'*') {
            if let Some(end) = find_pair(&chars, i + 2) {
                flush(&mut buf, &mut out);
                out.push(Span::styled(
                    chars[i + 2..end].iter().collect::<String>(),
                    base.add_modifier(Modifier::BOLD),
                ));
                i = end + 2;
                continue;
            }
        }
        // *italic* or _italic_
        if chars[i] == '*' || chars[i] == '_' {
            let marker = chars[i];
            if let Some(end) = find(&chars, i + 1, marker) {
                if end > i + 1 {
                    flush(&mut buf, &mut out);
                    out.push(Span::styled(
                        chars[i + 1..end].iter().collect::<String>(),
                        base.add_modifier(Modifier::ITALIC),
                    ));
                    i = end + 1;
                    continue;
                }
            }
        }
        // [text](url) — the text is what a reader reads; the URL follows dim so
        // it can still be copied off the screen.
        if chars[i] == '[' {
            if let Some(close) = find(&chars, i + 1, ']') {
                if chars.get(close + 1) == Some(&'(') {
                    if let Some(end) = find(&chars, close + 2, ')') {
                        flush(&mut buf, &mut out);
                        out.push(Span::styled(
                            chars[i + 1..close].iter().collect::<String>(),
                            base.add_modifier(Modifier::UNDERLINED),
                        ));
                        out.push(Span::styled(
                            format!(" ({})", chars[close + 2..end].iter().collect::<String>()),
                            dim(),
                        ));
                        i = end + 1;
                        continue;
                    }
                }
            }
        }
        buf.push(chars[i]);
        i += 1;
    }
    flush(&mut buf, &mut out);
    out
}

fn find(chars: &[char], from: usize, needle: char) -> Option<usize> {
    (from..chars.len()).find(|&i| chars[i] == needle)
}

/// The end of a `**` run, as an index of its first `*`.
fn find_pair(chars: &[char], from: usize) -> Option<usize> {
    (from..chars.len().saturating_sub(1))
        .find(|&i| chars[i] == '*' && chars[i + 1] == '*')
        .filter(|&end| end > from)
}

/// Wrap styled spans to `width`, keeping each span's style across the break.
fn wrap(
    spans: Vec<Span<'static>>,
    width: usize,
    first_prefix: &str,
    rest_prefix: &str,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = vec![];
    let mut current: Vec<Span<'static>> = vec![Span::raw(first_prefix.to_string())];
    let mut used = first_prefix.width();

    for span in spans {
        for word in split_words(&span.content) {
            let is_space = word.trim().is_empty();
            let mut word = word;
            loop {
                let w = word.width();
                if used + w > width && used > rest_prefix.width() {
                    lines.push(Line::from(std::mem::take(&mut current)));
                    current.push(Span::raw(rest_prefix.to_string()));
                    used = rest_prefix.width();
                    if is_space {
                        break;
                    }
                }
                let room = width.saturating_sub(used);
                if word.width() <= room {
                    current.push(Span::styled(word.clone(), span.style));
                    used += word.width();
                    break;
                }
                // Wider than a whole line even at the margin: break it rather
                // than let it run past the column.
                let (head, tail) = split_at_width(&word, room);
                if head.is_empty() {
                    break;
                }
                current.push(Span::styled(head, span.style));
                used = width;
                word = tail;
            }
        }
    }
    if current.iter().any(|s| !s.content.trim().is_empty()) || lines.is_empty() {
        lines.push(Line::from(current));
    }
    lines
}

/// Words with their following space attached, so a wrap never loses one.
fn split_words(text: &str) -> Vec<String> {
    let mut out = vec![];
    let mut word = String::new();
    for c in text.chars() {
        if c == ' ' {
            if !word.is_empty() {
                out.push(std::mem::take(&mut word));
            }
            out.push(" ".to_string());
        } else {
            word.push(c);
        }
    }
    if !word.is_empty() {
        out.push(word);
    }
    out
}

/// Split at a display width, never inside a character.
fn split_at_width(s: &str, width: usize) -> (String, String) {
    let mut head = String::new();
    let mut used = 0;
    let mut chars = s.chars().peekable();
    while let Some(&c) = chars.peek() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w > width {
            break;
        }
        head.push(c);
        used += w;
        chars.next();
    }
    (head, chars.collect())
}

fn truncate_to(s: &str, width: usize) -> String {
    if s.width() <= width {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w > width.saturating_sub(1) {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rendered text of each line, which is what a reader sees.
    fn text(lines: &[Line<'static>]) -> Vec<String> {
        lines.iter().map(|l| l.to_string()).collect()
    }

    fn styles(lines: &[Line<'static>]) -> Vec<Vec<(String, Style)>> {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| (s.content.to_string(), s.style))
                    .collect()
            })
            .collect()
    }

    fn has_modifier(lines: &[Line<'static>], needle: &str, m: Modifier) -> bool {
        styles(lines)
            .iter()
            .flatten()
            .any(|(content, style)| content.contains(needle) && style.add_modifier.contains(m))
    }

    #[test]
    fn a_heading_is_bold_and_keeps_its_text() {
        let out = render("# What happens", 40);
        assert_eq!(text(&out), vec!["What happens"]);
        assert!(has_modifier(&out, "What", Modifier::BOLD));
    }

    #[test]
    fn a_deeper_heading_is_indented_rather_than_marked_up() {
        let out = render("### Detail", 40);
        assert_eq!(text(&out), vec!["  Detail"]);
        assert!(has_modifier(&out, "Detail", Modifier::BOLD));
    }

    #[test]
    fn seven_hashes_is_not_a_heading() {
        let out = render("####### not a heading", 40);
        assert_eq!(text(&out), vec!["####### not a heading"]);
    }

    #[test]
    fn bold_italic_and_code_are_styled_and_their_markers_removed() {
        let out = render("a **bold** and *thin* and `code` word", 60);
        assert_eq!(text(&out), vec!["a bold and thin and code word"]);
        assert!(has_modifier(&out, "bold", Modifier::BOLD));
        assert!(has_modifier(&out, "thin", Modifier::ITALIC));
        let styled = styles(&out);
        assert!(
            styled
                .iter()
                .flatten()
                .any(|(c, s)| c == "code" && s.fg == Some(Color::Cyan)),
            "{styled:?}"
        );
    }

    #[test]
    fn an_unclosed_marker_is_left_as_text() {
        assert_eq!(text(&render("a *dangling star", 40)), vec!["a *dangling star"]);
        assert_eq!(text(&render("an `unclosed tick", 40)), vec!["an `unclosed tick"]);
    }

    #[test]
    fn a_link_shows_its_text_and_its_url() {
        let out = render("see [the docs](https://example.com/x)", 60);
        assert_eq!(text(&out), vec!["see the docs (https://example.com/x)"]);
        assert!(has_modifier(&out, "docs", Modifier::UNDERLINED));
    }

    #[test]
    fn bullets_numbers_and_checkboxes_get_their_markers() {
        let out = render("- one\n* two\n1. three\n- [ ] todo\n- [x] done", 40);
        assert_eq!(
            text(&out),
            vec!["• one", "• two", "1. three", "☐ todo", "☑ done"]
        );
    }

    #[test]
    fn a_quote_is_marked_and_dimmed() {
        let out = render("> quoted", 40);
        assert_eq!(text(&out), vec!["│ quoted"]);
        let styled = styles(&out);
        assert!(
            styled
                .iter()
                .flatten()
                .any(|(c, s)| c.contains("quoted") && s.fg == Some(Color::DarkGray)),
            "{styled:?}"
        );
    }

    #[test]
    fn a_fenced_block_renders_unwrapped_and_unstyled_inside() {
        let out = render("```rust\nlet **x** = 1;\n```\nafter", 40);
        assert_eq!(text(&out), vec!["let **x** = 1;", "after"]);
        let styled = styles(&out);
        assert!(
            styled[0]
                .iter()
                .all(|(_, s)| s.fg == Some(Color::Cyan) && !s.add_modifier.contains(Modifier::BOLD)),
            "markup inside a fence was rendered: {styled:?}"
        );
    }

    /// An unterminated fence must not swallow the rest of the document — but it
    /// also must not pretend the rest is prose, because it is not.
    #[test]
    fn an_unterminated_fence_ends_at_the_document() {
        let out = render("```\ncode one\ncode two", 40);
        assert_eq!(text(&out), vec!["code one", "code two"]);
    }

    #[test]
    fn a_tilde_fence_is_not_closed_by_a_backtick_fence() {
        let out = render("~~~\n```\nstill code\n~~~\nprose", 40);
        assert_eq!(text(&out), vec!["```", "still code", "prose"]);
    }

    #[test]
    fn an_image_a_table_and_an_embed_become_one_line_each() {
        let out = render("![a shot](https://x/y.png)", 60);
        assert_eq!(text(&out), vec!["[a shot] https://x/y.png"]);

        let out = render("| a | b |", 60);
        assert_eq!(text(&out), vec!["[table] open in Linear"]);

        let out = render("<iframe src=\"x\"></iframe>", 60);
        assert_eq!(text(&out), vec!["[embed] open in Linear"]);
    }

    #[test]
    fn an_image_with_no_alt_text_still_says_it_is_an_image() {
        let out = render("![](https://x/y.png)", 60);
        assert_eq!(text(&out), vec!["[image] https://x/y.png"]);
    }

    #[test]
    fn text_wraps_at_the_column() {
        let out = render("one two three four five six", 12);
        for line in text(&out) {
            assert!(line.width() <= 12, "{line:?} is wider than 12");
        }
        assert!(out.len() > 1);
    }

    /// A wrapped list item stays inside its own bullet rather than starting a
    /// new one at the margin.
    #[test]
    fn a_wrapped_list_item_indents_past_its_marker() {
        let out = render("- alpha beta gamma delta", 12);
        let lines = text(&out);
        assert!(lines[0].starts_with("• "), "{lines:?}");
        for line in &lines[1..] {
            assert!(line.starts_with("  "), "{lines:?}");
            assert!(!line.starts_with("• "), "{lines:?}");
        }
    }

    #[test]
    fn a_wrap_keeps_the_style_of_the_span_it_breaks() {
        let out = render("**alpha beta gamma delta**", 12);
        assert!(out.len() > 1);
        for line in &out {
            assert!(
                line.spans
                    .iter()
                    .filter(|s| !s.content.trim().is_empty())
                    .all(|s| s.style.add_modifier.contains(Modifier::BOLD)),
                "a wrap dropped the bold: {:?}",
                styles(&out)
            );
        }
    }

    #[test]
    fn a_wide_character_counts_by_display_width() {
        let out = render("一二三四五六七八", 8);
        for line in text(&out) {
            assert!(line.width() <= 8, "{line:?} is wider than 8");
        }
        assert!(out.len() > 1, "wide characters did not wrap");
    }

    #[test]
    fn plain_text_is_unchanged() {
        let out = render("nothing special here", 40);
        assert_eq!(text(&out), vec!["nothing special here"]);
    }

    #[test]
    fn a_blank_line_survives_as_a_paragraph_break() {
        let out = render("one\n\ntwo", 40);
        assert_eq!(text(&out), vec!["one", "", "two"]);
    }

    #[test]
    fn a_zero_width_column_renders_nothing_rather_than_looping() {
        assert!(render("anything at all", 0).is_empty());
    }
}
