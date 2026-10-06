//! The Linear board's focus: which tab and column, and whether a card, a lane
//! line, the filter or nothing is selected. The tab's lanes form one list
//! across its columns, and fold state is kept per tab by lane key. The focus
//! is re-found by key after every read and every tab switch.

use board_core::protocol::{LinearGroup, LinearTab};

use super::nav::step_clamped;
use super::LinearState;

/// One lane's cards in one column. `label` is `None` only on a board with no
/// lane grouping, where the column is a single run of cards with no header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneSection<'a> {
    pub key: &'a str,
    pub label: Option<&'a str>,
    pub issues: Vec<&'a str>,
}

const NO_LANE: &str = "no lane";

/// A column's cards grouped by lane, in lane order. A lane with no cards in
/// this column is left out. The lanes need not cover `group.issues`: any card
/// no lane lists follows under a "no lane" header, so nothing on the board is
/// dropped.
pub fn lane_sections(group: &LinearGroup) -> Vec<LaneSection<'_>> {
    if group.lanes.is_empty() {
        return vec![LaneSection {
            key: "",
            label: None,
            issues: group.issues.iter().map(String::as_str).collect(),
        }];
    }
    let mut sections: Vec<LaneSection> = group
        .lanes
        .iter()
        .filter(|lane| !lane.issues.is_empty())
        .map(|lane| LaneSection {
            key: &lane.key,
            label: Some(&lane.label),
            issues: lane.issues.iter().map(String::as_str).collect(),
        })
        .collect();
    let loose: Vec<&str> = group
        .issues
        .iter()
        .filter(|id| !group.lanes.iter().any(|lane| lane.issues.contains(id)))
        .map(String::as_str)
        .collect();
    if !loose.is_empty() {
        sections.push(LaneSection {
            key: "",
            label: Some(NO_LANE),
            issues: loose,
        });
    }
    sections
}

/// Every card slot of a column as `(lane key, identifier)`, in drawing order.
/// An issue listed in two lanes has two slots.
pub fn column_cards(group: &LinearGroup) -> Vec<(&str, &str)> {
    lane_sections(group)
        .into_iter()
        .flat_map(|section| {
            let lane = section.key;
            section.issues.into_iter().map(move |id| (lane, id))
        })
        .collect()
}

/// A tab's lanes as `(key, label)`: every lane any column lists, in order of
/// first appearance, then "no lane" when a laned column has loose cards. Empty
/// for a board without lane grouping.
pub fn tab_lanes(tab: &LinearTab) -> Vec<(&str, &str)> {
    let mut lanes: Vec<(&str, &str)> = vec![];
    for group in &tab.groups {
        let listed = group
            .lanes
            .iter()
            .map(|lane| (lane.key.as_str(), lane.label.as_str()));
        let loose = lane_sections(group)
            .into_iter()
            .filter_map(|section| Some((section.key, section.label?)));
        for lane in listed.chain(loose) {
            if !lanes.iter().any(|(key, _)| *key == lane.0) {
                lanes.push(lane);
            }
        }
    }
    lanes
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

/// What the board has selected. `Card` indexes the focused column's visible
/// cards; `None` keeps `sel_group` as the column the next key selects in. A
/// paging button holds the focus it was entered from, so paging never moves
/// the board's selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Focus {
    Card(usize),
    LaneLine(String),
    Filter,
    Pager(Side, Box<Focus>),
    Bar(super::BarItem),
    None,
}

impl Default for Focus {
    fn default() -> Focus {
        Focus::Card(0)
    }
}

impl Focus {
    /// The board's own selection: through a paging button to the focus it
    /// was entered from.
    pub fn board(&self) -> &Focus {
        match self {
            Focus::Pager(_, from) => from.board(),
            other => other,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Page {
    pub start: usize,
    pub visible: usize,
    pub total: usize,
}

impl Page {
    pub fn end(&self) -> usize {
        self.start + self.visible
    }

    pub fn hidden(&self, side: Side) -> usize {
        match side {
            Side::Left => self.start,
            Side::Right => self.total.saturating_sub(self.end()),
        }
    }

    fn edge(&self, side: Side) -> usize {
        match side {
            Side::Left => self.start,
            Side::Right => self.end().saturating_sub(1),
        }
    }
}

/// One lane as a column draws it: `issues` is empty while the lane is folded,
/// and `count` is the cards it holds there either way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneRow<'a> {
    pub key: &'a str,
    pub label: Option<&'a str>,
    pub issues: Vec<&'a str>,
    pub count: usize,
    pub folded: bool,
}

/// The focus by key, plus the indices it had, which are the fallback when the
/// keys no longer resolve. `at.2` is the card index for a card and the lane
/// index for a lane line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CardCursor {
    pub tab: String,
    pub column: String,
    pub lane: String,
    pub identifier: Option<String>,
    pub focus: Focus,
    pub at: (usize, usize, usize),
}

fn slot_in(cards: &[(&str, &str)], lane: &str, identifier: &str) -> Option<usize> {
    cards
        .iter()
        .position(|&(l, id)| l == lane && id == identifier)
        .or_else(|| cards.iter().position(|&(_, id)| id == identifier))
}

impl LinearState {
    pub fn tabs(&self) -> &[LinearTab] {
        self.snapshot().map(|s| s.tabs.as_slice()).unwrap_or(&[])
    }

