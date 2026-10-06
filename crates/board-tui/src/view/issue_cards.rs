//! The issue view's attachment and pull request cards. Linear's attachments
//! say which pull requests belong to the issue and draw at once; `gh` fields
//! fill in when the daemon's PR read lands.

use board_core::protocol::{LinearAttachment, LinearPrDetail};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::linear::icon::{
    BRANCH, CHECK, CLOCK, COMMIT, CROSS, DIFF, EYE, IMAGE, LINK, MERGE, PERSON, PULL_REQUEST, SLACK,
};
use super::linear::{clip_spans, fit, line, spans_width};
use super::linear_issue::{ago, plain_lines, section, Column, Row, RowKind};
use crate::app::{App, LinearState, PrRead};
use crate::theme::Theme;

const INSET: usize = 2;

/// `owner/repo` and the number of a pull request URL on any host. Only the
/// card reads this; the daemon checks the host again before `gh` sees it.
pub fn pr_shape(url: &str) -> Option<(String, u64)> {
    let rest = url.strip_prefix("https://")?;
    let (_, path) = rest.split_once('/')?;
    let path = path.split(['?', '#']).next()?.trim_end_matches('/');
    let [owner, repo, "pull", number] = path.split('/').collect::<Vec<_>>()[..] else {
        return None;
    };
    if owner.is_empty() || repo.is_empty() || !number.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((format!("{owner}/{repo}"), number.parse().ok()?))
}

pub fn pull_requests(attachments: &[LinearAttachment]) -> Vec<&LinearAttachment> {
    let mut prs: Vec<&LinearAttachment> = attachments
        .iter()
        .filter(|a| pr_shape(&a.url).is_some())
        .collect();
    prs.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    let mut seen = std::collections::BTreeSet::new();
    prs.retain(|a| seen.insert(a.url.clone()));
    prs
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrState {
    Open,
    Draft,
    Merged,
    Closed,
}

fn meta<'a>(a: &'a LinearAttachment, key: &str) -> Option<&'a str> {
    a.metadata
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.trim().is_empty())
}

/// `gh`'s state wins; Linear's status stands in until `gh` has read it.
fn pr_state(a: &LinearAttachment, gh: Option<&LinearPrDetail>) -> PrState {
    let draft = gh
        .and_then(|g| g.draft)
        .or_else(|| a.metadata.get("draft").and_then(serde_json::Value::as_bool))
        .unwrap_or(false);
    let state = gh
        .and_then(|g| g.state.clone())
        .or_else(|| meta(a, "status").map(str::to_string))
        .unwrap_or_default()
        .to_ascii_lowercase();
    if state.contains("merged") {
        PrState::Merged
    } else if state.contains("closed") {
        PrState::Closed
    } else if draft || state.contains("draft") {
        PrState::Draft
    } else {
        PrState::Open
    }
}

fn gh_detail(read: Option<&PrRead>) -> Option<&LinearPrDetail> {
    match read {
        Some(PrRead::Read(doc)) => doc.pr.as_ref(),
        _ => None,
    }
}

fn card_row(spans: Vec<Span<'static>>, width: usize, bg: Color) -> Line<'static> {
    let mut all = vec![Span::raw(" ".repeat(INSET))];
    all.extend(clip_spans(spans, width.saturating_sub(2 * INSET)));
    let fill = width.saturating_sub(spans_width(&all));
    all.push(Span::raw(" ".repeat(fill)));
    Line::from(all).style(Style::default().bg(bg))
}

fn blank(width: usize, bg: Color) -> Line<'static> {
    card_row(vec![], width, bg)
}

fn head(left: Vec<Span<'static>>, chip: Span<'static>, inner: usize) -> Vec<Span<'static>> {
    let gap = inner.saturating_sub(spans_width(&left) + chip.content.width());
    let mut spans = left;
    spans.push(Span::raw(" ".repeat(gap)));
    spans.push(chip);
    spans
}

/// `https://` and `www.` dropped; a long link keeps its host and its tail.
fn short_url(url: &str, max: usize) -> String {
    let bare = url
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("www.");
    if bare.width() <= max {
        return bare.to_string();
    }
    let host = bare.split('/').next().unwrap_or(bare);
    let tail = bare.rsplit('/').next().unwrap_or("");
    let keep = max.saturating_sub(host.width() + 3);
    let tail: String = tail
        .chars()
        .rev()
        .take(keep)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    fit(&format!("{host}/…{tail}"), max)
}

#[derive(Debug, Clone, Copy)]
enum Kind {
    Commit,
    Slack,
    Upload,
    Link,
}

