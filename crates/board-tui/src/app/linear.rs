//! Linear mode: the state and reducer for a herdr space bound to a Linear
//! project. The document is boardd's `linear.snapshot`,
//! overlaid by identifier with the space's local state (`linear.state.get`);
//! nothing here reads `App::board`. It writes local state only through boardd,
//! never Linear and never the plugin's records: the mark clears, show-request
//! answers and binds it emits are daemon requests on the Linear allow list.
//! The herdr writes it asks the daemon for are this pane's title and pane
//! focus.

use board_core::protocol::LinearState as LocalState;
use board_core::protocol::{
    LinearBinding, LinearIssue, LinearIssueDocument, LinearLinkedIssue, LinearListEnvelope,
    LinearListKind, LinearListResult, LinearSnapshot, LinearSpaceRow, LinearSpacesList, LinearTab,
    Mark, MarkKind, Note, ShowRequest,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::{HashMap, HashSet};

use super::linear_cursor::{CardCursor, Focus, Side};
use super::nav::{nav_delta, step_clamped};
use super::{App, Effect, Msg, Screen};

/// Why a snapshot request failed, as the driver classifies the client error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinearFailure {
    /// The daemon answered "unknown method": it predates `linear.snapshot`.
    MethodNotFound,
    /// The work plugin is installed and current but ships no script for this
    /// op (protocol code 7). Its remedy is to update the plugin, so a reader
    /// must not offer the retry it offers for `Failed`.
    OpUnsupported(String),
    /// No answer within the client's read limit for that request.
    TimedOut(std::time::Duration),
    /// `gh` could not run in the daemon (protocol code 8): a PR read only.
    GhUnavailable(String),
    /// Any other failure, with the daemon's or the transport's message.
    Failed(String),
}

/// A read's timeout as the board says it: `r` sends the read again.
pub(super) fn read_timeout_text(limit: std::time::Duration) -> String {
    format!(
        "the daemon did not answer within {}s; press r to try again",
        limit.as_secs()
    )
}

/// What a Linear worker delivers: one snapshot or one list, each classified.
#[derive(Debug)]
pub enum LinearArrival {
    Snapshot(Box<Result<LinearSnapshot, LinearFailure>>),
    Issue {
        /// The issue this read was asked for. Compared against the open page
        /// before anything is applied.
        issue: String,
        /// The read this answers, as `request_issue` numbered it. Compared
        /// against the read still in flight, because the identifier alone
        /// cannot tell two reads for the same issue apart.
        generation: u64,
        result: Box<Result<LinearIssueDocument, LinearFailure>>,
    },
    List {
        kind: LinearListKind,
        id: Option<String>,
        result: Result<LinearListResult, LinearFailure>,
    },
    State(Box<Result<LocalState, LinearFailure>>),
    Pr {
        url: String,
        result: Box<Result<board_core::protocol::LinearPrDocument, LinearFailure>>,
    },
    Session(Box<Result<board_core::protocol::LinearSessionGetResult, LinearFailure>>),
    PaneContext(Box<Result<board_core::protocol::PaneContext, LinearFailure>>),
    Wrote {
        write: LinearWrite,
        result: Result<(), WriteFailure>,
    },
}

/// A local-state write the board sent, named again when its answer lands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinearWrite {
    ClearMarks { ids: Vec<i64>, on_open: bool },
    AcceptShow { id: i64, issue: String },
    DismissShow { id: i64 },
    Bind { mark: Option<i64>, issue: String },
    SpaceBind { space: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteFailure {
    /// Not found or no longer pending: another reader, the agent or the
    /// expiry sweep closed it first.
    Gone(String),
    Failed(String),
}

/// One card's marks, show-request badge and newest note.
#[derive(Debug, Clone, Copy, Default)]
pub struct CardOverlay<'a> {
    attention: bool,
    question: bool,
    suggestion: bool,
    done: bool,
    requested: bool,
    pub latest_note: Option<&'a Note>,
}

impl<'a> CardOverlay<'a> {
    fn add_mark(&mut self, kind: MarkKind) {
        match kind {
            MarkKind::Attention => self.attention = true,
            MarkKind::Question => self.question = true,
            MarkKind::Suggestion => self.suggestion = true,
            MarkKind::Done => self.done = true,
        }
    }

    /// On equal keys the later note wins, as `Iterator::max_by` picks.
    fn add_note(&mut self, note: &'a Note) {
        if self
            .latest_note
            .is_none_or(|latest| (&note.created_at, note.id) >= (&latest.created_at, latest.id))
        {
            self.latest_note = Some(note);
        }
    }

    /// The 2-cell gutter left of a card's identifier: its glyphs in priority
    /// order, the first plus `+` when more than two.
    pub fn gutter(&self) -> String {
        let glyphs: Vec<char> = self.marks().into_iter().map(|(glyph, _)| glyph).collect();
        match glyphs.as_slice() {
            [] => "  ".to_string(),
            [one] => format!("{one} "),
            [one, two] => format!("{one}{two}"),
            [first, ..] => format!("{first}+"),
        }
    }

    /// Every mark glyph the card carries, in the gutter's priority order;
    /// `None` is the show-request badge, which has no mark kind.
    pub fn marks(&self) -> Vec<(char, Option<MarkKind>)> {
        let mut glyphs = vec![];
        for (present, kind) in [
            (self.attention, MarkKind::Attention),
            (self.question, MarkKind::Question),
            (self.suggestion, MarkKind::Suggestion),
        ] {
            if present {
                glyphs.push((mark_glyph(kind), Some(kind)));
            }
        }
        if self.requested {
            glyphs.push(('◉', None));
        }
        if self.done {
            glyphs.push((mark_glyph(MarkKind::Done), Some(MarkKind::Done)));
        }
        glyphs
    }

    /// What the attention counts count: a needs-you, question or suggestion
    /// mark, or a show-request badge. Done is not a call to act.
    pub fn needs_attention(&self) -> bool {
        self.attention || self.question || self.suggestion || self.requested
    }
}

/// [`CardOverlay`]s by identifier; a card with no local state gets the empty one.
pub struct CardOverlays<'a>(HashMap<&'a str, CardOverlay<'a>>);

impl<'a> CardOverlays<'a> {
    pub fn get(&self, identifier: &str) -> CardOverlay<'a> {
        self.0.get(identifier).copied().unwrap_or_default()
    }
}

pub fn mark_glyph(kind: MarkKind) -> char {
    match kind {
        MarkKind::Attention => '!',
        MarkKind::Question => '?',
        MarkKind::Suggestion => '◇',
        MarkKind::Done => '✓',
    }
}

/// Where a suggestion's bind points: its worktree, else the cwd it was
/// reported from.
fn suggestion_cwd(mark: &Mark) -> Option<String> {
    let detail = mark.detail.as_ref()?;
    ["worktree_path", "cwd"].iter().find_map(|key| {
        detail
            .get(key)
            .and_then(serde_json::Value::as_str)
            .filter(|path| !path.trim().is_empty())
            .map(str::to_string)
    })
}

/// One pane row of the detail screen: which binding it belongs to, its id,
/// and the live status the daemon attached (`unknown` when it attached none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneRow {
    pub binding: usize,
    pub pane_id: String,
    pub status: String,
}

/// The space list the strip draws from. A failed read is kept apart from an
/// empty one, so the strip never reports a failure as "every space is bound".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SpaceList {
    #[default]
    NotRead,
    Read(LinearSpacesList),
    Failed(String),
}

/// The `local_state_changed` events the runtime drained in one tick, by the
/// space each named (`None` is any space), with whether any of them carried
/// the daemon's post-refetch `snapshot` flag.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalStateSignals(std::collections::BTreeMap<Option<String>, bool>);

