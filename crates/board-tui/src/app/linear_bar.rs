//! The Linear board's bottom bar: global controls, a tip for the selection
//! and board-wide alerts. Items are kept by identity, so an alert list that
//! changes under the cursor re-finds the item rather than an index.

use board_core::protocol::MarkKind;
use crossterm::event::{KeyCode, KeyEvent};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::linear_cursor::{column_cards, Focus, Side};
use super::nav::nav_delta;
use super::{App, Density, Effect, LinearState, Screen};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BarItem {
    Keys,
    Filter,
    Deselect,
    Quit,
    Request(i64),
    NeedsYou,
    Question,
    CiFailing(String),
    CiCollapsed,
    GhUnavailable,
    More,
}

pub const CONTROLS: [BarItem; 4] = [
    BarItem::Keys,
    BarItem::Filter,
    BarItem::Deselect,
    BarItem::Quit,
];
pub const BAR_HINT: &str = "? bar";
/// The room the tip keeps on a one-row bar. Fixed, so the bar's height does
/// not change as the selection, and the tip's length with it, moves.
const TIP_MIN: usize = 40;
pub const BAR_GAP: usize = 2;
const WHO_MAX: usize = 16;

/// A label's cells on the bar: one blank either side.
pub fn item_width(label: &str) -> usize {
    label.width() + 2
}

fn clip(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        if used + w + 1 > max {
            break;
        }
        used += w;
        out.push(c);
    }
    out.push('…');
    out
}

fn one_line(s: &str) -> String {
    s.replace(['\n', '\t'], " ")
}

/// How the bar lays out at one width: one row or three, and which alerts
/// are on screen. `all` is every alert in the CI form that was chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BarLayout {
    pub stacked: bool,
    pub hint: bool,
    pub all: Vec<BarItem>,
    pub from: usize,
    pub shown: usize,
}

impl BarLayout {
    pub fn rows(&self) -> u16 {
        if self.stacked {
            3
        } else {
            1
        }
    }

    pub fn alerts(&self) -> &[BarItem] {
        &self.all[self.from..self.from + self.shown]
    }

    pub fn hidden(&self) -> usize {
        self.all.len() - self.shown
    }

    pub fn more_label(&self) -> String {
        format!("+{} more", self.hidden())
    }

    /// The items ←→ walk through, as drawn left to right.
    pub fn order(&self) -> Vec<BarItem> {
        let mut order = CONTROLS.to_vec();
        order.extend(self.alerts().iter().cloned());
        if self.hidden() > 0 {
            order.push(BarItem::More);
        }
        order
    }
}

/// The alerts from `from` that fit `room` with `+N more` after them, at
/// least one. A selected alert pulls the window by the least that shows it.
fn window(widths: &[usize], from: usize, selected: Option<usize>, room: usize) -> (usize, usize) {
    let total = widths.len();
    if widths.iter().sum::<usize>() <= room {
        return (0, total);
    }
    let more = item_width(&format!("+{total} more"));
    let fits = |from: usize| {
        let mut used = more;
        let mut n = 0;
        for w in &widths[from..] {
            if n > 0 && used + w > room {
                break;
            }
            used += w;
            n += 1;
        }
        n
    };
    let mut from = from.min(total.saturating_sub(1));
    if let Some(at) = selected {
        from = from.min(at);
        while at >= from + fits(from) {
            from += 1;
        }
    }
    (from, fits(from))
}

impl LinearState {
    /// Every identifier the snapshot's tabs draw, in tab, column and lane
    /// order, once each.
    fn board_ids(&self) -> Vec<&str> {
        let mut ids: Vec<&str> = vec![];
        for tab in self.tabs() {
            for group in &tab.groups {
                for (_, id) in column_cards(group) {
                    if !ids.contains(&id) {
                        ids.push(id);
                    }
                }
            }
        }
        ids
    }

    fn marked(&self, kind: MarkKind) -> Vec<&str> {
        self.board_ids()
            .into_iter()
            .filter(|id| self.marks_for(id).any(|m| m.kind == kind))
            .collect()
    }

    fn failing_ci(&self) -> Vec<&str> {
        self.board_ids()
            .into_iter()
            .filter(|id| {
                self.issue(id)
                    .and_then(|issue| issue.pr.as_ref()?.ci.as_ref())
                    .is_some_and(|ci| ci.state == "fail")
            })
            .collect()
    }

    fn gh_unavailable(&self) -> Option<String> {
        let gh = self.snapshot()?.gh.as_ref()?;
        (gh.status == "unavailable").then(|| gh.message.clone().unwrap_or_default())
    }