fn kind_of(a: &LinearAttachment) -> Kind {
    let source = a.source_type.as_deref().unwrap_or("").to_ascii_lowercase();
    let host = a
        .url
        .split("://")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .unwrap_or("")
        .to_ascii_lowercase();
    if a.url.contains("/commit/") && (source.contains("github") || host.ends_with("github.com")) {
        Kind::Commit
    } else if source.contains("slack") || host.ends_with("slack.com") {
        Kind::Slack
    } else if source.contains("upload") || host == "uploads.linear.app" {
        Kind::Upload
    } else {
        Kind::Link
    }
}

fn attachment_card(t: &Theme, a: &LinearAttachment, width: usize, bg: Color) -> Vec<Line<'static>> {
    let p = &t.palette;
    let (glyph, colour, kind, chip) = match kind_of(a) {
        Kind::Commit => (COMMIT, p.mauve, "Commit", " COMMIT "),
        Kind::Slack => (SLACK, p.peach, "Slack thread", " SLACK "),
        Kind::Upload => (IMAGE, p.teal, "Upload", " FILE "),
        Kind::Link => (LINK, p.blue, "Link", " LINK "),
    };
    let inner = width.saturating_sub(2 * INSET);
    let mut out = vec![blank(width, bg)];
    out.push(card_row(
        head(
            vec![
                Span::styled(glyph, Style::default().fg(colour)),
                Span::styled(format!(" {kind}"), t.dim()),
            ],
            Span::styled(
                chip,
                Style::default().fg(colour).add_modifier(Modifier::BOLD),
            ),
            inner,
        ),
        width,
        bg,
    ));
    let title = if a.title.trim().is_empty() {
        "(no title)".to_string()
    } else {
        line(&a.title)
    };
    for text in plain_lines(&title, inner).into_iter().take(2) {
        out.push(card_row(vec![Span::styled(text, t.text())], width, bg));
    }
    if let Some(detail) = a.subtitle.as_deref().filter(|s| !s.trim().is_empty()) {
        out.push(card_row(
            vec![Span::styled(fit(&line(detail), inner), t.dim())],
            width,
            bg,
        ));
    }
    out.push(blank(width, bg));
    out.push(card_row(
        vec![
            Span::styled(LINK, Style::default().fg(p.blue)),
            Span::styled(
                format!(" {}", short_url(&line(&a.url), inner.saturating_sub(2))),
                Style::default()
                    .fg(p.blue)
                    .add_modifier(Modifier::UNDERLINED),
            ),
        ],
        width,
        bg,
    ));
    out.push(blank(width, bg));
    out
}

/// What the card says about `gh` when it has no read to show, or nothing.
fn gh_line(state: &LinearState, read: Option<&PrRead>) -> Option<String> {
    if state.pr_read_missing {
        return Some("checks need a newer board daemon; restart it".into());
    }
    match read {
        None | Some(PrRead::Loading) => Some("reading checks from gh…".into()),
        Some(PrRead::Read(doc)) if doc.pr.is_none() => Some(match doc.message.as_deref() {
            Some(why) if !why.trim().is_empty() => format!("gh unavailable: {}", line(why)),
            _ => "gh unavailable".into(),
        }),
        Some(PrRead::Read(_)) => None,
        Some(PrRead::GhUnavailable(why)) | Some(PrRead::Failed(why)) if !why.trim().is_empty() => {
            Some(format!("gh unavailable: {}", line(why)))
        }
        Some(_) => Some("gh unavailable".into()),
    }
}