impl LocalStateSignals {
    pub fn add(&mut self, space: Option<String>, snapshot: bool) {
        *self.0.entry(space).or_default() |= snapshot;
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// `None` when no event concerns `space`; otherwise whether one that does
    /// carried the snapshot flag.
    pub fn for_space(&self, space: &str) -> Option<bool> {
        self.0
            .iter()
            .filter(|(named, _)| named.as_deref().is_none_or(|named| named == space))
            .map(|(_, snapshot)| *snapshot)
            .reduce(|a, b| a || b)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StripView {
    #[default]
    Spaces,
    Tabs,
}

/// How tall cards draw: roomy keeps padding, rules and blank rows; compact
/// drops them. Per board session, never saved.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Density {
    #[default]
    Roomy,
    Compact,
}

/// How the person last asked to see an open issue. Below the modal width the
/// view draws narrow whatever this says, and this comes back when it widens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum IssueMode {
    #[default]
    Window,
    Expanded,
}

pub struct LinearState {
    pub workspace_id: String,
    pub board_version: String,
    pub daemon_version: Option<String>,
    /// The last document that arrived, sanitised. Stays on screen when a
    /// later refresh fails.
    pub last_good: Option<LinearSnapshot>,
    pub sel_tab: usize,
    /// The focused column, kept while nothing is selected as the column the
    /// next key selects in.
    pub sel_group: usize,
    pub focus: Focus,
    /// The first column the board shows. Paging moves only this, so a
    /// paging button can leave the focused column off the page.
    pub page_start: usize,
    /// The first alert the bar shows; alerts before it sit behind `+N more`.
    pub bar_from: usize,
    /// Where the selected bar item sat, the fallback when it vanishes.
    pub bar_at: usize,
    /// The selected show-request vanished and its place was taken: the next
    /// `a` or `x` refuses rather than answering the item now under the cursor.
    pub bar_vanished: bool,
    /// The board's focus when `?` moved it from an open issue to the bar,
    /// given back when the bar is left.
    pub bar_return: Option<Focus>,
    /// The board filter's text; it hides cards it does not match.
    pub filter: String,
    /// The mark a group alert filters by; a card shows only when it carries
    /// this mark and matches the text.
    pub filter_chip: Option<MarkKind>,
    /// Folded lane keys by tab key.
    pub folds: std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
    /// The card selected when the focus last left a card, which Esc and the
    /// filter return to.
    pub last_card: Option<CardCursor>,
    /// The cursor each tab had when it was left, by tab key.
    pub tab_cursors: std::collections::BTreeMap<String, CardCursor>,
    /// The board's vertical offset in rows. The draw moves it by the least
    /// that keeps the selected item in view, so it needs the last value; a
    /// `Cell` because the view only borrows the state, as with `App::hit_map`.
    pub board_scroll: std::cell::Cell<u16>,
    pub density: Density,
    /// The identifier open on `Screen::LinearDetail`.
    pub detail: Option<String>,
    pub in_flight: bool,
    /// An automatic refresh (a reconnect) asked for while one was in flight;
    /// it is sent when that one lands, so it is not lost.
    pub queued: bool,
    /// `R` pressed while a read was in flight: the queued read is forced.
    pub queued_force: bool,
    pub error: Option<String>,
    /// `(board_version, daemon_version)` once the daemon answered that it has
    /// no `linear.snapshot`.
    pub stale_daemon: Option<(String, Option<String>)>,
    /// `App::now` when `last_good` arrived.
    pub fetched_at: Option<i64>,
    /// Lists with a read on the way, keyed by kind and the list's argument.
    pub lists_in_flight: std::collections::BTreeSet<(LinearListKind, Option<String>)>,
    /// The row last chosen in a Linear picker, for the flow that opened it.
    pub pick: Option<super::LinearPick>,
    /// Read once at start; shown only in the `?` sheet.
    pub herdr_keys: Vec<crate::herdr_keys::HerdrKey>,
    pub theme: crate::theme::Theme,
    pub spaces: SpaceList,
    pub strip: StripView,
    /// Whether up, down, Enter and Escape act on the strip instead of the board.
    pub strip_focus: bool,
    /// Index into [`LinearState::unbound_spaces`].
    pub strip_sel: usize,
    /// The space chosen on the strip, whose project picker is or was open.
    /// A project pick (`pick.kind == Projects`) is for this space.
    pub bind_space: Option<LinearSpaceRow>,
    /// The fetched detail for the issue page that is open, and the issue it was
    /// fetched for. Keyed so a read that lands after the reader has moved on is
    /// dropped rather than painted over the page they are looking at.
    pub detail_doc: Option<(String, LinearIssueDocument)>,
    /// The issue a `linear.issue` read is in flight for, and which read it is.
    /// Esc back to an issue whose first read has not landed starts a SECOND
    /// read for that same issue, so the identifier alone cannot say which
    /// answer is the current one.
    pub detail_in_flight: Option<(String, u64)>,
    /// How many issue reads this page has started. Only ever increments; it
    /// numbers reads, it does not count them down.
    pub detail_reads: u64,
    /// What the row the reader opened already knew about a linked issue. The
    /// board's snapshot holds only the issues on the board, and a sub-issue or
    /// relation is deliberately off it, so without this the page would have
    /// nothing to draw until the read landed - and nothing at all if it failed.
    pub detail_seed: Option<LinearLinkedIssue>,
    /// How the last read for the open issue failed, and whether `r` can retry
    /// it. A plugin that ships no issue script is not retryable.
    pub detail_error: Option<DetailError>,
    /// Which row of the issue page is selected, by identity rather than by
    /// index. The rows arrive in two waves - panes as soon as the page opens,
    /// then sub-issues, parent and relations when the read lands ABOVE them -
    /// so an index would quietly come to mean a different row.
    pub detail_selection: Option<crate::view::IssueRowKind>,
    /// The issues opened to get to the one on screen, oldest first. Each entry
    /// keeps what its page last showed, so stepping back re-renders it while
    /// its own fresh read runs rather than emptying to loading markers.
    pub detail_stack: Vec<DetailStep>,
    /// The last local-state read that succeeded, sanitised. Kept whole rather
    /// than pruned to the snapshot's issues, so a mark on a card the board has
    /// not fetched yet shows once a snapshot brings the card.
    pub local: Option<LocalState>,
    /// A `linear.state.get` is on the way. Separate from `in_flight`: the two
    /// reads answer different events and neither waits for the other.
    pub local_in_flight: bool,
    /// Local state changed again while a read was in flight; one more read is
    /// sent when it lands, however many changes arrived.
    pub local_queued: bool,
    /// The marks the open card showed when it was opened, by issue, so
    /// the list outlives their clearing and a linked issue does not show it.
    pub detail_marks: Option<(String, Vec<Mark>)>,
    /// The issue page's scroll floor. Only the session pane sets it; the
    /// board scrolls by its selected row alone.
    pub detail_scroll: usize,
    /// Whether the issue view's scroll follows the selected row. Paging past
    /// the rows turns it off until the selection moves again.
    pub detail_follow: bool,
    pub issue_mode: IssueMode,
    /// The card the open issue was opened from and where it sat, so closing
    /// can select the next shown card when opening it hid it.
    pub opened_from: Option<(String, (usize, usize, usize))>,
    pub pinned: Option<PinnedIssue>,
    /// What the daemon's PR read returned, by pull request URL. Kept across
    /// opens, so a card shows the last read while the next one runs.
    pub prs: HashMap<String, PrRead>,
    pub prs_in_flight: HashSet<String>,
    pub prs_queued: HashSet<String>,
    /// The daemon predates `linear.pr`: cards show Linear's data only, and
    /// no further read is sent.
    pub pr_read_missing: bool,
    /// Where an agent-opened board lands, taken by the first good snapshot.
    pub landing: Option<crate::ShowContext>,
    /// `Some` when the board knows its own herdr pane, so it can pick its
    /// level from where that pane sits.
    pub level: Option<super::LevelState>,
}

/// How many issues back Esc can walk. Each step holds a whole fetched
/// document, so the stack is bounded by count rather than left to grow with
/// the session. Deep enough that a real browse never reaches it.
pub(super) const DETAIL_STACK_MAX: usize = 32;

/// The daemon's `linear.pr` answer for one pull request, by URL.
#[derive(Debug, Clone, PartialEq)]
pub enum PrRead {
    Loading,
    Read(Box<board_core::protocol::LinearPrDocument>),
    /// `gh` could not run at all (protocol code 8), with the daemon's words.
    GhUnavailable(String),
    Failed(String),
}

/// The issue pinned beside the board: a second slot with its own read, so
/// a modal opened over the board leaves it loaded.
#[derive(Debug, Clone, PartialEq)]
pub struct PinnedIssue {
    pub issue: String,
    pub doc: Option<LinearIssueDocument>,
    /// The read in flight for it, numbered from the same counter as the open
    /// issue's, so the generation alone says which slot an answer is for.
    pub in_flight: Option<u64>,
    pub error: Option<DetailError>,
    pub seed: Option<LinearLinkedIssue>,
    pub selection: Option<crate::view::IssueRowKind>,
    pub stack: Vec<DetailStep>,
    pub marks: Vec<Mark>,
    pub scroll: usize,
    pub follow: bool,
}

/// One step of the issue page's history.
#[derive(Debug, Clone, PartialEq)]
pub struct DetailStep {
    pub issue: String,
    pub doc: Option<LinearIssueDocument>,
    pub seed: Option<LinearLinkedIssue>,
    pub selection: Option<crate::view::IssueRowKind>,
}

/// A failed issue read, as the page says it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailError {
    pub issue: String,
    pub message: String,
    /// False when the plugin ships no issue script: the remedy is to update the
    /// plugin, so offering `r` would retry something that cannot succeed.
    pub retryable: bool,
}

