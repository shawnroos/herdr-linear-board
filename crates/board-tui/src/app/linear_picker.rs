//! Linear mode's type-to-filter pickers: opening one for a list kind, the
//! filter keys, and applying a list read when it lands. The row data is the
//! plugin's list envelope, sanitised on arrival.

use board_core::protocol::{LinearListKind, LinearListResult, LinearListStatus};
use crossterm::event::{KeyCode, KeyEvent};

use super::linear::{sanitise_list, LinearFailure, SpaceList};
use super::{App, Effect, LinearPickerRow, ListOutcome, Picker, PickerPurpose, PickerRow, Screen};

/// A row chosen in a Linear picker: which list, the list's argument, the id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinearPick {
    pub kind: LinearListKind,
    pub list_id: Option<String>,
    pub id: String,
}

fn title(kind: LinearListKind) -> &'static str {
    match kind {
        LinearListKind::Spaces => "Choose a space",
        LinearListKind::Projects => "Choose a project",
        LinearListKind::Views => "Choose a view",
    }
}

fn is_open_for(app: &App, kind: LinearListKind, id: &Option<String>) -> bool {
    app.screen == Screen::LinearPicker
        && app
            .picker
            .as_ref()
            .is_some_and(|p| p.purpose == PickerPurpose::LinearList(kind) && &p.list_id == id)
}

/// Open (or keep open) the picker for `kind` and mark its read in flight.
/// Returns whether a read must leave now: false while one is already on the
/// way, so a second open never sends a duplicate.
pub fn open_linear_picker(app: &mut App, kind: LinearListKind, id: Option<String>) -> bool {
    if app.linear.is_none() {
        return false;
    }
    let key = (kind, id.clone());
    if !is_open_for(app, kind, &id) {
        let mut picker = Picker::new(
            title(kind).to_string(),
            Vec::new(),
            PickerPurpose::LinearList(kind),
            app.screen,
            0,
        );
        picker.list_id = id;
        app.picker = Some(picker);
        app.screen = Screen::LinearPicker;
    }
    let Some(state) = app.linear.as_mut() else {
        return false;
    };
    state.lists_in_flight.insert(key)
}

fn rows_of(result: &LinearListResult) -> (LinearListStatus, Option<String>, Vec<PickerRow>) {
    let row = |id: &str, tag: &str, name: &str| {
        PickerRow::Linear(LinearPickerRow {
            id: id.to_string(),
            tag: tag.to_string(),
            name: name.to_string(),
        })
    };
    match result {
        LinearListResult::Spaces(list) => (
            list.status,
            list.message.clone(),
            list.rows
                .iter()
                .map(|r| row(&r.id, &r.id, &r.label))
                .collect(),
        ),
        LinearListResult::Projects(list) => (
            list.status,
            list.message.clone(),
            list.rows
                .iter()
                .map(|r| row(&r.id, r.team_key.as_deref().unwrap_or_default(), &r.name))
                .collect(),
        ),
        LinearListResult::Views(list) => (
            list.status,
            list.message.clone(),
            list.rows
                .iter()
                .map(|r| row(&r.id, &r.id, &r.name))
                .collect(),
        ),
    }
}

pub(super) fn list_arrived(
    app: &mut App,
    kind: LinearListKind,
    id: Option<String>,
    result: Result<LinearListResult, LinearFailure>,
) {
    let Some(state) = app.linear.as_mut() else {
        return;
    };
    state.lists_in_flight.remove(&(kind, id.clone()));
    let result = result.map(sanitise_list);
    if kind == LinearListKind::Spaces && id.is_none() {
        state.spaces = match &result {
            Ok(LinearListResult::Spaces(list)) => SpaceList::Read(list.clone()),
            Ok(_) => SpaceList::Failed("the daemon answered with another list".to_string()),
            Err(failure) => SpaceList::Failed(failure_text(failure)),
        };
        state.clamp_strip();
    }
    if !is_open_for(app, kind, &id) {
        return;
    }
    let Some(picker) = app.picker.as_mut() else {
        return;
    };
    let keep = picker.selected_id().map(str::to_string);
    match result {
        Ok(list) => {
            let (status, message, rows) = rows_of(&list);
            picker.rows = rows;
            picker.outcome = Some(ListOutcome::Read { status, message });
        }
        Err(failure) => {
            picker.rows.clear();
            picker.outcome = Some(ListOutcome::Failed(failure_text(&failure)));
        }
    }
    picker.reselect(keep.as_deref());
}