fn open_card(
    state: &LinearState,
    a: &LinearAttachment,
    width: usize,
    bg: Color,
) -> Vec<Line<'static>> {
    let t = &state.theme;
    let p = &t.palette;
    let read = state.prs.get(&a.url);
    let gh = gh_detail(read);
    let (repo, number) = pr_shape(&a.url).unwrap_or_default();
    let inner = width.saturating_sub(2 * INSET);
    let (chip, chip_fg) = match pr_state(a, gh) {
        PrState::Draft => (" DRAFT ", p.overlay1),
        _ => (" OPEN ", p.green),
    };
    let mut out = vec![blank(width, bg)];
    out.push(card_row(
        head(
            vec![
                Span::styled(PULL_REQUEST, Style::default().fg(p.green)),
                Span::styled(format!(" #{number}"), t.text().add_modifier(Modifier::BOLD)),
                Span::styled(format!("  {}", line(&repo)), t.dim()),
            ],
            Span::styled(
                chip,
                Style::default().fg(chip_fg).add_modifier(Modifier::BOLD),
            ),
            inner,
        ),
        width,
        bg,
    ));
    let title = gh
        .and_then(|g| g.title.clone())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| a.title.clone());
    for text in plain_lines(&line(&title), inner).into_iter().take(2) {
        out.push(card_row(vec![Span::styled(text, t.text())], width, bg));
    }
    let branch = gh
        .and_then(|g| g.head_ref.clone())
        .or_else(|| meta(a, "branch").map(str::to_string));
    if let Some(branch) = branch.filter(|b| !b.trim().is_empty()) {
        let target = meta(a, "targetBranch")
            .map(|target| format!(" → {}", line(target)))
            .unwrap_or_default();
        out.push(card_row(
            vec![
                Span::styled(BRANCH, t.dim()),
                Span::styled(format!(" {}{target}", line(&branch)), t.dim()),
            ],
            width,
            bg,
        ));
    }
    out.push(blank(width, bg));
    if let Some(g) = gh {
        if g.additions.is_some() || g.deletions.is_some() {
            let mut diff = vec![
                Span::styled(DIFF, Style::default().fg(p.yellow)),
                Span::styled(
                    format!(" +{}", g.additions.unwrap_or(0)),
                    Style::default().fg(p.green),
                ),
                Span::styled(
                    format!(" −{}", g.deletions.unwrap_or(0)),
                    Style::default().fg(p.red),
                ),
            ];
            if let Some(files) = g.changed_files {
                diff.push(Span::styled(format!("  {files} files"), t.dim()));
            }
            out.push(card_row(diff, width, bg));
        }
        if let Some(ci) = &g.ci {
            let mut checks = vec![
                Span::styled(CHECK, Style::default().fg(p.green)),
                Span::styled(
                    format!(" {} passed", ci.passed),
                    Style::default().fg(p.green),
                ),
            ];
            if ci.failed > 0 {
                checks.push(Span::styled(
                    format!("  {CROSS} {} failed", ci.failed),
                    Style::default().fg(p.red),
                ));
            }
            if ci.pending > 0 {
                checks.push(Span::styled(
                    format!("  {CLOCK} {} pending", ci.pending),
                    Style::default().fg(p.yellow),
                ));
            }
            checks.push(Span::styled(format!("  {} skipped", ci.skipped), t.dim()));
            out.push(card_row(checks, width, bg));
        }
        for check in g.checks.iter().filter(|c| c.bucket == "fail") {
            out.push(card_row(
                vec![Span::styled(
                    fit(&format!("   {CROSS} {}", line(&check.name)), inner),
                    Style::default().fg(p.red),
                )],
                width,
                bg,
            ));
        }
        if let Some(reason) = g.blocker.as_deref().filter(|b| !b.trim().is_empty()) {
            let text = format!("Blocked: {}", line(reason));
            for (n, part) in plain_lines(&text, inner.saturating_sub(2))
                .into_iter()
                .take(2)
                .enumerate()
            {
                let lead = if n == 0 { "■ " } else { "  " };
                out.push(card_row(
                    vec![Span::styled(format!("{lead}{part}"), t.warning())],
                    width,
                    bg,
                ));
            }
        }
        let reviews: Vec<String> = g
            .reviews
            .iter()
            .map(|r| {
                let who = r
                    .author
                    .as_deref()
                    .map(line)
                    .unwrap_or_else(|| "someone".into());
                let what = r
                    .state
                    .as_deref()
                    .map(|s| line(s).to_lowercase().replace('_', " "))
                    .unwrap_or_default();
                format!("{who} {what}").trim().to_string()
            })
            .collect();
        if !reviews.is_empty() {
            out.push(card_row(
                vec![
                    Span::styled(PERSON, Style::default().fg(p.blue)),
                    Span::styled(format!(" {}", reviews.join(" · ")), t.dim()),
                ],
                width,
                bg,
            ));
        }
        if let Some(url) = g.preview.as_deref().filter(|u| !u.trim().is_empty()) {
            out.push(card_row(
                vec![
                    Span::styled(EYE, Style::default().fg(p.teal)),
                    Span::styled(
                        format!(" {}", short_url(&line(url), inner.saturating_sub(2))),
                        Style::default()
                            .fg(p.teal)
                            .add_modifier(Modifier::UNDERLINED),
                    ),
                ],
                width,
                bg,
            ));
        }
    }
    if gh.is_none() {
        if let Some(status) = meta(a, "status") {
            out.push(card_row(
                vec![Span::styled(format!("Linear: {}", line(status)), t.dim())],
                width,
                bg,
            ));
        }
    }
    if let Some(why) = gh_line(state, read) {
        out.push(card_row(
            vec![Span::styled(fit(&why, inner), t.dim())],
            width,
            bg,
        ));
    }
    out.push(blank(width, bg));
    out
}