impl LinearState {
    pub fn new(
        workspace_id: String,
        board_version: String,
        daemon_version: Option<String>,
    ) -> LinearState {
        LinearState {
            workspace_id,
            board_version,
            daemon_version,
            last_good: None,
            sel_tab: 0,
            sel_group: 0,
            focus: Focus::default(),
            page_start: 0,
            bar_from: 0,
            bar_at: 0,
            bar_vanished: false,
            bar_return: None,
            filter: String::new(),
            filter_chip: None,
            folds: Default::default(),
            last_card: None,
            tab_cursors: Default::default(),
            board_scroll: Default::default(),
            density: Density::default(),
            detail: None,
            in_flight: false,
            queued: false,
            queued_force: false,
            error: None,
            stale_daemon: None,
            fetched_at: None,
            lists_in_flight: Default::default(),
            pick: None,
            herdr_keys: Vec::new(),
            theme: crate::theme::Theme::default(),
            spaces: SpaceList::NotRead,
            strip: StripView::Spaces,
            strip_focus: false,
            strip_sel: 0,
            bind_space: None,
            detail_doc: None,
            detail_in_flight: None,
            detail_reads: 0,
            detail_seed: None,
            detail_error: None,
            detail_selection: None,
            detail_stack: vec![],
            local: None,
            local_in_flight: false,
            local_queued: false,
            detail_marks: None,
            detail_scroll: 0,
            detail_follow: true,
            issue_mode: IssueMode::default(),
            opened_from: None,
            pinned: None,
            prs: HashMap::new(),
            prs_in_flight: HashSet::new(),
            prs_queued: HashSet::new(),
            pr_read_missing: false,
            landing: None,
            level: None,
        }
    }

    /// The rows the strip lists: every read space whose state is not `bound`.
    /// A space whose own snapshot says it is not bound leads the list even
    /// when the space read failed, lags, or calls it bound, so the not-bound
    /// screen can always start a bind for it.
    pub fn unbound_spaces(&self) -> Vec<LinearSpaceRow> {
        let listed: &[LinearSpaceRow] = match &self.spaces {
            SpaceList::Read(list) => &list.rows,
            _ => &[],
        };
        let mut rows: Vec<LinearSpaceRow> = listed
            .iter()
            .filter(|r| r.state != "bound")
            .cloned()
            .collect();
        if let Some(snapshot) = self.snapshot().filter(|_| !self.bound()) {
            let current = listed
                .iter()
                .find(|r| r.id == self.workspace_id)
                .cloned()
                .unwrap_or_else(|| LinearSpaceRow {
                    id: self.workspace_id.clone(),
                    label: snapshot.workspace.label.clone(),
                    state: snapshot.record.state.clone().unwrap_or_default(),
                    ..LinearSpaceRow::default()
                });
            rows.retain(|r| r.id != self.workspace_id);
            rows.insert(0, current);
        }
        rows
    }

    pub(super) fn clamp_strip(&mut self) {
        let n = self.unbound_spaces().len();
        self.strip_sel = self.strip_sel.min(n.saturating_sub(1));
        if n == 0 {
            self.strip_focus = false;
        }
    }

    pub fn list_in_flight(&self, kind: LinearListKind, id: Option<&str>) -> bool {
        self.lists_in_flight
            .iter()
            .any(|(k, i)| *k == kind && i.as_deref() == id)
    }

