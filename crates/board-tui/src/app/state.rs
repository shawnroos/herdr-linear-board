//! The satellite state types `App` holds: the modals (`Picker`, `Confirm`,
//! `SwitcherState`, `CommentHistoryView`), the two mini-modes
//! (`MoveColumnState`, `DragState`), the archive filter, and the toast.
//!
//! Pure data plus the small pure helpers that build it. The reducer that
//! mutates it lives in `app` and its per-screen key handlers; nothing here
//! performs I/O or renders.

use board_core::model::{Column, CommentHistory};
use board_core::protocol::{LinearListKind, LinearListStatus};

use super::Screen;

/// State for the Compact-only column switcher sheet (`Screen::Switcher`): the
/// current board's columns plus a trailing "switch board" row (which opens
/// the board picker) and an "apply template" row.
pub struct SwitcherState {
    pub sel: usize,
    /// Where closing this sheet lands. See [`Screen`]'s `return_to` note.
    pub return_to: Screen,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DetailScrollTarget {
    Comments,
    Runs,
}

/// State for the `Screen::CommentHistory` sheet: one comment's full audit
/// trail, oldest → newest, with its own scroll offset.
pub struct CommentHistoryView {
    pub comment_id: i64,
    pub entries: Vec<CommentHistory>,
    pub scroll: usize,
}

/// Which cards are visible on the board. Archiving never deletes history.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CardFilter {
    Active,
    All,
    Archived,
}

impl CardFilter {
    pub fn next(self) -> Self {
        match self {
            Self::Active => Self::All,
            Self::All => Self::Archived,
            Self::Archived => Self::Active,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Active => "ACTIVE",
            Self::All => "ALL",
            Self::Archived => "ARCHIVED",
        }
    }
}

/// A transient status message.
pub struct Toast {
    pub text: String,
    pub is_error: bool,
    /// Wall-clock second at which it was raised (for expiry in the run loop).
    pub at: i64,
}

/// A project/board picker (switch flows) or a column picker (choose where a
/// deleted column's cards go). Rows are either concrete items (`(label, id)`)
/// or trailing action rows that open a follow-up picker/form.
pub struct Picker {
    pub title: String,
    pub rows: Vec<PickerRow>,
    pub sel: usize,
    pub purpose: PickerPurpose,
    /// Where dismissing this picker lands. See [`Screen`]'s `return_to` note.
    pub return_to: Screen,
    /// The project whose boards a board picker lists; for the project picker
    /// it is the current project. Unused by the delete-column picker.
    pub project_id: i64,
    /// Typed text narrowing the rows. Only a Linear picker takes typing.
    pub filter: String,
    /// The one argument a Linear list needs (a views list's project id).
    pub list_id: Option<String>,
    /// What the last Linear list read said; `None` until one lands.
    pub outcome: Option<ListOutcome>,
}

impl Picker {
    pub fn new(
        title: String,
        rows: Vec<PickerRow>,
        purpose: PickerPurpose,
        return_to: Screen,
        project_id: i64,
    ) -> Picker {
        Picker {
            title,
            rows,
            sel: 0,
            purpose,
            return_to,
            project_id,
            filter: String::new(),
            list_id: None,
            outcome: None,
        }
    }

    /// The rows the filter lets through, with their index into `rows`. `sel`,
    /// drawing and click zones all index this list, never `rows`.
    pub fn visible_rows(&self) -> Vec<(usize, &PickerRow)> {
        let needle = self.filter.to_lowercase();
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                needle.is_empty() || row.filter_text().to_lowercase().contains(&needle)
            })
            .collect()
    }

    pub fn selected_row(&self) -> Option<&PickerRow> {
        self.visible_rows().get(self.sel).map(|(_, row)| *row)
    }

    /// The identifier of the selected Linear row.
    pub fn selected_id(&self) -> Option<&str> {
        match self.selected_row()? {
            PickerRow::Linear(row) => Some(row.id.as_str()),
            _ => None,
        }
    }

    /// Select the visible row carrying `id`, else keep the index in range.
    pub(crate) fn reselect(&mut self, id: Option<&str>) {
        let visible = self.visible_rows();
        let found = id.and_then(|id| {
            visible
                .iter()
                .position(|(_, row)| matches!(row, PickerRow::Linear(r) if r.id == id))
        });
        let len = visible.len();
        self.sel = match found {
            Some(at) => at,
            None => self.sel.min(len.saturating_sub(1)),
        };
    }
}

/// A Linear list read as the picker shows it. An in-flight read is board
/// state on `LinearState`, not an outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListOutcome {
    Read {
        status: LinearListStatus,
        message: Option<String>,
    },
    Failed(String),
}

/// A Linear list row. `tag` is the discriminator a name cannot forge: the
/// team key for a project, the id for a space or a view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinearPickerRow {
    pub id: String,
    pub tag: String,
    pub name: String,
}

