//! Generated names for the tabs and splits of a Linear-bound space.

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scope {
    pub issue: Option<String>,
    pub title: Option<String>,
    pub agent: Option<String>,
    pub folder: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Tab,
    Split,
}

pub const MAX_CELLS: usize = 40;

const FALLBACK: &str = "shell";

fn clean(part: Option<&str>) -> Option<String> {
    let text = board_core::text::strip_control_and_format(part?);
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn joined(head: String, tail: Option<String>) -> String {
    match tail {
        Some(tail) => format!("{head} · {tail}"),
        None => head,
    }
}

/// A tab names its issue's title, a split names its agent, and a pane with
/// no bound issue names where it runs. Never empty.
pub fn derive(target: Target, scope: &Scope) -> String {
    let agent = clean(scope.agent.as_deref());
    let name = match clean(scope.issue.as_deref()) {
        Some(issue) => match target {
            Target::Tab => joined(issue, clean(scope.title.as_deref())),
            Target::Split => joined(issue, agent),
        },
        None => match clean(scope.folder.as_deref()) {
            Some(folder) => joined(folder, agent),
            None => agent.unwrap_or_else(|| FALLBACK.to_string()),
        },
    };
    capped(name)
}

fn capped(name: String) -> String {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    if name.width() <= MAX_CELLS {
        return name;
    }
    let mut out = String::new();
    let mut cells = 0;
    for c in name.chars() {
        let w = c.width().unwrap_or(0);
        if cells + w > MAX_CELLS - 1 {
            break;
        }
        cells += w;
        out.push(c);
    }
    let mut out = out.trim_end().to_string();
    out.push('…');
    out
}

/// herdr does not record who set a label, so only a blank label, herdr's
/// default, or the label the board itself last wrote may be replaced.
pub fn decide(
    current: Option<&str>,
    default: Option<&str>,
    stored: Option<&str>,
    derived: &str,
) -> bool {
    let Some(current) = current.map(str::trim).filter(|c| !c.is_empty()) else {
        return true;
    };
    if current == derived {
        return false;
    }
    Some(current) == default || Some(current) == stored
}