    fn request_issue_of(&self, id: i64) -> Option<(String, String)> {
        let request = self.pending_requests().into_iter().find(|r| r.id == id)?;
        let who = request
            .requested_by
            .as_deref()
            .map(one_line)
            .filter(|w| !w.trim().is_empty())
            .unwrap_or_else(|| "an agent".into());
        Some((who, request.issue.clone()))
    }

    /// Show-requests oldest first, then needs-you, questions and failing CI:
    /// one per PR, or one for all of them when `collapse`.
    pub fn bar_alerts(&self, collapse: bool) -> Vec<BarItem> {
        let mut alerts: Vec<BarItem> = self
            .pending_requests()
            .iter()
            .map(|r| BarItem::Request(r.id))
            .collect();
        if !self.marked(MarkKind::Attention).is_empty() {
            alerts.push(BarItem::NeedsYou);
        }
        if !self.marked(MarkKind::Question).is_empty() {
            alerts.push(BarItem::Question);
        }
        // CI data gh could not read is never counted, passing or failing.
        if self.gh_unavailable().is_some() {
            alerts.push(BarItem::GhUnavailable);
            return alerts;
        }
        let failing = self.failing_ci();
        if collapse && !failing.is_empty() {
            alerts.push(BarItem::CiCollapsed);
        } else {
            alerts.extend(
                failing
                    .into_iter()
                    .map(|id| BarItem::CiFailing(id.to_string())),
            );
        }
        alerts
    }

    /// An item's text; `More`'s count comes from the layout instead.
    pub fn bar_label(&self, item: &BarItem) -> String {
        match item {
            BarItem::Keys => "keys".into(),
            BarItem::Filter => "/ filter".into(),
            BarItem::Deselect => "esc deselect".into(),
            BarItem::Quit => "ctrl+q quit".into(),
            BarItem::Request(id) => match self.request_issue_of(*id) {
                Some((who, issue)) => format!("◉ {} {issue}", clip(&who, WHO_MAX)),
                None => "◉".into(),
            },
            BarItem::NeedsYou => format!("! {} needs you", self.marked(MarkKind::Attention).len()),
            BarItem::Question => match self.marked(MarkKind::Question).len() {
                1 => "? 1 question".into(),
                n => format!("? {n} questions"),
            },
            BarItem::CiFailing(id) => format!("✗ {id} CI failing"),
            BarItem::CiCollapsed => format!("✗ {} CI failing", self.failing_ci().len()),
            BarItem::GhUnavailable => "gh unavailable".into(),
            BarItem::More => "more".into(),
        }
    }

    pub fn bar_layout(&self, width: u16) -> BarLayout {
        let w = usize::from(width);
        let widths = |items: &[BarItem]| -> Vec<usize> {
            items
                .iter()
                .map(|i| item_width(&self.bar_label(i)))
                .collect()
        };
        let left = item_width(BAR_HINT) + widths(&CONTROLS).iter().sum::<usize>();
        let collapsed = self.bar_alerts(true);
        let collapsed_widths = widths(&collapsed);
        let alerts_w = match collapsed_widths.iter().sum::<usize>() {
            0 => 0,
            n => BAR_GAP + n,
        };
        let stacked = left + BAR_GAP + TIP_MIN + alerts_w > w;
        let room = if stacked {
            w
        } else {
            w.saturating_sub(left + 2 * BAR_GAP + TIP_MIN)
        };
        let expanded = self.bar_alerts(false);
        let expanded_widths = widths(&expanded);
        let (all, all_widths) = if expanded_widths.iter().sum::<usize>() <= room {
            (expanded, expanded_widths)
        } else {
            (collapsed, collapsed_widths)
        };
        let at = match &self.focus {
            Focus::Bar(item) => all.iter().position(|a| a == item),
            _ => None,
        };
        let (from, shown) = window(&all_widths, self.bar_from, at, room);
        BarLayout {
            stacked,
            hint: !stacked || left <= w,
            all,
            from,
            shown,
        }
    }

    /// Keeps the stored window what the bar draws at `width`, and where the
    /// selected item sits, the fallback when it vanishes.
    pub(super) fn bar_fit(&mut self, width: u16) {
        let Focus::Bar(item) = &self.focus else {
            return;
        };
        let layout = self.bar_layout(width);
        if let Some(at) = layout.order().iter().position(|i| i == item) {
            self.bar_at = at;
        }
        self.bar_from = layout.from;
    }

