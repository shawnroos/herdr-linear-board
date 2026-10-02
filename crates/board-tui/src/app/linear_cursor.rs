//! The Linear board's card cursor: which tab, column, lane and card is
//! selected, held as indices for drawing and re-found by key after every read
//! and every tab switch.

use board_core::protocol::{LinearGroup, LinearTab};

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

/// The selection by key, plus the indices it had, which are the fallback when
/// the keys no longer resolve.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CardCursor {
    pub tab: String,
    pub column: String,
    pub lane: String,
    pub identifier: Option<String>,
    pub at: (usize, usize, usize),
}

fn slot_in(group: &LinearGroup, lane: &str, identifier: &str) -> Option<usize> {
    let cards = column_cards(group);
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

    /// `(lane key, identifier)` of the selected card.
    pub fn selected_slot(&self) -> Option<(&str, &str)> {
        column_cards(self.groups().get(self.sel_group)?)
            .get(self.sel_card)
            .copied()
    }

    pub fn selected_identifier(&self) -> Option<&str> {
        self.selected_slot().map(|(_, id)| id)
    }

    pub(super) fn selected_column_len(&self) -> usize {
        self.groups()
            .get(self.sel_group)
            .map_or(0, |g| column_cards(g).len())
    }

    pub fn cursor(&self) -> CardCursor {
        let column = self.groups().get(self.sel_group);
        let slot = self.selected_slot();
        CardCursor {
            tab: self.active_tab().map(|t| t.key.clone()).unwrap_or_default(),
            column: column.map(|g| g.key.clone()).unwrap_or_default(),
            lane: slot.map(|(lane, _)| lane.to_string()).unwrap_or_default(),
            identifier: slot.map(|(_, id)| id.to_string()),
            at: (self.sel_tab, self.sel_group, self.sel_card),
        }
    }

    /// The card at `(column, lane, identifier)` in the active tab: that lane,
    /// then any lane of that column, then any column.
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
            .and_then(|c| Some((c, slot_in(&groups[c], lane, identifier)?)));
        in_column.or_else(|| {
            groups
                .iter()
                .enumerate()
                .find_map(|(c, g)| Some((c, slot_in(g, lane, identifier)?)))
        })
    }

    /// Re-finds `cursor` after the document changed: its tab by key, then its
    /// card, and otherwise the same column with the index clamped.
    pub(super) fn refind(&mut self, cursor: &CardCursor) {
        self.sel_tab = self
            .tabs()
            .iter()
            .position(|t| t.key == cursor.tab)
            .unwrap_or(cursor.at.0);
        self.refind_in_tab(cursor);
    }

    fn refind_in_tab(&mut self, cursor: &CardCursor) {
        let found = cursor
            .identifier
            .as_deref()
            .and_then(|id| self.locate(&cursor.column, &cursor.lane, id));
        match found {
            Some((group, card)) => {
                self.sel_group = group;
                self.sel_card = card;
            }
            None => {
                self.sel_group = self
                    .groups()
                    .iter()
                    .position(|g| g.key == cursor.column)
                    .unwrap_or(cursor.at.1);
                self.sel_card = cursor.at.2;
            }
        }
        self.clamp();
    }

    /// `[` / `]`: the tab `delta` away, wrapping. Each tab keeps its own
    /// cursor, so coming back lands on the card left there.
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

    /// Selects `(tab, group, card)`. Leaving a tab stores its cursor, as `[`
    /// and `]` do, so coming back lands where it was left.
    pub(super) fn select_slot(&mut self, tab: usize, group: usize, card: usize) {
        if tab != self.sel_tab {
            let from = self.cursor();
            self.tab_cursors.insert(from.tab.clone(), from);
            self.sel_tab = tab;
        }
        self.sel_group = group;
        self.sel_card = card;
        self.clamp();
    }

    /// Selects `identifier` where the active tab draws it, else on the first
    /// other tab that does. False when no tab draws it.
    pub(super) fn select_identifier(&mut self, identifier: &str) -> bool {
        let tabs = self.tabs().len();
        let order = std::iter::once(self.sel_tab).chain((0..tabs).filter(|&t| t != self.sel_tab));
        let found = order.filter(|&t| t < tabs).find_map(|t| {
            self.tabs()[t]
                .groups
                .iter()
                .enumerate()
                .find_map(|(g, group)| {
                    let card = column_cards(group)
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

    /// The next (`delta > 0`) or previous slot, across every tab, whose card
    /// `wanted` accepts, wrapping at either end.
    pub(super) fn next_slot(
        &self,
        delta: isize,
        wanted: impl Fn(&str) -> bool,
    ) -> Option<(usize, usize, usize)> {
        let mut slots = vec![];
        for (t, tab) in self.tabs().iter().enumerate() {
            for (g, group) in tab.groups.iter().enumerate() {
                for (c, (_, id)) in column_cards(group).into_iter().enumerate() {
                    if wanted(id) {
                        slots.push((t, g, c));
                    }
                }
            }
        }
        let at = (self.sel_tab, self.sel_group, self.sel_card);
        if delta >= 0 {
            slots
                .iter()
                .find(|&&slot| slot > at)
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

    /// `<` / `>`: the first column of the page `delta` pages away, with
    /// `per_page` columns to a page. Stops at the first and last page.
    pub(super) fn jump_page(&mut self, delta: isize, per_page: usize) {
        let per_page = per_page.max(1);
        let pages = self.groups().len().div_ceil(per_page);
        let page = (self.sel_group / per_page) as isize + delta;
        if page < 0 || page as usize >= pages {
            return;
        }
        self.sel_group = page as usize * per_page;
        self.clamp();
    }
}