    pub fn active_tab(&self) -> Option<&LinearTab> {
        self.tabs().get(self.sel_tab)
    }

    pub fn groups(&self) -> &[LinearGroup] {
        self.active_tab()
            .map(|tab| tab.groups.as_slice())
            .unwrap_or(&[])
    }

    /// Whether `identifier` passes the board filter: a case-insensitive
    /// substring of its identifier and title, as the pickers match.
    pub fn filter_matches(&self, identifier: &str) -> bool {
        if self.filter.is_empty() {
            return true;
        }
        let needle = super::state::collapse_spaces(&self.filter.to_lowercase());
        let title = self
            .issue(identifier)
            .map_or("", |issue| issue.title.as_str());
        super::state::collapse_spaces(&format!("{identifier} {title}").to_lowercase())
            .contains(&needle)
    }

    pub fn folded(&self, lane: &str) -> bool {
        self.active_tab()
            .and_then(|tab| self.folds.get(&tab.key))
            .is_some_and(|folds| folds.contains(lane))
    }

    /// The active tab's lane lines. With a filter, only lanes holding a card
    /// it matches.
    pub fn lanes(&self) -> Vec<(&str, &str)> {
        let Some(tab) = self.active_tab() else {
            return vec![];
        };
        let mut lanes = tab_lanes(tab);
        if !self.filter.is_empty() {
            lanes.retain(|(key, _)| {
                tab.groups.iter().any(|group| {
                    column_cards(group)
                        .iter()
                        .any(|(lane, id)| lane == key && self.filter_matches(id))
                })
            });
        }
        lanes
    }

    fn visible_in<'a>(&'a self, tab: &str, group: &'a LinearGroup) -> Vec<(&'a str, &'a str)> {
        let folds = self.folds.get(tab);
        column_cards(group)
            .into_iter()
            .filter(|(lane, id)| {
                !folds.is_some_and(|folds| folds.contains(*lane)) && self.filter_matches(id)
            })
            .collect()
    }

    /// `(lane key, identifier)` of every card a column shows: folded lanes
    /// and cards the filter hides are left out.
    pub fn visible_cards(&self, column: usize) -> Vec<(&str, &str)> {
        match (self.active_tab(), self.groups().get(column)) {
            (Some(tab), Some(group)) => self.visible_in(&tab.key, group),
            _ => vec![],
        }
    }

    /// A column's lanes in the tab's lane order, each with the cards the
    /// filter lets through. A lane with none is left out unless it is `keep`.
    pub fn lane_rows<'a>(&'a self, group: &'a LinearGroup, keep: Option<&str>) -> Vec<LaneRow<'a>> {
        let sections = lane_sections(group);
        let lanes = self.lanes();
        let matching = |section: &LaneSection<'a>| -> Vec<&'a str> {
            section
                .issues
                .iter()
                .copied()
                .filter(|id| self.filter_matches(id))
                .collect()
        };
        let mut rows = vec![];
        for (key, label) in &lanes {
            let issues: Vec<&str> = sections
                .iter()
                .filter(|section| section.key == *key && section.label.is_some())
                .flat_map(&matching)
                .collect();
            if issues.is_empty() && keep != Some(*key) {
                continue;
            }
            let folded = self.folded(key);
            rows.push(LaneRow {
                key,
                label: Some(label),
                count: issues.len(),
                issues: if folded { vec![] } else { issues },
                folded,
            });
        }
        for section in &sections {
            let laned = section.label.is_some() && lanes.iter().any(|(key, _)| *key == section.key);
            let issues = matching(section);
            if laned || issues.is_empty() {
                continue;
            }
            rows.push(LaneRow {
                key: section.key,
                label: section.label,
                count: issues.len(),
                issues,
                folded: false,
            });
        }
        rows
    }