/// A merged or closed pull request on one line, its repo and diff in
/// columns.
fn done_line(state: &LinearState, now: i64, a: &LinearAttachment, width: usize) -> Line<'static> {
    let t = &state.theme;
    let p = &t.palette;
    let gh = gh_detail(state.prs.get(&a.url));
    let (repo, number) = pr_shape(&a.url).unwrap_or_default();
    let merged = pr_state(a, gh) == PrState::Merged;
    let glyph_fg = if merged { p.mauve } else { p.red };
    let left = vec![
        Span::styled(format!("  {MERGE}"), Style::default().fg(glyph_fg)),
        Span::styled(format!(" #{number:<5}"), t.secondary()),
        Span::styled(format!(" {:<16}", fit(&line(&repo), 16)), t.dim()),
    ];
    let diff = gh
        .filter(|g| g.additions.is_some() || g.deletions.is_some())
        .map(|g| {
            (
                format!("+{}", g.additions.unwrap_or(0)),
                format!("−{}", g.deletions.unwrap_or(0)),
            )
        });
    let when = gh
        .and_then(|g| g.merged_at.clone())
        .or_else(|| meta(a, "mergedAt").map(str::to_string))
        .map(|at| ago(now, Some(&at)))
        .unwrap_or_default();
    let when = if merged { when } else { "closed".into() };
    let (add, del) = diff.unwrap_or_default();
    let right = vec![
        Span::styled(format!("{add:>6}"), Style::default().fg(p.overlay1)),
        Span::styled(format!(" {del:<5}"), t.dim()),
        Span::styled(format!(" {when:<6}"), t.dim()),
    ];
    let title = gh
        .and_then(|g| g.title.clone())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| a.title.clone());
    let room = width.saturating_sub(spans_width(&left) + spans_width(&right) + 2);
    let title = fit(&line(&title), room);
    let gap = width.saturating_sub(spans_width(&left) + 1 + title.width() + spans_width(&right));
    let mut spans = left;
    spans.push(Span::styled(
        format!(" {title}"),
        Style::default().fg(p.overlay1),
    ));
    spans.push(Span::raw(" ".repeat(gap)));
    spans.extend(right);
    Line::from(clip_spans(spans, width))
}

fn push_card(
    out: &mut Vec<Line<'static>>,
    rows: &mut Vec<Row>,
    url: &str,
    card: Vec<Line<'static>>,
) {
    rows.push(Row {
        line: out.len(),
        kind: RowKind::Link(url.to_string()),
        column: Column::Main,
        height: card.len(),
    });
    out.extend(card);
}

/// ATTACHMENTS: every attachment that is not a pull request, as a card.
pub(super) fn attachments_section(
    _app: &App,
    state: &LinearState,
    attachments: &[LinearAttachment],
    width: usize,
    bg: Color,
    out: &mut Vec<Line<'static>>,
    rows: &mut Vec<Row>,
) {
    let others: Vec<&LinearAttachment> = attachments
        .iter()
        .filter(|a| pr_shape(&a.url).is_none())
        .collect();
    if others.is_empty() {
        return;
    }
    let t = &state.theme;
    out.push(Line::from(""));
    out.push(section(
        t,
        &format!("ATTACHMENTS  {}", others.len()),
        "",
        width,
    ));
    for a in others {
        out.push(Line::from(""));
        push_card(out, rows, &a.url, attachment_card(t, a, width, bg));
    }
}

/// PULL REQUESTS: open ones as cards, merged and closed ones a line each.
pub(super) fn pull_requests_section(
    app: &App,
    state: &LinearState,
    attachments: &[LinearAttachment],
    width: usize,
    bg: Color,
    out: &mut Vec<Line<'static>>,
    rows: &mut Vec<Row>,
) {
    let prs = pull_requests(attachments);
    if prs.is_empty() {
        return;
    }
    let states: Vec<PrState> = prs
        .iter()
        .map(|a| pr_state(a, gh_detail(state.prs.get(&a.url))))
        .collect();
    let open = states
        .iter()
        .filter(|s| matches!(s, PrState::Open | PrState::Draft))
        .count();
    let merged = states.iter().filter(|s| **s == PrState::Merged).count();
    let t = &state.theme;
    out.push(Line::from(""));
    out.push(section(
        t,
        &format!("PULL REQUESTS  {}", prs.len()),
        &format!("{open} open · {merged} merged"),
        width,
    ));
    for (a, s) in prs.iter().zip(&states) {
        if matches!(s, PrState::Open | PrState::Draft) {
            out.push(Line::from(""));
            push_card(out, rows, &a.url, open_card(state, a, width, bg));
        }
    }
    if open < prs.len() {
        out.push(Line::from(""));
    }
    for (a, s) in prs.iter().zip(&states) {
        if matches!(s, PrState::Merged | PrState::Closed) {
            push_card(out, rows, &a.url, vec![done_line(state, app.now, a, width)]);
        }
    }
}