/// One-row text: the sanitiser keeps tab and newline, which a row cannot show.
pub fn collapse_line(s: &str) -> String {
    s.replace(['\n', '\t'], " ")
}

/// One selectable row of a [`Picker`]: either a concrete item (a project or
/// board id) or a trailing action.
#[derive(Clone, Debug)]
pub enum PickerRow {
    Item(String, i64),
    Action(String, PickerAction),
    Linear(LinearPickerRow),
}

impl PickerRow {
    /// The text a filter matches: what the row draws, collapsed to one line.
    pub fn filter_text(&self) -> String {
        match self {
            PickerRow::Item(label, _) | PickerRow::Action(label, _) => collapse_line(label),
            PickerRow::Linear(row) => collapse_line(&format!("{} {}", row.tag, row.name)),
        }
    }
}

/// The trailing action rows a picker can offer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PickerAction {
    /// Board picker → open the project picker ("⇄ Other projects…").
    OtherProjects,
    /// Project picker → open the project-create form ("＋ New project").
    NewProject,
    /// Board picker → open the board-create form ("＋ New board").
    NewBoard,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PickerPurpose {
    SwitchBoard,
    SwitchProject,
    DeleteColumnMoveTo { column_id: i64 },
    LinearList(LinearListKind),
}

/// Columns as `(label, id)` picker options, optionally without `exclude` — the
/// one place that shape is built, for the three column pickers (`D`'s
/// relocation target, `m`'s same-board move, and stage 2 of a cross-board
/// move, which excludes nothing because none of its columns is the current
/// one).
pub(crate) fn column_options(columns: &[Column], exclude: Option<i64>) -> Vec<(String, i64)> {
    columns
        .iter()
        .filter(|c| Some(c.id) != exclude)
        .map(|c| (c.name.clone(), c.id))
        .collect()
}

/// A yes/no confirmation.
pub struct Confirm {
    pub message: String,
    pub purpose: ConfirmPurpose,
    /// Where BOTH answers land. See [`Screen`]'s `return_to` note — the screen
    /// to go back to is a property of how the sheet was opened, never
    /// something to re-derive from `purpose`.
    pub return_to: Screen,
}

#[derive(Clone, Copy)]
pub enum ConfirmPurpose {
    DeleteCard(i64),
    /// Delete a column, optionally relocating its cards first (the
    /// destination the `D` picker collected).
    DeleteColumn {
        id: i64,
        move_cards_to: Option<i64>,
    },
    CancelRun(i64),
    /// Relaunch a real agent for this card — as destructive as it is
    /// expensive, hence the confirmation.
    RetryRun(i64),
    DeleteComment(i64),
    ArchiveBoard(i64),
    ArchiveProject(i64),
}

/// In-progress "move column" mini-mode state (entered with `M`).
///
/// The staged reorder lives **here**, never in `App::board`: the snapshot is
/// the daemon's answer to `board.get` and a refresh tick can replace it at any
/// moment, which would silently discard an order staged inside it. Holding the
/// permutation separately means the staged order survives a mid-mode refresh
/// and is applied only at read time (see [`super::App::display_column`]).
pub struct MoveColumnState {
    pub column_id: i64,
    /// Where the column sat when `M` was pressed — where `Esc` puts it back.
    pub original_index: usize,
    /// Where ←/→ have currently staged it. `Enter` commits exactly this as the
    /// `column.reorder` position.
    pub staged_index: usize,
}

/// In-progress "reorder card" mini-mode state (entered with `O`).
///
/// Like [`MoveColumnState`], the staged position lives **here**, never in
/// `App::board`: `App::cards_of` applies it as a read-time permutation, so a
/// refresh tick landing mid-mode cannot silently discard the staged order.
pub struct ReorderCardState {
    pub card_id: i64,
    pub column_id: i64,
    /// Where the card sat when `O` was pressed — where `Esc` puts it back.
    pub original_index: usize,
    /// Where `j`/`k` have currently staged it. `Enter` commits exactly this as
    /// the `card.move` position within the same column.
    pub staged_index: usize,
}

/// Mouse drag in progress.
pub struct DragState {
    pub kind: DragKind,
    pub from_col: usize,
    pub hover_col: usize,
    /// For a card drag: the card's index within `from_col` when the drag
    /// began, so a drop back at the origin is a no-op. `None` when the card
    /// is no longer in that column (or for a column drag).
    pub from_card: Option<usize>,
    /// For a card drag: the card index currently hovered in the drag's
    /// column — the position a same-column drop would land at. `None` while
    /// hovering empty space or another column.
    pub hover_card: Option<usize>,
}

#[derive(Clone, Copy)]
pub enum DragKind {
    Card { card_id: i64 },
    Column { column_id: i64 },
}