    /// `(lane key, identifier)` of the selected card.
    pub fn selected_slot(&self) -> Option<(&str, &str)> {
        match *self.focus.board() {
            Focus::Card(card) => self.visible_cards(self.sel_group).get(card).copied(),
            _ => None,
        }
    }

    pub fn selected_identifier(&self) -> Option<&str> {
        self.selected_slot().map(|(_, id)| id)
    }

    /// The selected lane line's key.
    pub fn selected_lane_line(&self) -> Option<&str> {
        match self.focus.board() {
            Focus::LaneLine(key) => Some(key),
            _ => None,
        }
    }

    pub fn cursor(&self) -> CardCursor {
        let column = self.groups().get(self.sel_group);
        let slot = self.selected_slot();
        let (lane, third) = match self.focus.board() {
            Focus::Card(card) => (slot.map(|(lane, _)| lane.to_string()), *card),
            Focus::LaneLine(key) => (
                Some(key.clone()),
                self.lanes()
                    .iter()
                    .position(|(lane, _)| lane == key)
                    .unwrap_or(0),
            ),
            _ => (None, 0),
        };
        CardCursor {
            tab: self.active_tab().map(|t| t.key.clone()).unwrap_or_default(),
            column: column.map(|g| g.key.clone()).unwrap_or_default(),
            lane: lane.unwrap_or_default(),
            identifier: slot.map(|(_, id)| id.to_string()),
            focus: self.focus.clone(),
            at: (self.sel_tab, self.sel_group, third),
        }
    }

    /// The card at `(column, lane, identifier)` among the active tab's
    /// visible cards: that lane, then any lane of that column, then any column.
    pub(super) fn locate(
        &self,
        column: &str,
        lane: &str,
        identifier: &str,
    ) -> Option<(usize, usize)> {
        let groups = self.groups();
        let in_column = groups
            .iter()
            .position(|g| g.key == column)
            .and_then(|c| Some((c, slot_in(&self.visible_cards(c), lane, identifier)?)));
        in_column.or_else(|| {
            (0..groups.len())
                .find_map(|c| Some((c, slot_in(&self.visible_cards(c), lane, identifier)?)))
        })
    }

    /// Where a card the fold or the filter hides sits: its column and lane.
    fn hidden_lane(&self, column: &str, lane: &str, identifier: &str) -> Option<(usize, String)> {
        let groups = self.groups();
        let order = groups
            .iter()
            .position(|g| g.key == column)
            .into_iter()
            .chain(0..groups.len());
        order.into_iter().find_map(|c| {
            let cards = column_cards(&groups[c]);
            let at = slot_in(&cards, lane, identifier)?;
            Some((c, cards[at].0.to_string()))
        })
    }

    /// The lane line for `lane`, else the first lane line. With no lane
    /// line, the filter when it is what hid every card; otherwise the empty
    /// column stays focused with no card, so `h` / `l` are not swallowed as
    /// filter text.
    fn line_or_filter(&self, lane: Option<&str>) -> Focus {
        let lanes = self.lanes();
        match lane
            .and_then(|lane| lanes.iter().find(|(key, _)| *key == lane))
            .or(lanes.first())
        {
            Some((key, _)) => Focus::LaneLine(key.to_string()),
            None if self.filter.is_empty() => Focus::Card(0),
            None => Focus::Filter,
        }
    }

    /// Clamps the indices and moves a focus that no longer resolves: a card
    /// index past the column's cards clamps, an empty column selects a lane
    /// line, and a lane line that is gone selects another.
    pub(super) fn settle(&mut self) {
        self.sel_tab = self.sel_tab.min(self.tabs().len().saturating_sub(1));
        self.sel_group = self.sel_group.min(self.groups().len().saturating_sub(1));
        self.focus = self.settled(&self.focus);
    }

    fn settled(&self, focus: &Focus) -> Focus {
        match focus {
            Focus::Card(card) => match self.visible_cards(self.sel_group).len() {
                0 => self.line_or_filter(None),
                n => Focus::Card((*card).min(n - 1)),
            },
            Focus::LaneLine(key) => self.line_or_filter(Some(key)),
            Focus::Pager(side, from) => Focus::Pager(*side, Box::new(self.settled(from))),
            other => other.clone(),
        }
    }