fn failure_text(failure: &LinearFailure) -> String {
    match failure {
        LinearFailure::MethodNotFound => {
            "the daemon is older than the board and has no linear.list".to_string()
        }
        LinearFailure::OpUnsupported(_) => {
            "this list needs a newer work plugin; update it".to_string()
        }
        LinearFailure::TimedOut(limit) => super::linear::read_timeout_text(*limit),
        LinearFailure::Failed(text) => super::sanitise(text),
    }
}

pub(super) fn linear_picker_key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    let Some(picker) = app.picker.as_mut() else {
        app.screen = app
            .linear
            .as_ref()
            .map_or(Screen::LinearBoard, |s| s.home_screen());
        return vec![];
    };
    let PickerPurpose::LinearList(kind) = picker.purpose else {
        return vec![];
    };
    match k.code {
        KeyCode::Up => picker.sel = picker.sel.saturating_sub(1),
        KeyCode::Down => {
            let last = picker.visible_rows().len().saturating_sub(1);
            picker.sel = (picker.sel + 1).min(last);
        }
        KeyCode::Enter => {
            let Some(id) = picker.selected_id().map(str::to_string) else {
                return vec![];
            };
            let space = app
                .linear
                .as_ref()
                .and_then(|s| s.bind_space.as_ref())
                .map(|row| row.id.clone());
            if let (LinearListKind::Projects, Some(space)) = (kind, space) {
                return vec![Effect::LinearSpaceBind {
                    space,
                    project: id,
                    view: None,
                }];
            }
            if let (LinearListKind::Views, Some(project)) = (kind, picker.list_id.clone()) {
                let Some(space) = app.linear.as_ref().map(|s| s.workspace_id.clone()) else {
                    return vec![];
                };
                return vec![Effect::LinearSpaceBind {
                    space,
                    project,
                    view: Some(id),
                }];
            }
            let list_id = picker.list_id.clone();
            let return_to = picker.return_to;
            app.picker = None;
            app.screen = return_to;
            if let Some(state) = app.linear.as_mut() {
                state.pick = Some(LinearPick { kind, list_id, id });
            }
        }
        KeyCode::Esc if picker.filter.is_empty() => {
            let return_to = picker.return_to;
            app.picker = None;
            app.screen = return_to;
            // A space picker closed without a choice leaves no space to bind.
            if kind == LinearListKind::Projects {
                if let Some(state) = app.linear.as_mut() {
                    state.bind_space = None;
                }
            }
        }
        KeyCode::Esc => edit_filter(picker, String::clear),
        KeyCode::Backspace => edit_filter(picker, |f| {
            f.pop();
        }),
        KeyCode::Char(c) => edit_filter(picker, |f| f.push(c)),
        _ => {}
    }
    vec![]
}

/// Change the filter, keeping the selected row when it is still visible and
/// clamping otherwise.
fn edit_filter(picker: &mut Picker, edit: impl FnOnce(&mut String)) {
    let keep = picker.selected_id().map(str::to_string);
    edit(&mut picker.filter);
    picker.reselect(keep.as_deref());
}

/// A space bind landed: the picker that chose it closes, and the board and
/// the strip's space list are read again so the space shows bound.
pub(super) fn space_bound(app: &mut App, space: &str) -> Vec<Effect> {
    let closes = app.screen == Screen::LinearPicker
        && app.picker.as_ref().is_some_and(|p| {
            matches!(
                p.purpose,
                PickerPurpose::LinearList(LinearListKind::Projects | LinearListKind::Views)
            )
        });
    if closes {
        if let Some(picker) = app.picker.take() {
            app.screen = picker.return_to;
        }
    }
    let Some(state) = app.linear.as_mut() else {
        return vec![];
    };
    state.bind_space = None;
    app.set_toast(format!("bound space {}", super::sanitise(space)), false);
    super::linear::request_or_queue(app)
}