    /// After the alerts changed: a selected item that is gone gives way to
    /// the item now at its place. A request that went this way makes the next
    /// `a` or `x` refuse, rather than answer the item that took its place.
    pub(super) fn refind_bar(&mut self, width: u16) {
        let Focus::Bar(item) = self.focus.clone() else {
            return;
        };
        let order = self.bar_layout(width).order();
        if !order.contains(&item) {
            let at = self.bar_at.min(order.len() - 1);
            self.focus = Focus::Bar(order[at].clone());
            self.bar_vanished = matches!(item, BarItem::Request(_));
        }
        self.bar_fit(width);
    }

    fn open_bar(&mut self, width: u16) {
        self.strip_focus = false;
        self.remember_card();
        self.bar_from = 0;
        self.bar_vanished = false;
        self.bar_select(BarItem::Keys, width);
    }

    fn bar_select(&mut self, item: BarItem, width: u16) {
        self.focus = Focus::Bar(item);
        self.bar_fit(width);
    }

    /// Moves the window past the alerts it shows; with none past them, back
    /// to the start, selecting `wrap_to` (`More` means the first alert).
    fn bar_more(&mut self, width: u16, wrap_to: BarItem) {
        let layout = self.bar_layout(width);
        let next = layout.from + layout.shown;
        if let Some(item) = layout.all.get(next) {
            self.bar_from = next;
            return self.bar_select(item.clone(), width);
        }
        self.bar_from = 0;
        let first = match wrap_to {
            BarItem::More => layout.all.first().cloned().unwrap_or(BarItem::Keys),
            other => other,
        };
        self.bar_select(first, width);
    }

    fn bar_step(&mut self, delta: isize, width: u16) {
        let Focus::Bar(item) = self.focus.clone() else {
            return;
        };
        self.bar_vanished = false;
        let layout = self.bar_layout(width);
        let order = layout.order();
        let at = order
            .iter()
            .position(|i| *i == item)
            .unwrap_or(self.bar_at.min(order.len() - 1));
        if delta > 0 && item == BarItem::More {
            return self.bar_more(width, BarItem::Keys);
        }
        if delta < 0 && at == CONTROLS.len() && layout.from > 0 {
            self.bar_from = layout.from - 1;
            return self.bar_select(layout.all[layout.from - 1].clone(), width);
        }
        let next = (at as isize + delta).rem_euclid(order.len() as isize) as usize;
        self.bar_select(order[next].clone(), width);
    }

    /// Selects `identifier` wherever the board draws it: the lane folding it
    /// opens, a filter hiding it clears, and the page follows it.
    fn jump_to(&mut self, identifier: &str, width: u16) -> bool {
        let found = self.tabs().iter().find_map(|tab| {
            tab.groups.iter().find_map(|group| {
                column_cards(group)
                    .into_iter()
                    .find(|(_, id)| *id == identifier)
                    .map(|(lane, _)| (tab.key.clone(), lane.to_string()))
            })
        });
        let Some((tab, lane)) = found else {
            return false;
        };
        if !self.filter_matches(identifier) {
            self.clear_filter();
        }
        if let Some(folds) = self.folds.get_mut(&tab) {
            folds.remove(&lane);
            if folds.is_empty() {
                self.folds.remove(&tab);
            }
        }
        let found = self.select_identifier(identifier);
        self.refit(width);
        found
    }

    /// A group alert: the filter takes `kind`'s chip and keeps its text, and
    /// the first of `marked` it still shows is selected. With none shown the
    /// filter stays selected over the empty board.
    fn filter_by(&mut self, kind: MarkKind, marked: &[String], width: u16) {
        self.filter_chip = Some(kind);
        let first = marked.iter().find(|id| self.filter_matches(id));
        match first.cloned() {
            Some(id) => {
                self.jump_to(&id, width);
            }
            None => {
                self.focus = Focus::Filter;
                self.refit(width);
            }
        }
    }

    /// The bar's tip for what is selected at `width` cells, saying when a
    /// pinned issue is hidden for want of room (KTD12).
    pub fn bar_tip(&self, width: u16) -> String {
        let tip = self.focus_tip(width);
        match &self.pinned {
            Some(pin)
                if !self.pin_shown(width)
                    && matches!(
                        self.focus,
                        Focus::Card(_) | Focus::LaneLine(_) | Focus::None
                    ) =>
            {
                format!("{} is still pinned · {tip}", pin.issue)
            }
            _ => tip,
        }
    }