    /// Re-finds `cursor` after the document changed: its tab by key, then its
    /// focus.
    pub(super) fn refind(&mut self, cursor: &CardCursor) {
        self.sel_tab = self
            .tabs()
            .iter()
            .position(|t| t.key == cursor.tab)
            .unwrap_or(cursor.at.0);
        self.prune_folds();
        self.refind_in_tab(cursor);
    }

    fn refind_in_tab(&mut self, cursor: &CardCursor) {
        self.sel_group = self
            .groups()
            .iter()
            .position(|g| g.key == cursor.column)
            .unwrap_or(cursor.at.1);
        self.focus = self.refind_focus(&cursor.focus, cursor);
        self.clamp();
    }

    fn refind_focus(&mut self, focus: &Focus, cursor: &CardCursor) -> Focus {
        match focus {
            Focus::Card(_) => self.refind_card(cursor),
            Focus::LaneLine(key) => {
                let lanes = self.lanes();
                if lanes.iter().any(|(lane, _)| lane == key) {
                    Focus::LaneLine(key.clone())
                } else {
                    // The lane that followed the vanished one now sits at its index.
                    lanes
                        .get(cursor.at.2)
                        .or(lanes.last())
                        .map_or(Focus::Filter, |(lane, _)| Focus::LaneLine(lane.to_string()))
                }
            }
            Focus::Pager(side, from) => {
                Focus::Pager(*side, Box::new(self.refind_focus(from, cursor)))
            }
            other => other.clone(),
        }
    }

    /// The card again, else the line of the folded or filtered lane hiding
    /// it, else the same index in the same column.
    fn refind_card(&mut self, cursor: &CardCursor) -> Focus {
        let Some(id) = cursor.identifier.as_deref() else {
            return Focus::Card(cursor.at.2);
        };
        if let Some((group, card)) = self.locate(&cursor.column, &cursor.lane, id) {
            self.sel_group = group;
            return Focus::Card(card);
        }
        if let Some((group, lane)) = self.hidden_lane(&cursor.column, &cursor.lane, id) {
            self.sel_group = group;
            return self.line_or_filter(Some(&lane));
        }
        Focus::Card(cursor.at.2)
    }

    /// A lane that left its tab drops its fold; a tab that left drops all.
    fn prune_folds(&mut self) {
        let tabs: Vec<(String, Vec<String>)> = self
            .tabs()
            .iter()
            .map(|tab| {
                let lanes = tab_lanes(tab).into_iter().map(|(key, _)| key.to_string());
                (tab.key.clone(), lanes.collect())
            })
            .collect();
        self.folds
            .retain(|tab, folds| match tabs.iter().find(|(key, _)| key == tab) {
                Some((_, lanes)) => {
                    folds.retain(|lane| lanes.contains(lane));
                    !folds.is_empty()
                }
                None => false,
            });
    }

    /// Return on a lane line: folds or unfolds it in every column of the tab.
    pub(super) fn toggle_fold(&mut self, lane: &str) {
        let Some(tab) = self.active_tab().map(|t| t.key.clone()) else {
            return;
        };
        let folds = self.folds.entry(tab.clone()).or_default();
        if !folds.remove(lane) {
            folds.insert(lane.to_string());
        }
        if folds.is_empty() {
            self.folds.remove(&tab);
        }
    }

    /// ↑ / ↓ through the focused column's lane lines and cards, in drawing
    /// order. ↑ from the first opens the filter unless `wheel`.
    pub(super) fn step(&mut self, delta: isize, wheel: bool) {
        let cards = self.visible_cards(self.sel_group);
        let mut stops = vec![];
        let mut placed = vec![false; cards.len()];
        for (key, _) in self.lanes() {
            stops.push(Focus::LaneLine(key.to_string()));
            for (at, (lane, _)) in cards.iter().enumerate() {
                if *lane == key {
                    stops.push(Focus::Card(at));
                    placed[at] = true;
                }
            }
        }
        stops.extend((0..cards.len()).filter(|at| !placed[*at]).map(Focus::Card));
        let at = stops.iter().position(|stop| *stop == self.focus);
        if delta < 0 && at.unwrap_or(0) == 0 {
            if !wheel {
                self.open_filter();
            }
            return;
        }
        let Some(at) = at else {
            return;
        };
        self.focus = stops[step_clamped(at, delta, stops.len() - 1)].clone();
    }