    pub fn marks_for<'a>(&'a self, identifier: &'a str) -> impl Iterator<Item = &'a Mark> + 'a {
        self.local
            .iter()
            .flat_map(|l| &l.marks)
            .filter(move |m| m.issue == identifier)
    }

    pub fn notes_for<'a>(&'a self, identifier: &'a str) -> impl Iterator<Item = &'a Note> + 'a {
        self.local
            .iter()
            .flat_map(|l| &l.notes)
            .filter(move |n| n.issue == identifier)
    }

    /// Pending requests only: the daemon filters expired ones on its own clock.
    pub fn show_requests_for<'a>(
        &'a self,
        identifier: &'a str,
    ) -> impl Iterator<Item = &'a ShowRequest> + 'a {
        self.local
            .iter()
            .flat_map(|l| &l.show_requests)
            .filter(move |r| r.issue == identifier)
    }

    /// Every pending show-request, oldest first: the bar's order.
    pub fn pending_requests(&self) -> Vec<&ShowRequest> {
        let mut requests: Vec<&ShowRequest> =
            self.local.iter().flat_map(|l| &l.show_requests).collect();
        requests.sort_by(|a, b| (&a.created_at, a.id).cmp(&(&b.created_at, b.id)));
        requests
    }

    pub fn latest_note(&self, identifier: &str) -> Option<&Note> {
        self.overlay(identifier).latest_note
    }

    fn requested(&self, identifier: &str) -> bool {
        self.show_requests_for(identifier).next().is_some()
    }

    /// The gutter left of a card's identifier; see [`CardOverlay::gutter`].
    pub fn gutter(&self, identifier: &str) -> String {
        self.overlay(identifier).gutter()
    }

    /// See [`CardOverlay::needs_attention`].
    pub fn needs_attention(&self, identifier: &str) -> bool {
        self.overlay(identifier).needs_attention()
    }

    fn overlay(&self, identifier: &str) -> CardOverlay<'_> {
        let mut overlay = CardOverlay::default();
        if let Some(local) = &self.local {
            local
                .marks
                .iter()
                .filter(|m| m.issue == identifier)
                .for_each(|m| overlay.add_mark(m.kind));
            overlay.requested |= local.show_requests.iter().any(|r| r.issue == identifier);
            local
                .notes
                .iter()
                .filter(|n| n.issue == identifier)
                .for_each(|n| overlay.add_note(n));
        }
        overlay
    }

    /// Every card's overlay in one pass over the local state, for a draw that
    /// asks about many cards.
    pub fn overlays(&self) -> CardOverlays<'_> {
        let mut map: HashMap<&str, CardOverlay<'_>> = HashMap::new();
        if let Some(local) = &self.local {
            for mark in &local.marks {
                map.entry(&mark.issue).or_default().add_mark(mark.kind);
            }
            for request in &local.show_requests {
                map.entry(&request.issue).or_default().requested = true;
            }
            for note in &local.notes {
                map.entry(&note.issue).or_default().add_note(note);
            }
        }
        CardOverlays(map)
    }

    /// What `n` and `N` stop on: any mark or badge, done included.
    pub fn walkable(&self, identifier: &str) -> bool {
        self.marks_for(identifier).next().is_some() || self.requested(identifier)
    }

    /// The selected card's oldest suggestion, which `a` and `x` act on first.
    pub fn selected_suggestion(&self) -> Option<&Mark> {
        let identifier = self.selected_identifier()?;
        self.marks_for(identifier)
            .filter(|m| m.kind == MarkKind::Suggestion)
            .min_by_key(|m| m.id)
    }

    /// The marks the open issue page lists.
    pub fn detail_marks(&self) -> &[Mark] {
        match (&self.detail_marks, &self.detail) {
            (Some((issue, marks)), Some(open)) if issue == open => marks,
            _ => &[],
        }
    }

    /// A write the daemon confirmed, applied without waiting for the read
    /// its announcement will trigger.
    fn forget_marks(&mut self, ids: &[i64]) {
        if let Some(local) = self.local.as_mut() {
            local.marks.retain(|m| !ids.contains(&m.id));
        }
    }

    fn forget_request(&mut self, id: i64) {
        if let Some(local) = self.local.as_mut() {
            local.show_requests.retain(|r| r.id != id);
        }
    }

    pub fn snapshot(&self) -> Option<&LinearSnapshot> {
        self.last_good.as_ref()
    }

    /// Only a `bound` record reaches Linear; every other record state (and
    /// an unreadable record, whose state is null) is the not-bound screen.
    pub fn bound(&self) -> bool {
        self.snapshot()
            .is_some_and(|s| s.record.state.as_deref() == Some("bound"))
    }

    pub fn issue(&self, identifier: &str) -> Option<&LinearIssue> {
        self.snapshot()?.issues.get(identifier)
    }

    pub fn selected_issue(&self) -> Option<&LinearIssue> {
        self.issue(self.selected_identifier()?)
    }

    pub fn detail_issue(&self) -> Option<&LinearIssue> {
        self.issue(self.detail.as_deref()?)
    }

    /// The project the space's record binds, when the space is bound.
    pub fn bound_project_id(&self) -> Option<&str> {
        if !self.bound() {
            return None;
        }
        self.snapshot()?.record.project_id.as_deref()
    }

    /// The binding `b` and `y` act on: the one the selected pane belongs to,
    /// and otherwise the issue's first. A selection on an issue row is not a
    /// binding, so those keys act on the page's own issue.
    pub fn detail_binding(&self) -> Option<&LinearBinding> {
        let binding = match &self.detail_selection {
            Some(crate::view::IssueRowKind::Pane(index)) => self
                .detail_pane_rows()
                .get(*index)
                .map_or(0, |row| row.binding),
            _ => 0,
        };
        self.detail_issue()?.bindings.get(binding)
    }

    pub fn pane_status(&self, pane_id: &str) -> &str {
        self.snapshot()
            .and_then(|s| s.pane_status.get(pane_id))
            .map(String::as_str)
            .unwrap_or("unknown")
    }

    /// Where the selection sits in `rows`, or the first row when the selected
    /// one is not on the page (it was in a section this issue does not have).
    pub fn selected_row(
        rows: &[crate::view::IssueRow],
        selection: Option<&crate::view::IssueRowKind>,
    ) -> usize {
        selection
            .and_then(|kind| rows.iter().position(|row| &row.kind == kind))
            .unwrap_or(0)
    }

    /// What is selected, falling back to the first row so a page always has
    /// something under the cursor.
    pub fn selected_kind(
        rows: &[crate::view::IssueRow],
        selection: Option<&crate::view::IssueRowKind>,
    ) -> Option<crate::view::IssueRowKind> {
        rows.get(Self::selected_row(rows, selection))
            .map(|row| row.kind.clone())
    }

    pub fn pane_count(issue: &LinearIssue) -> usize {
        issue.bindings.iter().map(|b| b.panes.len()).sum()
    }

    /// Every pane of `issue`'s bindings, in document order.
    pub fn pane_rows(&self, issue: &LinearIssue) -> Vec<PaneRow> {
        issue
            .bindings
            .iter()
            .enumerate()
            .flat_map(|(binding, b)| {
                b.panes.iter().map(move |pane_id| PaneRow {
                    binding,
                    pane_id: pane_id.clone(),
                    status: self.pane_status(pane_id).to_string(),
                })
            })
            .collect()
    }

    pub fn detail_pane_rows(&self) -> Vec<PaneRow> {
        self.detail_issue()
            .map(|issue| self.pane_rows(issue))
            .unwrap_or_default()
    }

    /// The mapping the snapshot reports, when it is not the default.
    pub fn non_default_mapping(&self) -> Option<String> {
        let mapping = &self.snapshot()?.mapping;
        // A document with no mapping section has nothing to report.
        if mapping.source.is_empty() {
            return None;
        }
        let default = ("default", "project", "work", "session");
        let actual = (
            mapping.source.as_str(),
            mapping.space.as_str(),
            mapping.tab.as_str(),
            mapping.pane.as_str(),
        );
        (actual != default).then(|| {
            format!(
                "{}: space={} tab={} pane={}",
                mapping.source, mapping.space, mapping.tab, mapping.pane
            )
        })
    }

    /// One line per source whose status is not `ok`.
    pub fn source_warnings(&self) -> Vec<String> {
        let Some(s) = self.snapshot() else {
            return vec![];
        };
        let mut out = Vec::new();
        match s.linear.status.as_str() {
            "ok" | "unknown" => {}
            "unavailable" => out.push("Linear unavailable".to_string()),
            "truncated" => out.push("Linear listing truncated".to_string()),
            // The not-bound screen prints the daemon's message for it instead.
            "not_imported" => {}
            other => out.push(format!("Linear {other}")),
        }
        // An empty status is a section the document left out, not a failure.
        match s.herdr.status.as_str() {
            "ok" | "" => {}
            "unavailable" => out.push("herdr unavailable".to_string()),
            other => out.push(format!("herdr {other}")),
        }
        if s.record.status == "unreadable" {
            out.push("record unreadable".to_string());
        }
        match s.view.status.as_str() {
            "ok" | "none" => {}
            "unsupported_grouping" => out.push(format!(
                "view unsupported grouping {}",
                s.view
                    .layout
                    .as_ref()
                    .map(|l| l.grouping.as_str())
                    .unwrap_or("?")
            )),
            other => out.push(format!("view {}", other.replace('_', " "))),
        }
        if !matches!(s.mapping.status.as_str(), "ok" | "") {
            out.push(format!("mapping {}", s.mapping.status));
        }
        out
    }

    pub(super) fn clamp(&mut self) {
        self.settle();
        // Only close the page for an issue the reader reached FROM the board:
        // a linked issue is deliberately off-board, and closing it on the next
        // refresh would drop the reader back mid-read.
        if self.detail_seed.is_none()
            && self
                .detail
                .as_deref()
                .is_some_and(|id| self.issue(id).is_none())
        {
            self.detail = None;
            self.detail_marks = None;
        }
        // A selection on a pane that the refresh removed falls back to the
        // first row rather than pointing at a pane that is no longer there.
        let panes = self.detail_issue().map_or(0, Self::pane_count);
        if matches!(self.detail_selection, Some(crate::view::IssueRowKind::Pane(i)) if i >= panes) {
            self.detail_selection = None;
        }
    }

    /// A newly opened issue: a window, scrolled to the top, following its
    /// selected row.
    pub(super) fn open_fresh(&mut self) {
        self.opened_from = None;
        self.issue_mode = IssueMode::Window;
        self.detail_scroll = 0;
        self.detail_follow = true;
    }

    pub(super) fn home_screen(&self) -> Screen {
        if !self.bound() {
            Screen::LinearNotBound
        } else if self.detail.is_some() {
            Screen::LinearDetail
        } else {
            Screen::LinearBoard
        }
    }
}

/// Linear's own progression, so a board reads left to right from the work not
/// started to the work finished. A column the plugin gave no state type keeps
/// its place among its equals, because the sort is stable.
fn state_kind_rank(kind: Option<&str>) -> u8 {
    match kind {
        Some("triage") => 0,
        Some("backlog") => 1,
        Some("unstarted") => 2,
        Some("started") => 3,
        Some("completed") => 4,
        Some("canceled") => 5,
        _ => 6,
    }
}

/// A document from before `tabs` becomes one unlabelled tab of its `groups`,
/// and `groups` stays equal to the first tab, as the daemon sends it.
pub(super) fn order_tabs(snapshot: &mut LinearSnapshot) {
    if snapshot.tabs.is_empty() && !snapshot.groups.is_empty() {
        snapshot.tabs = vec![LinearTab {
            groups: std::mem::take(&mut snapshot.groups),
            ..LinearTab::default()
        }];
    }
    for tab in &mut snapshot.tabs {
        tab.groups
            .sort_by_key(|g| (g.issues.is_empty(), state_kind_rank(g.kind.as_deref())));
    }
    snapshot.groups = snapshot
        .tabs
        .first()
        .map(|t| t.groups.clone())
        .unwrap_or_default();
}

pub(super) fn update_linear(app: &mut App, msg: Msg) -> Vec<Effect> {
    match msg {
        // `board_changed` never refreshes a Linear board.
        Msg::Refresh => vec![],
        Msg::Mouse(m) => {
            let mut effects = super::mouse::on_mouse(app, m);
            effects.extend(super::linear_level::settle(app));
            effects
        }
        Msg::LinearRefresh => {
            let mut effects = request_or_queue(app);
            effects.extend(super::linear_level::request(app));
            effects
        }
        Msg::PaneLayoutChanged(workspaces) => super::linear_level::layout_changed(app, &workspaces),
        Msg::LinearArrived(arrival) => match *arrival {
            LinearArrival::Snapshot(result) => arrived(app, *result),
            LinearArrival::List { kind, id, result } => {
                super::linear_picker::list_arrived(app, kind, id, result);
                vec![]
            }
            LinearArrival::Issue {
                issue,
                generation,
                result,
            } => issue_arrived(app, &issue, generation, *result),
            LinearArrival::State(result) => local_state_arrived(app, *result),
            LinearArrival::Wrote { write, result } => write_arrived(app, write, result),
            LinearArrival::Session(result) => super::linear_level::start_arrived(app, *result),
            LinearArrival::PaneContext(result) => super::linear_level::arrived(app, *result),
            LinearArrival::Pr { url, result } => super::linear_issue::pr_arrived(app, url, *result),
        },
        Msg::Key(k) => {
            let mut effects = linear_key(app, k);
            effects.extend(super::linear_level::settle(app));
            effects
        }
        Msg::LocalStateChanged(signals) => {
            let mut effects = local_state_changed(app, &signals);
            let ours = app
                .linear
                .as_ref()
                .is_some_and(|s| signals.for_space(&s.workspace_id).is_some());
            if ours {
                effects.extend(super::linear_level::request(app));
            }
            effects
        }
    }
}