    fn focus_tip(&self, width: u16) -> String {
        match &self.focus {
            Focus::Pinned => {
                "↑↓ scroll the pinned issue · Enter follows the row · Esc or p goes back".into()
            }
            Focus::Bar(item) => self.item_tip(item),
            Focus::Filter if self.filter_chip.is_some() => {
                "Type to narrow by id or title · Backspace drops the chip · Esc clears".into()
            }
            Focus::Filter => {
                "Type to filter cards by id or title · Enter or ↓ returns to the board".into()
            }
            Focus::None => {
                let z = match self.density {
                    Density::Roomy => "z compacts cards",
                    Density::Compact => "z expands cards",
                };
                format!("Nothing selected · ↑↓, Tab or Enter selects a card · {z}")
            }
            Focus::LaneLine(key) => self.lane_tip(key),
            Focus::Pager(side, _) => {
                let page = self.page(width);
                let toward = match side {
                    Side::Left => "left",
                    Side::Right => "right",
                };
                let hidden = page.hidden(*side);
                let plural = if hidden == 1 { "" } else { "s" };
                format!(
                    "Pages {hidden} hidden column{plural} on the {toward} into view · Enter returns to your card"
                )
            }
            Focus::Card(_) => match self.selected_identifier() {
                None => "This column has no cards · Tab moves to the next".into(),
                Some(id) => self.card_tip(id),
            },
        }
    }

    fn lane_tip(&self, key: &str) -> String {
        let label = self
            .lanes()
            .into_iter()
            .find(|(lane, _)| *lane == key)
            .map_or(key.to_string(), |(_, label)| one_line(label));
        if !self.folded(key) {
            return format!("Enter folds {label} in every column");
        }
        let hidden: usize = self
            .groups()
            .iter()
            .flat_map(|g| self.lane_rows(g, None))
            .filter(|row| row.key == key)
            .map(|row| row.count)
            .sum();
        format!("Enter unfolds {label} · {hidden} cards hidden")
    }

    fn card_tip(&self, id: &str) -> String {
        let signals: Vec<&str> = self
            .overlays()
            .get(id)
            .marks()
            .into_iter()
            .map(|(_, kind)| match kind {
                Some(MarkKind::Attention) => "needs you",
                Some(MarkKind::Question) => "has a question",
                Some(MarkKind::Suggestion) => "has a suggestion",
                Some(MarkKind::Done) => "is done",
                None => "wants to show you something",
            })
            .collect();
        let mut tip = if signals.is_empty() {
            format!("Enter opens {id} · Tab changes column")
        } else {
            format!("{id} {} · Enter opens it", signals.join(" and "))
        };
        if self.selected_suggestion().is_some() {
            tip.push_str(" · a binds it · x dismisses it");
        }
        tip
    }

    fn item_tip(&self, item: &BarItem) -> String {
        let count = |n: usize, one: &str, many: &str| {
            if n == 1 {
                format!("1 {one}")
            } else {
                format!("{n} {many}")
            }
        };
        let go = |n: usize| {
            if n > 1 {
                "Enter filters the board to them"
            } else {
                "Enter goes to it"
            }
        };
        match item {
            BarItem::Keys => "Enter lists every key · ←→ chooses · ↑ back to the board".into(),
            BarItem::Filter => "Enter opens the filter".into(),
            BarItem::Deselect => "Enter clears the selection".into(),
            BarItem::Quit => "Enter quits the board".into(),
            BarItem::Request(id) => match self.request_issue_of(*id) {
                Some((who, issue)) => format!(
                    "{who} asks to show you {issue} · a accepts · x dismisses · Enter goes to it"
                ),
                None => "request no longer pending".into(),
            },
            BarItem::NeedsYou => {
                let n = self.marked(MarkKind::Attention).len();
                format!("{} you · {}", count(n, "card needs", "cards need"), go(n))
            }
            BarItem::Question => {
                let n = self.marked(MarkKind::Question).len();
                format!(
                    "{} · {}",
                    count(n, "card has a question", "cards have questions"),
                    go(n)
                )
            }
            BarItem::CiFailing(id) => {
                format!("{id}'s pull request is failing CI · Enter goes to it")
            }
            BarItem::CiCollapsed => format!(
                "{} failing CI · Enter goes to the first",
                count(
                    self.failing_ci().len(),
                    "pull request is",
                    "pull requests are"
                )
            ),
            BarItem::GhUnavailable => match self.gh_unavailable() {
                Some(why) if !why.trim().is_empty() => format!(
                    "gh unavailable: {} · checks show from Linear only",
                    one_line(&why)
                ),
                _ => "gh unavailable · checks show from Linear only".into(),
            },
            BarItem::More => "Enter shows the alerts that do not fit".into(),
        }
    }
}