    pub(super) fn remember_card(&mut self) {
        if matches!(self.focus.board(), Focus::Card(_)) {
            self.last_card = Some(self.cursor());
        }
    }

    pub(super) fn open_filter(&mut self) {
        self.remember_card();
        self.focus = Focus::Filter;
    }

    /// Esc: nothing selected; `sel_group` stays as the column to come back to.
    pub(super) fn deselect(&mut self) {
        self.remember_card();
        self.focus = Focus::None;
    }

    /// Selects again in the focused column: the last card selected when it is
    /// still shown there, else the column's first card, else a lane line,
    /// else the filter.
    pub(super) fn reselect(&mut self) {
        let column = self.groups().get(self.sel_group).map(|g| g.key.clone());
        let last = self.last_card.clone().and_then(|cursor| {
            let id = cursor.identifier?;
            let column = column.as_deref()?;
            let (group, card) = self.locate(column, &cursor.lane, &id)?;
            (group == self.sel_group).then_some(card)
        });
        self.focus = Focus::Card(last.unwrap_or(0));
        self.clamp();
    }

    /// ← / → / Tab at `width` cells: the column `delta` away, clamped. A lane
    /// line stays selected; a card that has no counterpart there selects its
    /// lane's line. Past the page's last shown column, the paging button on
    /// that side. On a button, toward it pages; away steps onto the column
    /// beside it.
    pub(super) fn move_column(&mut self, delta: isize, width: u16) {
        let page = self.page(width);
        self.page_start = page.start;
        let toward = if delta > 0 { Side::Right } else { Side::Left };
        if let Focus::Pager(side, _) = self.focus {
            if side == toward {
                self.page_start = match side {
                    Side::Left => page.start.saturating_sub(1),
                    Side::Right => page.start + 1,
                };
                self.refit(width);
            } else {
                self.leave_pager_to(page.edge(side));
            }
            return;
        }
        // The filter keeps the keyboard while it is open, so it never hands
        // focus to a button.
        if self.focus != Focus::Filter
            && self.sel_group == page.edge(toward)
            && page.hidden(toward) > 0
        {
            let from = std::mem::replace(&mut self.focus, Focus::None);
            self.focus = Focus::Pager(toward, Box::new(from));
            return;
        }
        let lane = self.selected_slot().map(|(lane, _)| lane.to_string());
        let max = self.groups().len().saturating_sub(1);
        self.sel_group = step_clamped(self.sel_group, delta, max);
        if matches!(self.focus, Focus::Card(_)) && self.visible_cards(self.sel_group).is_empty() {
            self.focus = self.line_or_filter(lane.as_deref());
        }
        self.clamp();
        self.refit(width);
    }

    /// The page at `width` cells. Only a paging button may leave the focused
    /// column off the page; any other focus pulls the page by the least that
    /// shows it.
    pub fn page(&self, width: u16) -> Page {
        let total = self.groups().len();
        let visible = crate::view::board_columns(width).min(total);
        let mut start = self.page_start.min(total - visible);
        if visible > 0 && !matches!(self.focus, Focus::Pager(..)) {
            let sel = self.sel_group.min(total - 1);
            if sel < start {
                start = sel;
            } else if sel >= start + visible {
                start = sel + 1 - visible;
            }
        }
        Page {
            start,
            visible,
            total,
        }
    }

    /// Keeps the stored page what `page` draws at `width`, and moves a
    /// paging button's focus whose button is gone onto the edge column.
    pub(super) fn refit(&mut self, width: u16) {
        let page = self.page(width);
        self.page_start = page.start;
        if let Focus::Pager(side, _) = self.focus {
            if page.hidden(side) == 0 {
                self.leave_pager_to(page.edge(side));
            }
        }
    }

    /// Off a paging button onto `column`, carrying the selection the button
    /// held as `move_column` would.
    fn leave_pager_to(&mut self, column: usize) {
        let lane = self.selected_slot().map(|(lane, _)| lane.to_string());
        self.focus = self.focus.board().clone();
        self.sel_group = column;
        match self.focus {
            Focus::None => return self.reselect(),
            Focus::Card(_) if self.visible_cards(column).is_empty() => {
                self.focus = self.line_or_filter(lane.as_deref());
            }
            _ => {}
        }
        self.clamp();
    }

    /// Enter, Esc or ↑ / ↓ on a paging button: back to the selection it held;
    /// the page follows it on the next fit.
    pub(super) fn leave_pager(&mut self) {
        if let Focus::Pager(_, from) = &self.focus {
            self.focus = from.board().clone();
            self.clamp();
        }
    }