/// Start the issue-page read for `issue`, unless one is already in flight for
/// it. The page is already on screen by now, showing what the snapshot holds;
/// this fills in the rest.
pub(super) fn request_issue(app: &mut App, issue: &str) -> Vec<Effect> {
    let Some(state) = app.linear.as_mut() else {
        return vec![];
    };
    if state
        .detail_in_flight
        .as_ref()
        .is_some_and(|(open, _)| open == issue)
    {
        return vec![];
    }
    state.detail_reads += 1;
    let generation = state.detail_reads;
    state.detail_in_flight = Some((issue.to_string(), generation));
    state.detail_error = None;
    // The document for a DIFFERENT issue is dropped here rather than left on
    // screen under the new issue's title.
    if state.detail_doc.as_ref().is_some_and(|(id, _)| id != issue) {
        state.detail_doc = None;
    }
    vec![Effect::LinearIssue {
        issue: issue.to_string(),
        generation,
    }]
}

/// A read that lands after the reader has opened another issue is
/// dropped. Overlapping reads are the normal case once Enter opens a linked
/// issue in place, so this is the rule, not an edge case.
pub(super) fn issue_arrived(
    app: &mut App,
    issue: &str,
    generation: u64,
    result: Result<LinearIssueDocument, LinearFailure>,
) -> Vec<Effect> {
    let Some(state) = app.linear.as_mut() else {
        return vec![];
    };
    if let Some(effects) = super::linear_issue::pinned_arrived(state, issue, generation, &result) {
        return effects;
    }
    let current = state
        .detail_in_flight
        .as_ref()
        .is_some_and(|(_, open)| *open == generation);
    if state.detail.as_deref() != Some(issue) {
        // Not the page on screen. Clear the in-flight marker only when it is
        // this read's, so a newer read for the open issue keeps its own.
        if current {
            state.detail_in_flight = None;
        }
        return vec![];
    }
    if !current {
        // The open issue, but a read it has already superseded: Esc back to an
        // issue whose first read was still running starts a second read for it,
        // and the first can land after. Applying it would paint an older copy
        // over a newer one. The in-flight marker stays, because the read it
        // names is still running.
        return vec![];
    }
    state.detail_in_flight = None;
    match read_outcome(issue, result) {
        Ok(document) => {
            state.detail_error = None;
            let effects = super::linear_issue::request_prs(state, &document);
            state.detail_doc = Some((issue.to_string(), document));
            effects
        }
        Err(error) => {
            state.detail_error = Some(error);
            vec![]
        }
    }
}

/// An issue read as the page shows it: a sanitised document, or the error
/// the page draws instead.
pub(super) fn read_outcome(
    issue: &str,
    result: Result<LinearIssueDocument, LinearFailure>,
) -> Result<LinearIssueDocument, DetailError> {
    match result {
        // The plugin carries a reachability failure IN the document -
        // `status: "unavailable"` with no issue - because the read ran and
        // answered. The page still has to say so: without this it renders an
        // issue with no description and no activity, which reads as an empty
        // issue rather than as Linear being unreachable.
        Ok(document) if document.issue.is_none() => {
            let message = document
                .message
                .filter(|m| !m.trim().is_empty())
                .unwrap_or_else(|| "this issue could not be read".to_string());
            Err(DetailError {
                issue: issue.to_string(),
                message: sanitise(&message),
                retryable: true,
            })
        }
        Ok(document) => Ok(sanitise_issue(document)),
        Err(failure) => Err(detail_failure(issue, failure)),
    }
}

/// The same walk over an issue document: descriptions, comments and
/// attachment fields are outside text.
pub(crate) fn sanitise_issue(document: LinearIssueDocument) -> LinearIssueDocument {
    board_core::text::scrubbed(&document).unwrap_or_default()
}

pub(super) fn detail_failure(issue: &str, failure: LinearFailure) -> DetailError {
    let (message, retryable) = match failure {
        // The plugin is installed and current but has no issue script: `r`
        // would retry something that cannot succeed, so the page says what to
        // do instead.
        LinearFailure::OpUnsupported(_) => (
            "this issue page needs a newer work plugin; update it to read the issue".to_string(),
            false,
        ),
        LinearFailure::MethodNotFound => (
            "this board's daemon predates the issue read; restart the daemon".to_string(),
            false,
        ),
        LinearFailure::TimedOut(limit) => (read_timeout_text(limit), true),
        LinearFailure::Failed(text) | LinearFailure::GhUnavailable(text) => (sanitise(&text), true),
    };
    DetailError {
        issue: issue.to_string(),
        message,
        retryable,
    }
}

fn request_snapshot(app: &mut App, force: bool) -> Vec<Effect> {
    let Some(in_flight) = app.linear.as_ref().map(|s| s.in_flight) else {
        return vec![];
    };
    if in_flight {
        app.set_toast("refresh already in flight", false);
        return vec![];
    }
    let Some(state) = app.linear.as_mut() else {
        return vec![];
    };
    state.in_flight = true;
    let mut effects = vec![Effect::LinearSnapshot { force }];
    // The strip's space list is read with every snapshot.
    if state.lists_in_flight.insert((LinearListKind::Spaces, None)) {
        effects.push(Effect::LinearList {
            kind: LinearListKind::Spaces,
            id: None,
        });
    }
    effects
}

/// An automatic refresh: sent now, or queued behind the one in flight.
pub(super) fn request_or_queue(app: &mut App) -> Vec<Effect> {
    match app.linear.as_mut() {
        Some(state) if state.in_flight => {
            state.queued = true;
            vec![]
        }
        Some(_) => request_snapshot(app, false),
        None => vec![],
    }
}

/// `R`: a read past the daemon's cache. Pressed during a read, it is
/// queued behind it rather than dropped, since the read in flight may be the
/// cached copy `R` is there to skip.
fn force_refresh(app: &mut App) -> Vec<Effect> {
    match app.linear.as_mut() {
        Some(state) if state.in_flight => {
            state.queued = true;
            state.queued_force = true;
            vec![]
        }
        Some(_) => request_snapshot(app, true),
        None => vec![],
    }
}

fn local_state_changed(app: &mut App, signals: &LocalStateSignals) -> Vec<Effect> {
    let Some(state) = app.linear.as_ref() else {
        return vec![];
    };
    match signals.for_space(&state.workspace_id) {
        None => vec![],
        // The daemon already refetched Linear, so the snapshot read is never
        // forced. The state is read too: the daemon sends no separate signal
        // for a mark written alongside a refetch.
        // A background `gh` pass also arrives as `snapshot: true`, and only
        // a PR read brings its result to the open or pinned issue's cards.
        Some(true) => {
            let mut effects = request_or_queue(app);
            effects.extend(request_local_state(app));
            if let Some(state) = app.linear.as_mut() {
                effects.extend(super::linear_issue::refresh_prs(state));
            }
            effects
        }
        Some(false) => request_local_state(app),
    }
}

pub(super) fn request_local_state(app: &mut App) -> Vec<Effect> {
    let Some(state) = app.linear.as_mut() else {
        return vec![];
    };
    if state.local_in_flight {
        state.local_queued = true;
        return vec![];
    }
    state.local_in_flight = true;
    vec![Effect::LinearStateGet]
}

/// A failed read keeps the last local state and says nothing: it is a
/// background read, and a daemon that predates it must not cost the board
/// its screen.
fn local_state_arrived(app: &mut App, result: Result<LocalState, LinearFailure>) -> Vec<Effect> {
    let width = app.last_area.width;
    let Some(state) = app.linear.as_mut() else {
        return vec![];
    };
    state.local_in_flight = false;
    let follow_up = std::mem::take(&mut state.local_queued);
    let mut bindings_changed = false;
    if let Ok(local) = result {
        let local = sanitise_local(local);
        // The first read has nothing to compare with; the snapshot on screen
        // was read with whatever bindings existed then.
        bindings_changed = state.local.as_ref().is_some_and(|last| {
            last.space_bindings != local.space_bindings
                || last.worktree_bindings != local.worktree_bindings
        });
        state.local = Some(local);
        state.refind_bar(width);
    }
    let mut effects = vec![];
    if bindings_changed {
        effects.extend(request_or_queue(app));
    }
    if follow_up {
        effects.extend(request_local_state(app));
    }
    effects
}