/// `?` on the board.
pub(super) fn open(app: &mut App) {
    let width = app.last_area.width;
    if let Some(state) = app.linear.as_mut() {
        state.open_bar(width);
    }
}

/// Keys while the bar has the focus. `None` hands the key to the board.
pub(super) fn bar_key(app: &mut App, k: KeyEvent) -> Option<Vec<Effect>> {
    let width = app.last_area.width;
    let state = app.linear.as_mut()?;
    let Focus::Bar(item) = state.focus.clone() else {
        return None;
    };
    if let Some(delta) = nav_delta(k.code) {
        if delta < 0 {
            state.bar_vanished = false;
            state.reselect();
        }
        return Some(vec![]);
    }
    match k.code {
        KeyCode::Left | KeyCode::Char('h') | KeyCode::BackTab => state.bar_step(-1, width),
        KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab => state.bar_step(1, width),
        KeyCode::Esc => {
            state.bar_vanished = false;
            state.deselect();
        }
        KeyCode::Enter => return Some(run(app, item)),
        KeyCode::Char('a') => return Some(answer(app, item, true)),
        KeyCode::Char('x') => return Some(answer(app, item, false)),
        KeyCode::Char('/' | 'n' | 'N' | '[' | ']' | 'z') => return None,
        _ => {}
    }
    Some(vec![])
}

fn run(app: &mut App, item: BarItem) -> Vec<Effect> {
    let width = app.last_area.width;
    let Some(state) = app.linear.as_mut() else {
        return vec![];
    };
    state.bar_vanished = false;
    let target = match &item {
        BarItem::Keys => {
            app.help_return_to = app.screen;
            app.help_scroll = 0;
            app.screen = Screen::Help;
            return vec![];
        }
        BarItem::Filter => {
            state.open_filter();
            return vec![];
        }
        BarItem::Deselect => {
            state.deselect();
            return vec![];
        }
        BarItem::Quit => return vec![Effect::Quit],
        BarItem::More => {
            state.bar_more(width, BarItem::More);
            return vec![];
        }
        BarItem::GhUnavailable => {
            let tip = state.item_tip(&item);
            app.set_toast(tip, false);
            return vec![];
        }
        BarItem::Request(id) => state.request_issue_of(*id).map(|(_, issue)| issue),
        BarItem::NeedsYou | BarItem::Question => {
            let kind = if item == BarItem::NeedsYou {
                MarkKind::Attention
            } else {
                MarkKind::Question
            };
            let marked: Vec<String> = state.marked(kind).into_iter().map(str::to_string).collect();
            if marked.len() > 1 {
                state.filter_by(kind, &marked, width);
                return vec![];
            }
            marked.into_iter().next()
        }
        BarItem::CiFailing(id) => Some(id.clone()),
        BarItem::CiCollapsed => state.failing_ci().first().map(|s| s.to_string()),
    };
    let Some(target) = target else {
        app.set_toast("that alert has cleared", false);
        return vec![];
    };
    if !state.jump_to(&target, width) {
        app.set_toast(
            format!("{target} is not on this board; a opens its request"),
            false,
        );
    }
    vec![]
}

/// `a` / `x` on a show-request alert. `x` moves the bar to the next item at
/// once, so the cursor never waits on the dismissal it sent.
fn answer(app: &mut App, item: BarItem, accept: bool) -> Vec<Effect> {
    let width = app.last_area.width;
    let Some(state) = app.linear.as_mut() else {
        return vec![];
    };
    if std::mem::take(&mut state.bar_vanished) {
        app.set_toast("request no longer pending", false);
        return vec![];
    }
    let BarItem::Request(id) = item else {
        app.set_toast(
            if accept {
                "nothing to accept"
            } else {
                "nothing to dismiss"
            },
            false,
        );
        return vec![];
    };
    let Some((_, issue)) = state.request_issue_of(id) else {
        app.set_toast("request no longer pending", false);
        return vec![];
    };
    if accept {
        return vec![Effect::LinearShowAccept { id, issue }];
    }
    let all = state.bar_layout(width).all;
    let at = all.iter().position(|i| *i == item).unwrap_or(0);
    let next = all
        .get(at + 1)
        .or(at.checked_sub(1).and_then(|before| all.get(before)))
        .cloned()
        .unwrap_or(BarItem::Keys);
    state.bar_select(next, width);
    vec![Effect::LinearShowDismiss { id }]
}