    /// A sideways scroll toward `side`, or `<` / `>`: pages one column and
    /// selects the button that still has columns behind it, preferring
    /// `side`'s. The board's selection stays where it is.
    pub(super) fn scroll_page(&mut self, side: Side, width: u16) {
        if matches!(self.focus, Focus::Filter) {
            return;
        }
        let page = self.page(width);
        if page.hidden(Side::Left) + page.hidden(Side::Right) == 0 {
            return;
        }
        let start = match side {
            Side::Left => page.start.saturating_sub(1),
            Side::Right => (page.start + 1).min(page.total - page.visible),
        };
        let paged = Page { start, ..page };
        let button = match (paged.hidden(side) > 0, side) {
            (true, _) => side,
            (false, Side::Left) => Side::Right,
            (false, Side::Right) => Side::Left,
        };
        let from = std::mem::replace(&mut self.focus, Focus::None);
        self.focus = Focus::Pager(button, Box::new(from.board().clone()));
        self.page_start = start;
    }

    /// `[` / `]`: the tab `delta` away, wrapping. Each tab keeps its own
    /// cursor, so coming back lands on what was left selected there.
    pub(super) fn cycle_tab(&mut self, delta: isize) {
        let n = self.tabs().len();
        if n < 2 {
            return;
        }
        let from = self.cursor();
        let next = (self.sel_tab as isize + delta).rem_euclid(n as isize) as usize;
        let key = self.tabs()[next].key.clone();
        let target = self
            .tab_cursors
            .get(&key)
            .cloned()
            .unwrap_or_else(|| CardCursor {
                tab: key.clone(),
                ..from.clone()
            });
        self.tab_cursors.insert(from.tab.clone(), from);
        self.sel_tab = next;
        self.refind_in_tab(&target);
    }

    /// Selects card `card` of column `group` in tab `tab`. Leaving a tab
    /// stores its cursor, as `[` and `]` do, so coming back lands where it
    /// was left.
    pub(super) fn select_slot(&mut self, tab: usize, group: usize, card: usize) {
        if tab != self.sel_tab {
            let from = self.cursor();
            self.tab_cursors.insert(from.tab.clone(), from);
            self.sel_tab = tab;
        }
        self.sel_group = group;
        self.focus = Focus::Card(card);
        self.clamp();
    }

    /// Selects `identifier` where the active tab shows it, else on the first
    /// other tab that does. False when no tab shows it.
    pub(super) fn select_identifier(&mut self, identifier: &str) -> bool {
        let tabs = self.tabs().len();
        let order = std::iter::once(self.sel_tab).chain((0..tabs).filter(|&t| t != self.sel_tab));
        let found = order.filter(|&t| t < tabs).find_map(|t| {
            let tab = &self.tabs()[t];
            tab.groups.iter().enumerate().find_map(|(g, group)| {
                let card = self
                    .visible_in(&tab.key, group)
                    .iter()
                    .position(|&(_, id)| id == identifier)?;
                Some((t, g, card))
            })
        });
        match found {
            Some((tab, group, card)) => {
                self.select_slot(tab, group, card);
                true
            }
            None => false,
        }
    }

    /// The next (`delta > 0`) or previous shown card, across every tab, that
    /// `wanted` accepts, wrapping at either end. With no card selected it
    /// starts from the top of the focused column.
    pub(super) fn next_slot(
        &self,
        delta: isize,
        wanted: impl Fn(&str) -> bool,
    ) -> Option<(usize, usize, usize)> {
        let mut slots = vec![];
        for (t, tab) in self.tabs().iter().enumerate() {
            for (g, group) in tab.groups.iter().enumerate() {
                for (c, (_, id)) in self.visible_in(&tab.key, group).into_iter().enumerate() {
                    if wanted(id) {
                        slots.push((t, g, c));
                    }
                }
            }
        }
        let (card, on_card) = match *self.focus.board() {
            Focus::Card(card) => (card, true),
            _ => (0, false),
        };
        let at = (self.sel_tab, self.sel_group, card);
        if delta >= 0 {
            slots
                .iter()
                .find(|&&slot| slot > at || (!on_card && slot == at))
                .or(slots.first())
                .copied()
        } else {
            slots
                .iter()
                .rev()
                .find(|&&slot| slot < at)
                .or(slots.last())
                .copied()
        }
    }
}