fn arrived(app: &mut App, result: Result<LinearSnapshot, LinearFailure>) -> Vec<Effect> {
    let now = app.now;
    let Some(state) = app.linear.as_mut() else {
        return vec![];
    };
    state.in_flight = false;
    let follow_up = std::mem::take(&mut state.queued);
    let follow_up_force = std::mem::take(&mut state.queued_force);
    let mut effects = vec![];
    let mut read_local = false;
    let mut landing = None;
    let screen = match result {
        Ok(mut snapshot) => {
            sanitise_snapshot(&mut snapshot);
            order_tabs(&mut snapshot);
            // A board started beside an agent keeps the label its opener gave
            // it: `scripts/open-board.sh` takes any `Linear: …` pane for the
            // board and would toggle the agent's side pane instead.
            let beside = state.level.as_ref().is_some_and(|l| l.start.is_some());
            if !beside {
                effects.push(Effect::SetLinearPaneTitle(crate::view::linear_pane_title(
                    &snapshot,
                    &state.workspace_id,
                )));
            }
            state.error = None;
            state.stale_daemon = None;
            state.fetched_at = Some(now);
            // Events only announce changes; what was already there when the
            // board opened is read once, here.
            if state.last_good.is_none() {
                read_local = true;
                landing = state.landing.take();
            }
            // Taken before the swap: the keys come from the document the
            // cursor was placed in.
            let cursor = state.cursor();
            state.last_good = Some(snapshot);
            state.refind(&cursor);
            state.refit(app.last_area.width);
            state.refind_bar(app.last_area.width);
            state.clamp_strip();
            state.home_screen()
        }
        Err(LinearFailure::MethodNotFound) => {
            state.stale_daemon = Some((state.board_version.clone(), state.daemon_version.clone()));
            Screen::LinearStaleDaemon
        }
        Err(LinearFailure::TimedOut(limit)) => {
            state.error = Some(read_timeout_text(limit));
            Screen::LinearError
        }
        Err(LinearFailure::OpUnsupported(text))
        | Err(LinearFailure::Failed(text))
        | Err(LinearFailure::GhUnavailable(text)) => {
            state.error = Some(sanitise(&text));
            Screen::LinearError
        }
    };
    // An overlay open when a snapshot lands stays open: the new home
    // screen becomes where closing it goes. The split level keeps its issue
    // on screen; leaving it picks the screen again.
    let at_split = app.linear.as_ref().is_some_and(LinearState::at_split);
    if at_split {
        landing = None;
    }
    match app.screen {
        _ if at_split => {}
        Screen::LinearPicker if app.picker.is_some() => {
            if let Some(picker) = app.picker.as_mut() {
                picker.return_to = screen;
            }
        }
        Screen::Help => app.help_return_to = screen,
        _ => app.screen = screen,
    }
    // After the screen is set, or the board screen would replace a detail
    // the landing opened.
    if let Some(show) = landing {
        effects.extend(land(app, &show));
    }
    if read_local {
        effects.extend(request_local_state(app));
    }
    if follow_up {
        effects.extend(request_snapshot(app, follow_up_force));
    }
    effects
}

/// The first target the view can place, selected like an accepted
/// request and never clearing marks. Only a bound board has cards to land on.
fn land(app: &mut App, show: &crate::ShowContext) -> Vec<Effect> {
    if !app.linear.as_ref().is_some_and(LinearState::bound) {
        return vec![];
    }
    for target in show.targets() {
        match target {
            // A card id names a card of the kanban board; a Linear snapshot
            // carries none, so here it places nothing and the issue stands in.
            crate::Landing::Card(_) => {}
            crate::Landing::Issue(issue) => return show_target(app, &issue),
        }
    }
    vec![]
}

/// A confirmed write is applied at once; a write someone else got to first
/// re-reads the state it was drawn from.
fn write_arrived(
    app: &mut App,
    write: LinearWrite,
    result: Result<(), WriteFailure>,
) -> Vec<Effect> {
    let width = app.last_area.width;
    let Some(state) = app.linear.as_mut() else {
        return vec![];
    };
    match (write, result) {
        (LinearWrite::ClearMarks { ids, .. }, Ok(())) => {
            state.forget_marks(&ids);
            vec![]
        }
        // The marks stay and the next open retries.
        (LinearWrite::ClearMarks { on_open: true, .. }, Err(_)) => vec![],
        (LinearWrite::ClearMarks { .. }, Err(WriteFailure::Gone(_))) => request_local_state(app),
        (LinearWrite::ClearMarks { .. }, Err(WriteFailure::Failed(text))) => {
            app.set_toast(format!("dismiss failed: {}", sanitise(&text)), true);
            vec![]
        }
        (LinearWrite::AcceptShow { id, issue }, Ok(())) => {
            state.forget_request(id);
            state.refind_bar(width);
            show_target(app, &issue)
        }
        (LinearWrite::DismissShow { id }, Ok(())) => {
            state.forget_request(id);
            state.refind_bar(width);
            vec![]
        }
        (
            LinearWrite::AcceptShow { .. } | LinearWrite::DismissShow { .. },
            Err(WriteFailure::Gone(_)),
        ) => {
            app.set_toast("request no longer pending", false);
            request_local_state(app)
        }
        (
            LinearWrite::AcceptShow { .. } | LinearWrite::DismissShow { .. },
            Err(WriteFailure::Failed(text)),
        ) => {
            app.set_toast(format!("request failed: {}", sanitise(&text)), true);
            vec![]
        }
        // The daemon clears a suggestion only when its `worktree_path` is the
        // bound worktree; one reported from a bare cwd names none, so the
        // accepted mark is cleared by id as well (a mark already gone is
        // skipped). The read after it moves the card through its binding.
        (LinearWrite::Bind { mark, issue }, Ok(())) => {
            app.set_toast(format!("bound {issue}"), false);
            let mut effects: Vec<Effect> = mark
                .map(|id| Effect::LinearMarkClear {
                    ids: vec![id],
                    on_open: false,
                })
                .into_iter()
                .collect();
            effects.extend(request_local_state(app));
            effects
        }
        (LinearWrite::SpaceBind { space }, Ok(())) => {
            super::linear_picker::space_bound(app, &space)
        }
        (
            LinearWrite::Bind { .. } | LinearWrite::SpaceBind { .. },
            Err(WriteFailure::Gone(text) | WriteFailure::Failed(text)),
        ) => {
            app.set_toast(format!("bind failed: {}", sanitise(&text)), true);
            vec![]
        }
    }
}

/// An accepted request's card: selected where it is drawn, switching tab and
/// page, or opened as an issue page when no tab draws it. Neither clears its
/// marks.
fn show_target(app: &mut App, issue: &str) -> Vec<Effect> {
    let Some(state) = app.linear.as_mut() else {
        return vec![];
    };
    state.strip_focus = false;
    if state.select_identifier(issue) {
        return vec![];
    }
    let seed = LinearLinkedIssue {
        identifier: issue.to_string(),
        title: state
            .issue(issue)
            .map(|i| i.title.clone())
            .unwrap_or_default(),
        ..LinearLinkedIssue::default()
    };
    state.detail_marks = Some((issue.to_string(), state.marks_for(issue).cloned().collect()));
    state.open_fresh();
    state.detail = Some(issue.to_string());
    state.detail_stack.clear();
    state.detail_selection = None;
    state.detail_doc = None;
    state.detail_error = None;
    // Without a seed, `clamp` closes a page whose issue the snapshot lacks.
    state.detail_seed = Some(seed);
    app.screen = Screen::LinearDetail;
    request_issue(app, issue)
}

/// `a` (accept) and `x` (reject) on the board: the selected card's oldest
/// suggestion. Show-requests are answered from the bottom bar, not here.
fn answer(app: &mut App, accept: bool) -> Vec<Effect> {
    let Some(state) = app.linear.as_ref() else {
        return vec![];
    };
    if let Some(mark) = state.selected_suggestion().cloned() {
        if !accept {
            return vec![Effect::LinearMarkClear {
                ids: vec![mark.id],
                on_open: false,
            }];
        }
        let Some(cwd) = suggestion_cwd(&mark) else {
            app.set_toast(
                "this suggestion names no worktree to bind; x dismisses it",
                true,
            );
            return vec![];
        };
        return vec![Effect::LinearBind {
            mark: Some(mark.id),
            issue: mark.issue,
            cwd,
        }];
    }
    app.set_toast(
        if accept {
            "nothing to accept"
        } else {
            "nothing to dismiss"
        },
        false,
    );
    vec![]
}

/// `n` / `N`: the next or previous card carrying a mark or badge, in tab,
/// column, then lane order, wrapping.
fn walk(app: &mut App, delta: isize) {
    let Some(state) = app.linear.as_mut() else {
        return;
    };
    match state.next_slot(delta, |id| state.walkable(id)) {
        Some((tab, group, card)) => state.select_slot(tab, group, card),
        None => app.set_toast("no card carries a mark or a request", false),
    }
}

pub(super) fn linear_key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    // Above the chord guard, which drops every Control press.
    if k.code == KeyCode::Char('q') && k.modifiers.contains(KeyModifiers::CONTROL) {
        return vec![Effect::Quit];
    }
    // Same rule as the upstream reducer: a fast `Esc <key>` arrives as
    // `Alt+<key>`, which would otherwise open the browser or focus a pane.
    if k.modifiers.intersects(
        KeyModifiers::CONTROL
            | KeyModifiers::ALT
            | KeyModifiers::META
            | KeyModifiers::SUPER
            | KeyModifiers::HYPER,
    ) {
        return vec![];
    }
    // Before the global keys: in a picker every printable key is filter text,
    // so `r` must not fetch and `?` must not open help.
    if app.screen == Screen::LinearPicker {
        return super::linear_picker::linear_picker_key(app, k);
    }
    // Before the global keys for the same reason: `?`, `r` and `q` are text.
    if app.screen == Screen::LinearBoard
        && app
            .linear
            .as_ref()
            .is_some_and(|s| s.focus == Focus::Filter && !s.strip_focus)
    {
        return filter_key(app, k);
    }
    let issue_bar =
        app.screen == Screen::LinearDetail && app.linear.as_ref().is_some_and(|s| !s.at_split());
    if k.code == KeyCode::Char('?') && (app.screen == Screen::LinearBoard || issue_bar) {
        super::linear_bar::open(app);
        return vec![];
    }
    if k.code == KeyCode::Char('?') && app.screen != Screen::Help {
        app.help_return_to = app.screen;
        app.help_scroll = 0;
        app.screen = Screen::Help;
        return vec![];
    }
    if let (KeyCode::Char(c @ ('r' | 'R')), false) = (k.code, app.screen == Screen::Help) {
        if app.screen == Screen::LinearDetail {
            let retry = app.linear.as_ref().and_then(|state| {
                let open = state.detail.clone()?;
                // Only a retryable failure: an unsupported op or a stale daemon
                // is fixed by updating something, not by asking again.
                state
                    .detail_error
                    .as_ref()
                    .is_some_and(|e| e.retryable && e.issue == open)
                    .then_some(open)
            });
            if let Some(issue) = retry {
                return request_issue(app, &issue);
            }
        }
        return if c == 'R' {
            force_refresh(app)
        } else {
            request_snapshot(app, false)
        };
    }
    if app.screen == Screen::LinearDetail && app.linear.as_ref().is_some_and(|s| s.at_split()) {
        return super::linear_level::split_key(app, k);
    }
    match app.screen {
        Screen::Help => {
            match nav_delta(k.code) {
                Some(delta) => {
                    let max = crate::view::linear_help_max_scroll(app, app.last_area);
                    app.help_scroll = step_clamped(app.help_scroll, delta, max);
                }
                None => app.screen = app.help_return_to,
            }
            vec![]
        }
        Screen::LinearBoard => board_key(app, k),
        Screen::LinearDetail => super::linear_issue::detail_key(app, k),
        Screen::LinearError => {
            if k.code == KeyCode::Esc {
                if let Some(state) = app.linear.as_ref() {
                    if state.last_good.is_some() {
                        app.screen = state.home_screen();
                    }
                }
            }
            vec![]
        }
        Screen::LinearNotBound => match strip_owned_key(app, k) {
            Some(effects) => effects,
            None if k.code == KeyCode::Esc => vec![Effect::Quit],
            None => vec![],
        },
        Screen::LinearStaleDaemon => match k.code {
            KeyCode::Esc => vec![Effect::Quit],
            _ => vec![],
        },
        _ => vec![],
    }
}

/// The keys the board and the not-bound screen share: the strip toggles, the
/// view picker, and every key while the strip has focus. `None` leaves the key
/// to the screen.
fn strip_owned_key(app: &mut App, k: KeyEvent) -> Option<Vec<Effect>> {
    let state = app.linear.as_mut()?;
    match k.code {
        KeyCode::Char('t') => {
            state.strip = match state.strip {
                StripView::Spaces => StripView::Tabs,
                StripView::Tabs => StripView::Spaces,
            };
            state.strip_focus = false;
            return Some(vec![]);
        }
        KeyCode::Char('s') => {
            state.strip = StripView::Spaces;
            state.strip_focus = !state.unbound_spaces().is_empty();
            return Some(vec![]);
        }
        KeyCode::Char('v') => {
            state.strip_focus = false;
            return Some(open_view_picker(app));
        }
        _ => {}
    }
    // Ahead of the screen's own arms: Escape here unfocuses the strip rather
    // than quitting, and up/down move the strip rather than the card.
    state.strip_focus.then(|| strip_key(app, k))
}

fn board_key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    if let Some(effects) = strip_owned_key(app, k) {
        return effects;
    }
    if let Some(effects) = super::linear_bar::bar_key(app, k) {
        return effects;
    }
    if let Some(effects) = super::linear_issue::pinned_key(app, k) {
        return effects;
    }
    let Some(state) = app.linear.as_mut() else {
        return vec![];
    };
    if state.focus == Focus::None
        && matches!(
            k.code,
            KeyCode::Up
                | KeyCode::Down
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::Tab
                | KeyCode::BackTab
                | KeyCode::Enter
                | KeyCode::Char('h' | 'j' | 'k' | 'l')
        )
    {
        state.reselect();
        return vec![];
    }
    let width = app.last_area.width;
    // The stored page may predate a resize or a jump the draw already
    // followed; moves below start from the page on screen.
    state.refit(width);
    if let Focus::Pager(..) = state.focus {
        match k.code {
            KeyCode::Enter | KeyCode::Esc => return pager_left(state, width),
            _ if nav_delta(k.code).is_some() => return pager_left(state, width),
            KeyCode::Left
            | KeyCode::Right
            | KeyCode::Char('h' | 'l' | '<' | '>')
            | KeyCode::Tab
            | KeyCode::BackTab => {}
            _ => state.leave_pager(),
        }
    }
    if let Some(delta) = nav_delta(k.code) {
        state.step(delta, false);
        return vec![];
    }
    let effects = board_action(app, k);
    if let Some(state) = app.linear.as_mut() {
        state.refit(width);
    }
    effects
}

fn pager_left(state: &mut LinearState, width: u16) -> Vec<Effect> {
    state.leave_pager();
    state.refit(width);
    vec![]
}

fn board_action(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    let width = app.last_area.width;
    let Some(state) = app.linear.as_mut() else {
        return vec![];
    };
    match k.code {
        KeyCode::Char('[') => state.cycle_tab(-1),
        KeyCode::Char(']') => state.cycle_tab(1),
        KeyCode::Char('a') => return answer(app, true),
        KeyCode::Char('x') => return answer(app, false),
        KeyCode::Char('n') => walk(app, 1),
        KeyCode::Char('N') => walk(app, -1),
        KeyCode::Char('<') => state.scroll_page(Side::Left, width),
        KeyCode::Char('>') => state.scroll_page(Side::Right, width),
        KeyCode::Left | KeyCode::Char('h') | KeyCode::BackTab => state.move_column(-1, width),
        KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab => state.move_column(1, width),
        KeyCode::Char('/') => state.open_filter(),
        KeyCode::Char('o') => super::linear_issue::unpin(app),
        KeyCode::Char('p') => super::linear_issue::focus_pinned(app),
        KeyCode::Char('z') => {
            state.density = match state.density {
                Density::Roomy => Density::Compact,
                Density::Compact => Density::Roomy,
            };
        }
        KeyCode::Esc => state.deselect(),
        KeyCode::Enter => {
            if let Some(lane) = state.selected_lane_line().map(str::to_string) {
                state.toggle_fold(&lane);
                return vec![];
            }
            if let Some(id) = state.selected_identifier().map(str::to_string) {
                return open_card(app, &id);
            }
        }
        _ => {}
    }
    vec![]
}

/// Return on a card: its issue page, clearing the marks it shows.
fn open_card(app: &mut App, id: &str) -> Vec<Effect> {
    let Some(state) = app.linear.as_mut() else {
        return vec![];
    };
    state.open_fresh();
    if let Focus::Card(card) = state.focus {
        state.opened_from = Some((id.to_string(), (state.sel_tab, state.sel_group, card)));
    }
    state.detail = Some(id.to_string());
    state.detail_stack.clear();
    state.detail_seed = None;
    // The working pane, as the overlay opened on: getting to the
    // pane working on an issue is why a card gets opened.
    state.detail_selection = state
        .detail_pane_rows()
        .iter()
        .position(|row| row.status == "working")
        .map(crate::view::IssueRowKind::Pane);
    // The marks shown now clear, in one request; one
    // set later stays. A suggestion clears only on `a` or `x`.
    let shown: Vec<Mark> = state.marks_for(id).cloned().collect();
    let ids: Vec<i64> = shown
        .iter()
        .filter(|m| m.kind != MarkKind::Suggestion)
        .map(|m| m.id)
        .collect();
    state.detail_marks = Some((id.to_string(), shown));
    app.screen = Screen::LinearDetail;
    let mut effects = vec![];
    if !ids.is_empty() {
        effects.push(Effect::LinearMarkClear { ids, on_open: true });
    }
    effects.extend(request_issue(app, id));
    effects
}

/// Keys while the filter is selected: printable keys are filter text.
fn filter_key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    let width = app.last_area.width;
    let Some(state) = app.linear.as_mut() else {
        return vec![];
    };
    match k.code {
        KeyCode::Esc if state.filtering() => state.leave_filter_cleared(width),
        KeyCode::Esc => state.deselect(),
        KeyCode::Enter | KeyCode::Tab | KeyCode::Down => state.reselect(),
        KeyCode::Left => state.move_column(-1, width),
        KeyCode::Right => state.move_column(1, width),
        KeyCode::Backspace => {
            if state.filter.pop().is_none() {
                state.filter_chip = None;
            }
            state.clamp();
        }
        KeyCode::Char(c) => {
            state.filter.push(c);
            state.clamp();
        }
        _ => {}
    }
    vec![]
}

/// The view picker for the bound project; a space with no bound project has
/// no views to list, so it says so instead of opening an empty picker.
pub(super) fn open_view_picker(app: &mut App) -> Vec<Effect> {
    let Some(project) = app
        .linear
        .as_ref()
        .and_then(|s| s.bound_project_id())
        .map(str::to_string)
    else {
        app.set_toast("this space has no project to list views for", true);
        return vec![];
    };
    let id = Some(project);
    if super::linear_picker::open_linear_picker(app, LinearListKind::Views, id.clone()) {
        return vec![Effect::LinearList {
            kind: LinearListKind::Views,
            id,
        }];
    }
    vec![]
}

fn strip_key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    let Some(state) = app.linear.as_mut() else {
        return vec![];
    };
    if let Some(delta) = nav_delta(k.code) {
        let max = state.unbound_spaces().len().saturating_sub(1);
        state.strip_sel = step_clamped(state.strip_sel, delta, max);
        return vec![];
    }
    match k.code {
        KeyCode::Esc => state.strip_focus = false,
        KeyCode::Enter => {
            let Some(space) = state.unbound_spaces().into_iter().nth(state.strip_sel) else {
                return vec![];
            };
            state.bind_space = Some(space);
            if super::linear_picker::open_linear_picker(app, LinearListKind::Projects, None) {
                return vec![Effect::LinearList {
                    kind: LinearListKind::Projects,
                    id: None,
                }];
            }
        }
        _ => {}
    }
    vec![]
}

/// The wheel over the board: ↑ / ↓ that never opens the filter.
pub(super) fn wheel(app: &mut App, delta: isize) -> Vec<Effect> {
    let strip = app.linear.as_ref().is_some_and(|s| s.strip_focus);
    let key = if delta < 0 {
        KeyCode::Up
    } else {
        KeyCode::Down
    };
    match app.linear.as_mut() {
        Some(state) if !strip && matches!(state.focus, Focus::Card(_) | Focus::LaneLine(_)) => {
            state.step(delta, true);
            vec![]
        }
        Some(state) if !strip && state.focus == Focus::Filter => vec![],
        _ => linear_key(app, KeyEvent::new(key, KeyModifiers::NONE)),
    }
}

/// A sideways scroll, or Shift with the wheel, over the board: pages toward
/// `side` as `<` / `>` do.
pub(super) fn page_wheel(app: &mut App, side: Side) -> Vec<Effect> {
    let width = app.last_area.width;
    match app.linear.as_mut() {
        Some(state) if !state.strip_focus => state.scroll_page(side, width),
        _ => {}
    }
    vec![]
}

/// Selects the strip row drawn under the click, found again by space id, then
/// opens it the way `Enter` does. A space no longer listed is left alone.
pub(super) fn click_strip_row(app: &mut App, space_id: &str) -> Vec<Effect> {
    let Some(state) = app.linear.as_mut() else {
        return vec![];
    };
    let Some(at) = state
        .unbound_spaces()
        .iter()
        .position(|row| row.id == space_id)
    else {
        return vec![];
    };
    state.strip = StripView::Spaces;
    state.strip_sel = at;
    state.strip_focus = true;
    linear_key(app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
}

/// Selects the card drawn under the click, found again by identifier because
/// the snapshot may have changed since that frame, then opens it the way
/// `Enter` does. A card no longer in the snapshot is left alone.
pub(super) fn click_card(app: &mut App, group: &str, lane: &str, identifier: &str) -> Vec<Effect> {
    let width = app.last_area.width;
    let Some(state) = app.linear.as_mut() else {
        return vec![];
    };
    state.refit(width);
    let Some((sel_group, sel_card)) = state.locate(group, lane, identifier) else {
        return vec![];
    };
    state.sel_group = sel_group;
    state.focus = Focus::Card(sel_card);
    // Otherwise the Enter below reaches the strip, not the card.
    state.strip_focus = false;
    linear_key(app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
}

pub(super) fn click_group(app: &mut App, group: &str) {
    let width = app.last_area.width;
    let Some(state) = app.linear.as_mut() else {
        return;
    };
    state.refit(width);
    if let Some(index) = state.groups().iter().position(|g| g.key == group) {
        state.strip_focus = false;
        state.sel_group = index;
        if matches!(state.focus, Focus::None | Focus::Filter) {
            state.focus = Focus::default();
        }
        state.clamp();
    }
}

// -- sanitising -------------------------------------------------------------

/// The plugin's `HERDR_LINEAR_SANITIZE_JQ_DEF` codepoint set: C0 except tab
/// and newline, DEL, C1, and the invisible format characters. Applied to
/// every document string and to error text before rendering.
pub fn sanitise(s: &str) -> String {
    board_core::text::strip_control_keep_lines(s)
}

/// Every string in the document, keys included, through [`sanitise`]. It walks
/// the serialised form rather than the struct's fields, so a string field
/// added to any Linear type is covered without a line here.
pub fn sanitise_snapshot(snapshot: &mut LinearSnapshot) {
    match board_core::text::scrubbed(&*snapshot) {
        Ok(cleaned) => *snapshot = cleaned,
        // The document round-trips through its own types, so this does not
        // happen; if it did, nothing unsanitised may reach the screen.
        Err(_) => *snapshot = LinearSnapshot::default(),
    }
}

/// The same walk over local state: mark text and note bodies are agent text.
pub(super) fn sanitise_local(local: LocalState) -> LocalState {
    let space = local.space.clone();
    board_core::text::scrubbed(&local).unwrap_or(LocalState {
        space,
        space_bindings: vec![],
        worktree_bindings: vec![],
        grouping: None,
        marks: vec![],
        notes: vec![],
        show_requests: vec![],
        resolved_show_requests: vec![],
    })
}

/// The same walk over a list envelope.
pub fn sanitise_list(result: LinearListResult) -> LinearListResult {
    let kind = result.kind();
    let cleaned = serde_json::to_value(&result)
        .map(board_core::text::sanitise_json)
        .and_then(|value| LinearListResult::from_value(kind, value));
    match cleaned {
        Ok(cleaned) => cleaned,
        // Same reasoning as the snapshot: an unknown-status empty envelope,
        // never the unsanitised one.
        Err(_) => match kind {
            LinearListKind::Spaces => LinearListResult::Spaces(LinearListEnvelope::default()),
            LinearListKind::Projects => LinearListResult::Projects(LinearListEnvelope::default()),
            LinearListKind::Views => LinearListResult::Views(LinearListEnvelope::default()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitise_strips_bidi_override_and_escape_but_keeps_text() {
        let title = "Fix\u{202E}drawer\u{1b}[31m blank\u{FEFF} state\tnow\n";
        assert_eq!(sanitise(title), "Fixdrawer[31m blank state\tnow\n");
    }

    #[test]
    fn sanitise_snapshot_reaches_issue_keys_and_pane_status() {
        let mut snapshot = LinearSnapshot::default();
        snapshot.issues.insert(
            "WEB-1\u{200B}".into(),
            LinearIssue {
                title: "a\u{7f}b".into(),
                ..Default::default()
            },
        );
        snapshot
            .pane_status
            .insert("p\u{00AD}1".into(), "work\u{2066}ing".into());
        sanitise_snapshot(&mut snapshot);
        assert_eq!(snapshot.issues["WEB-1"].title, "ab");
        assert_eq!(snapshot.pane_status["p1"], "working");
    }
}
