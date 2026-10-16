//! Marks, notes and show-requests on the Linear board, and the `a`, `x`,
//! `n`, `N` keys that act on them.

use super::*;

use board_core::protocol::{LinearNoteSetParams, LinearShowRequestParams};
use board_tui::app::{LinearWrite, WriteFailure};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::Terminal;

impl SharedFake {
    /// `tabs_lanes`-style board with no strip rows.
    pub(super) fn quiet(snapshot: LinearSnapshot) -> SharedFake {
        SharedFake {
            inner: Arc::new(Mutex::new(
                fake_with(snapshot.clone()).with_linear_list(spaces(vec![])),
            )),
            snapshot: Arc::new(Mutex::new(snapshot)),
            state_unknown: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The store refuses local state for an issue it has no record of.
    pub(super) fn known(&self, issue: &str) {
        self.inner
            .lock()
            .unwrap()
            .linear_activity_record(&LinearActivityRecordParams {
                tool_name: "mcp__linear__save_comment".into(),
                issue: Some(issue.into()),
                space: Some("wA".into()),
                ..LinearActivityRecordParams::default()
            })
            .unwrap();
    }

    pub(super) fn mark_kind(&self, issue: &str, kind: MarkKind, text: &str, by: &str) -> i64 {
        self.known(issue);
        self.inner
            .lock()
            .unwrap()
            .linear_mark_set(&LinearMarkSetParams {
                space: "wA".into(),
                issue: issue.into(),
                kind,
                text: Some(text.into()),
                created_by: Some(by.into()),
                owner: Default::default(),
            })
            .unwrap()
            .after
            .id
    }

    /// A reported `save_issue` the store could not link: a `◇` naming `cwd`.
    pub(super) fn suggest(&self, issue: &str, cwd: Option<&std::path::Path>) -> i64 {
        self.known(issue);
        self.inner
            .lock()
            .unwrap()
            .linear_activity_record(&LinearActivityRecordParams {
                tool_name: "mcp__linear__save_issue".into(),
                issue: Some(issue.into()),
                space: Some("wA".into()),
                cwd: cwd.map(|p| p.to_str().unwrap().to_string()),
                ..LinearActivityRecordParams::default()
            })
            .unwrap()
            .mark
            .expect("a suggestion")
            .after
            .id
    }

    pub(super) fn ask(&self, issue: &str, by: &str) -> i64 {
        self.known(issue);
        self.inner
            .lock()
            .unwrap()
            .linear_show_request(&LinearShowRequestParams {
                space: "wA".into(),
                issue: issue.into(),
                reason: None,
                requested_by: Some(by.into()),
                owner: board_core::protocol::LinearOwner {
                    herdr_pane_id: Some(format!("pane-{by}-{issue}")),
                    ..Default::default()
                },
            })
            .unwrap()
            .after
            .unwrap()
            .id
    }

    /// Closes a request behind the board, as a withdraw or another board would.
    pub(super) fn close(&self, id: i64) {
        self.inner.lock().unwrap().linear_show_dismiss(id).unwrap();
    }

    pub(super) fn note(&self, issue: &str, body: &str, author: &str) {
        self.known(issue);
        self.inner
            .lock()
            .unwrap()
            .linear_note_set(&LinearNoteSetParams {
                space: "wA".into(),
                issue: issue.into(),
                body: body.into(),
                author: author.into(),
                owner: Default::default(),
            })
            .unwrap();
    }
}

/// A board over `shared` that has read its local state, with the log cleared.
pub(super) fn board(shared: &SharedFake) -> (Driver, RequestLog) {
    let (client, log) = RecordingClient::new(shared.clone());
    let (mut d, _, _) = linear_driver(client, linear_start());
    d.on_local_state_changed(local(&[(Some("wA"), false)]));
    log.lock().unwrap().clear();
    (d, log)
}

pub(super) fn params(log: &RequestLog, method: &str) -> Vec<Value> {
    log.lock()
        .unwrap()
        .iter()
        .filter(|(m, _)| m == method)
        .map(|(_, p)| p.clone())
        .collect()
}

fn active_tab(d: &Driver) -> String {
    let state = d.app.linear.as_ref().unwrap();
    state.active_tab().unwrap().key.clone()
}

fn buffer(d: &mut Driver, w: u16, h: u16) -> Buffer {
    d.app.last_area = Rect::new(0, 0, w, h);
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| board_tui::view::view(&d.app, f)).unwrap();
    term.backend().buffer().clone()
}

fn buffer_row(buf: &Buffer, y: u16) -> String {
    (0..buf.area.width)
        .map(|x| {
            buf.content[usize::from(y) * usize::from(buf.area.width) + usize::from(x)].symbol()
        })
        .collect()
}

fn row_with(frame: &str, needle: &str) -> String {
    frame
        .lines()
        .map(backend_row)
        .find(|row| row.contains(needle))
        .unwrap_or_else(|| panic!("no row with {needle:?}:\n{frame}"))
}

/// The header row of `id`'s card at 144 cells: its 40 inner cells from the
/// identifier, trailing blanks trimmed. Marks sit right-aligned at its end.
fn header_of(frame: &str, id: &str) -> String {
    let row: Vec<char> = row_with(frame, id).chars().collect();
    let id: Vec<char> = id.chars().collect();
    let at = (0..row.len())
        .find(|&i| row[i..].starts_with(&id))
        .unwrap_or_else(|| panic!("no {id:?} in its own row"));
    row[at..(at + 40).min(row.len())]
        .iter()
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// The label of the view tab drawn on the accent, from the tab row.
fn active_tab_label(d: &mut Driver, w: u16, h: u16) -> String {
    let accent = board_tui::theme::Theme::default().palette.accent;
    let buf = buffer(d, w, h);
    (0..w)
        .filter(|&x| buf[(x, 2)].bg == accent && buf[(x, 2)].symbol() != "\u{e0bc}")
        .map(|x| buf[(x, 2)].symbol().to_string())
        .collect::<String>()
        .trim()
        .to_string()
}

#[test]
fn a_card_shows_every_mark_right_aligned_on_its_header() {
    let shared = SharedFake::quiet(tabs_lanes());
    shared.mark_kind("WEB-101", MarkKind::Done, "shipped", "agent-a");
    shared.mark_kind("WEB-101", MarkKind::Attention, "look", "agent-a");
    shared.mark_kind("WEB-101", MarkKind::Question, "which?", "agent-a");
    shared.mark_kind("WEB-102", MarkKind::Question, "which?", "agent-a");
    shared.mark_kind("WEB-102", MarkKind::Done, "done", "agent-a");
    shared.mark_kind("WEB-103", MarkKind::Done, "done", "agent-a");
    let (mut d, _) = board(&shared);
    let frame = render_at(&mut d, 144, 80);
    assert!(header_of(&frame, "WEB-101").ends_with(" !?✓"), "{frame}");
    assert!(header_of(&frame, "WEB-102").ends_with(" ?✓"), "{frame}");
    assert!(header_of(&frame, "WEB-103").ends_with(" ✓"), "{frame}");
    assert_eq!(header_of(&frame, "WEB-104"), "WEB-104 P3", "{frame}");
}

#[test]
fn a_requested_card_carries_the_badge_after_its_marks_and_before_done() {
    let shared = SharedFake::quiet(tabs_lanes());
    shared.mark_kind("WEB-101", MarkKind::Done, "done", "agent-a");
    shared.ask("WEB-101", "agent-b");
    shared.ask("WEB-102", "agent-b");
    shared.mark_kind("WEB-102", MarkKind::Question, "which?", "agent-a");
    let (mut d, _) = board(&shared);
    let frame = render_at(&mut d, 144, 40);
    assert!(header_of(&frame, "WEB-101").ends_with(" ◉✓"), "{frame}");
    assert!(header_of(&frame, "WEB-102").ends_with(" ?◉"), "{frame}");
}

#[test]
fn a_card_with_a_note_shows_the_latest_one_under_its_title() {
    let shared = SharedFake::quiet(tabs_lanes());
    shared.note("WEB-101", "first thought", "agent-a");
    shared.note("WEB-101", "waiting on review", "agent-a");
    let (mut d, _) = board(&shared);
    let frame = render_at(&mut d, 140, 40);
    assert!(frame.contains("› waiting on review"), "{frame}");
    assert!(!frame.contains("first thought"), "{frame}");
    let rows: Vec<String> = frame.lines().map(backend_row).collect();
    let id = rows.iter().position(|r| r.contains("WEB-101")).unwrap();
    let note = rows.iter().position(|r| r.contains("› waiting")).unwrap();
    let next = rows.iter().position(|r| r.contains("WEB-102")).unwrap();
    assert!(id < note && note < next, "{frame}");
}

#[test]
fn the_board_reads_local_state_after_its_first_snapshot_and_after_a_reconnect() {
    let shared = SharedFake::quiet(tabs_lanes());
    shared.mark_kind("WEB-101", MarkKind::Attention, "look", "agent-a");
    let (client, log) = RecordingClient::new(shared.clone());
    let (mut d, _, _) = linear_driver(client, linear_start());
    assert_eq!(count(&log, "linear.state.get"), 1, "{:?}", methods(&log));
    assert!(header_of(&render_at(&mut d, 144, 40), "WEB-101").ends_with(" !"));
    press(&mut d, KeyCode::Char('r'));
    assert_eq!(
        count(&log, "linear.state.get"),
        1,
        "a later snapshot does not"
    );
    d.on_daemon_signals(false, true);
    assert_eq!(count(&log, "linear.state.get"), 2, "{:?}", methods(&log));
}

#[test]
fn a_suggestion_announced_only_by_a_refetch_renders() {
    let shared = SharedFake::quiet(tabs_lanes());
    let (mut d, _) = board(&shared);
    assert!(!render_at(&mut d, 144, 40).contains('◇'));
    shared.suggest("WEB-101", None);
    d.on_local_state_changed(local(&[(Some("wA"), true)]));
    let frame = render_at(&mut d, 144, 40);
    assert!(header_of(&frame, "WEB-101").ends_with(" ◇"), "{frame}");
}

#[test]
fn ae1_a_on_a_selected_suggestion_binds_it_and_the_pinned_request_stays() {
    let shared = SharedFake::quiet(tabs_lanes());
    let worktree = Worktree::new("ae1");
    shared.ask("WEB-105", "agent-b");
    shared.suggest("WEB-101", Some(&worktree.0));
    let (mut d, log) = board(&shared);
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-101"));
    assert!(header_of(&render_at(&mut d, 144, 40), "WEB-101").ends_with(" ◇"));
    press(&mut d, KeyCode::Char('a'));
    let binds = params(&log, "linear.bind");
    assert_eq!(binds.len(), 1, "{:?}", methods(&log));
    assert_eq!(binds[0]["cwd"], worktree.0.to_str().unwrap());
    assert_eq!(binds[0]["issue"], "WEB-101");
    assert_eq!(binds[0]["space"], "wA");
    assert_eq!(count(&log, "linear.show.accept"), 0);
    let frame = render_at(&mut d, 140, 40);
    assert!(
        !frame.contains("◇"),
        "the bind cleared the suggestion:\n{frame}"
    );
    assert!(row_with(&frame, "◉ agent-b").contains("WEB-105"), "{frame}");
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-101"));
}

#[test]
fn a_and_x_on_the_board_leave_a_pending_request_alone() {
    let shared = SharedFake::quiet(tabs_lanes());
    shared.ask("WEB-105", "agent-b");
    let (mut d, log) = board(&shared);
    press(&mut d, KeyCode::Char('j'));
    assert!(render_at(&mut d, 140, 40).contains("◉ agent-b"));
    press(&mut d, KeyCode::Char('x'));
    assert_eq!(toast(&d), "nothing to dismiss");
    press(&mut d, KeyCode::Char('a'));
    assert_eq!(toast(&d), "nothing to accept");
    assert!(methods(&log).is_empty(), "{:?}", methods(&log));
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-102"));
    assert!(render_at(&mut d, 140, 40).contains("◉ agent-b"));
}

fn accepted(d: &mut Driver, id: i64, issue: &str, result: Result<(), WriteFailure>) {
    d.handle(Msg::LinearArrived(Box::new(
        board_tui::app::LinearArrival::Wrote {
            write: LinearWrite::AcceptShow {
                id,
                issue: issue.into(),
            },
            result,
        },
    )));
}

#[test]
fn f2_an_accepted_request_for_a_card_on_another_tab_switches_tab_and_page_and_selects_it() {
    let shared = SharedFake::quiet(tabs_lanes());
    let id = shared.ask("WEB-202", "agent-b");
    let (mut d, log) = board(&shared);
    render_at(&mut d, 44, 30);
    assert_eq!(active_tab(&d), "ms-frontend");
    accepted(&mut d, id, "WEB-202", Ok(()));
    assert_eq!(active_tab(&d), "ms-backend");
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-202"));
    assert_eq!(selected_column(&d), "st-prog");
    assert_eq!(d.app.screen, Screen::LinearBoard);
    assert_eq!(count(&log, "linear.mark.clear"), 0);
    assert_eq!(active_tab_label(&mut d, 44, 30), "Backend");
    let frame = render_at(&mut d, 44, 30);
    assert!(frame.contains("IN PROGRESS  1"), "{frame}");
    assert!(!frame.contains('◉'), "{frame}");
    press(&mut d, KeyCode::Char('['));
    assert_eq!(
        selected_id(&d).as_deref(),
        Some("WEB-101"),
        "the tab it left keeps its own cursor"
    );
    press(&mut d, KeyCode::Char(']'));
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-202"));
}

#[test]
fn an_accepted_request_whose_target_is_not_on_the_board_opens_its_detail_and_clears_nothing() {
    let shared = SharedFake::quiet(tabs_lanes());
    shared.mark_kind("WEB-9999", MarkKind::Attention, "look here", "agent-a");
    let id = shared.ask("WEB-9999", "agent-b");
    let (mut d, log) = board(&shared);
    accepted(&mut d, id, "WEB-9999", Ok(()));
    assert_eq!(d.app.screen, Screen::LinearDetail);
    let state = d.app.linear.as_ref().unwrap();
    assert_eq!(state.detail.as_deref(), Some("WEB-9999"));
    assert_eq!(params(&log, "linear.issue")[0]["issue"], "WEB-9999");
    assert_eq!(count(&log, "linear.mark.clear"), 0, "accept never clears");
    arrive(&mut d, tabs_lanes());
    assert_eq!(
        d.app.linear.as_ref().unwrap().detail.as_deref(),
        Some("WEB-9999"),
        "a snapshot without the issue does not close its page"
    );
    let frame = render_at(&mut d, 140, 40);
    assert!(frame.contains("look here"), "{frame}");
}

#[test]
fn an_accept_answered_elsewhere_first_says_it_is_gone_and_rereads_state() {
    let shared = SharedFake::quiet(tabs_lanes());
    let id = shared.ask("WEB-105", "agent-b");
    let (mut d, log) = board(&shared);
    assert!(render_at(&mut d, 140, 40).contains("◉ agent-b"));
    shared.close(id);
    accepted(
        &mut d,
        id,
        "WEB-105",
        Err(WriteFailure::Gone("gone".into())),
    );
    assert_eq!(
        methods(&log),
        vec!["linear.state.get"],
        "{:?}",
        methods(&log)
    );
    assert_eq!(toast(&d), "request no longer pending");
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-101"));
    let frame = render_at(&mut d, 140, 40);
    assert!(!frame.contains("◉ agent-b"), "{frame}");
}

#[test]
fn a_and_x_with_nothing_to_act_on_say_so_and_send_nothing() {
    let shared = SharedFake::quiet(tabs_lanes());
    let (mut d, log) = board(&shared);
    press(&mut d, KeyCode::Char('a'));
    assert_eq!(toast(&d), "nothing to accept");
    press(&mut d, KeyCode::Char('x'));
    assert_eq!(toast(&d), "nothing to dismiss");
    assert!(methods(&log).is_empty(), "{:?}", methods(&log));
}

#[test]
fn a_on_a_suggestion_with_no_worktree_refuses_and_sends_nothing() {
    let shared = SharedFake::quiet(tabs_lanes());
    shared.suggest("WEB-101", None);
    shared.ask("WEB-105", "agent-b");
    let (mut d, log) = board(&shared);
    press(&mut d, KeyCode::Char('a'));
    assert!(toast(&d).contains("no worktree"), "{}", toast(&d));
    assert!(methods(&log).is_empty(), "{:?}", methods(&log));
}

#[test]
fn accepting_a_suggestion_that_named_only_its_cwd_also_clears_it() {
    let shared = SharedFake::quiet(tabs_lanes());
    let dir = Worktree::new("cwd-only");
    std::fs::remove_dir_all(dir.0.join(".git")).unwrap();
    // Reported from outside any worktree, so the mark names only its cwd and
    // the daemon's bind cannot match it to the worktree it binds.
    let id = shared.suggest("WEB-101", Some(&dir.0));
    std::fs::create_dir_all(dir.0.join(".git")).unwrap();
    let (mut d, log) = board(&shared);
    press(&mut d, KeyCode::Char('a'));
    assert_eq!(params(&log, "linear.bind").len(), 1, "{:?}", methods(&log));
    assert_eq!(toast(&d), "bound WEB-101");
    assert_eq!(
        params(&log, "linear.mark.clear"),
        vec![serde_json::json!({ "ids": [id] })]
    );
    let frame = render_at(&mut d, 140, 40);
    assert!(!frame.contains('◇'), "{frame}");
}

#[test]
fn x_on_a_selected_suggestion_clears_that_mark() {
    let shared = SharedFake::quiet(tabs_lanes());
    let worktree = Worktree::new("x-sugg");
    let id = shared.suggest("WEB-101", Some(&worktree.0));
    shared.ask("WEB-105", "agent-b");
    let (mut d, log) = board(&shared);
    press(&mut d, KeyCode::Char('x'));
    assert_eq!(
        params(&log, "linear.mark.clear"),
        vec![serde_json::json!({ "ids": [id] })]
    );
    assert_eq!(count(&log, "linear.show.dismiss"), 0);
    let frame = render_at(&mut d, 140, 40);
    assert!(!frame.contains('◇'), "{frame}");
    assert!(frame.contains("◉ agent-b"), "{frame}");
}

#[test]
fn a_snapshot_that_reorders_cards_before_a_still_binds_the_card_that_was_selected() {
    let shared = SharedFake::quiet(tabs_lanes());
    let first = Worktree::new("reorder-101");
    let third = Worktree::new("reorder-103");
    shared.suggest("WEB-101", Some(&first.0));
    shared.suggest("WEB-103", Some(&third.0));
    let (mut d, log) = board(&shared);
    press(&mut d, KeyCode::Char('j'));
    press(&mut d, KeyCode::Char('j'));
    render_at(&mut d, 140, 40);
    let mut snapshot = tabs_lanes();
    edit_first_tab(&mut snapshot, |groups| {
        groups[0].issues.reverse();
        for lane in &mut groups[0].lanes {
            lane.issues.reverse();
        }
    });
    arrive(&mut d, snapshot);
    press(&mut d, KeyCode::Char('a'));
    let binds = params(&log, "linear.bind");
    assert_eq!(binds.len(), 1, "{:?}", methods(&log));
    assert_eq!(binds[0]["issue"], "WEB-103");
    assert_eq!(binds[0]["cwd"], third.0.to_str().unwrap());
}

#[test]
fn ae3_ae10_enter_clears_only_the_marks_it_showed_and_a_suggestion_stays() {
    let shared = SharedFake::quiet(tabs_lanes());
    let worktree = Worktree::new("ae3");
    let bang = shared.mark_kind("WEB-101", MarkKind::Attention, "look", "agent-a");
    shared.suggest("WEB-101", Some(&worktree.0));
    let (mut d, log) = board(&shared);
    press(&mut d, KeyCode::Enter);
    assert_eq!(d.app.screen, Screen::LinearDetail);
    assert_eq!(
        params(&log, "linear.mark.clear"),
        vec![serde_json::json!({ "ids": [bang] })],
        "one bulk clear of the ids shown"
    );
    shared.mark_kind("WEB-101", MarkKind::Question, "later", "agent-b");
    d.on_local_state_changed(local(&[(Some("wA"), false)]));
    press(&mut d, KeyCode::Esc);
    assert_eq!(d.app.screen, Screen::LinearBoard);
    let frame = render_at(&mut d, 144, 40);
    assert!(header_of(&frame, "WEB-101").ends_with(" ?◇"), "{frame}");
    assert_eq!(count(&log, "linear.mark.clear"), 1);
}

#[test]
fn ae12_the_detail_lists_the_marks_it_opened_with_and_keeps_them_after_the_clear() {
    let shared = SharedFake::quiet(tabs_lanes());
    shared.mark_kind(
        "WEB-101",
        MarkKind::Question,
        "which lane order?",
        "agent-a",
    );
    let (mut d, log) = board(&shared);
    press(&mut d, KeyCode::Enter);
    assert_eq!(count(&log, "linear.mark.clear"), 1);
    d.on_local_state_changed(local(&[(Some("wA"), false)]));
    assert_eq!(
        d.app.linear.as_ref().unwrap().marks_for("WEB-101").count(),
        0,
        "the mark is cleared"
    );
    let frame = render_at(&mut d, 140, 40);
    let row = row_with(&frame, "which lane order?");
    assert!(row.contains("? agent-a"), "{frame}");
    insta::assert_snapshot!("linear_detail_marks", frame);
}

#[test]
fn a_click_on_a_card_clears_its_marks_like_enter() {
    let shared = SharedFake::quiet(tabs_lanes());
    let bang = shared.mark_kind("WEB-102", MarkKind::Attention, "look", "agent-a");
    let (mut d, log) = board(&shared);
    let frame = render_at(&mut d, 140, 40);
    let rows: Vec<String> = frame.lines().map(backend_row).collect();
    let y = rows.iter().position(|r| r.contains("WEB-102")).unwrap();
    d.handle(left_down(5, y as u16));
    assert_eq!(d.app.screen, Screen::LinearDetail);
    assert_eq!(
        params(&log, "linear.mark.clear"),
        vec![serde_json::json!({ "ids": [bang] })]
    );
}

#[test]
fn n_walks_marked_cards_across_pages_and_tabs_and_wraps() {
    let shared = SharedFake::quiet(tabs_lanes());
    shared.mark_kind("WEB-104", MarkKind::Attention, "look", "agent-a");
    shared.mark_kind("WEB-107", MarkKind::Done, "done", "agent-a");
    shared.ask("WEB-201", "agent-b");
    let (mut d, log) = board(&shared);
    render_at(&mut d, 44, 30);
    let mut walk = vec![];
    for _ in 0..4 {
        press(&mut d, KeyCode::Char('n'));
        walk.push((
            active_tab(&d),
            selected_column(&d),
            selected_id(&d).unwrap(),
        ));
    }
    let step =
        |tab: &str, column: &str, id: &str| (tab.to_string(), column.to_string(), id.to_string());
    assert_eq!(
        walk,
        vec![
            step("ms-frontend", "st-todo", "WEB-104"),
            step("ms-frontend", "st-review", "WEB-107"),
            step("ms-backend", "st-todo", "WEB-201"),
            step("ms-frontend", "st-todo", "WEB-104"),
        ]
    );
    press(&mut d, KeyCode::Char('N'));
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-201"));
    assert_eq!(active_tab(&d), "ms-backend");
    assert_eq!(active_tab_label(&mut d, 44, 30), "Backend");
    assert!(
        methods(&log).is_empty(),
        "n writes nothing: {:?}",
        methods(&log)
    );
}

#[test]
fn n_with_nothing_selected_starts_from_the_top_of_the_focused_column() {
    let shared = SharedFake::quiet(tabs_lanes());
    shared.mark_kind("WEB-101", MarkKind::Attention, "look", "agent-a");
    shared.mark_kind("WEB-104", MarkKind::Attention, "look", "agent-a");
    let (mut d, _) = board(&shared);
    press(&mut d, KeyCode::Esc);
    press(&mut d, KeyCode::Char('n'));
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-101"));
    press(&mut d, KeyCode::Char('n'));
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-104"));
    press(&mut d, KeyCode::Char('n'));
    press(&mut d, KeyCode::Char('N'));
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-104"));
}

#[test]
fn ae6_a_sidebar_board_counts_attention_on_other_tabs() {
    let shared = SharedFake::quiet(tabs_lanes());
    shared.mark_kind("WEB-103", MarkKind::Attention, "look", "agent-a");
    shared.mark_kind("WEB-108", MarkKind::Done, "done", "agent-a");
    shared.mark_kind("WEB-202", MarkKind::Question, "which?", "agent-a");
    let (mut d, _) = board(&shared);
    render_at(&mut d, 240, 30);
    press(&mut d, KeyCode::Char('l'));
    let frame = render_at(&mut d, 44, 30);
    assert_eq!(pager(&frame), (Some(1), Some(2)), "{frame}");
    assert!(frame_row(&frame, 2).contains("  Backend !1  "), "{frame}");
    assert_eq!(active_tab_label(&mut d, 44, 30), "Frontend");
    insta::assert_snapshot!("linear_marks_44", frame);
}

#[test]
fn with_a_suggestion_selected_the_tip_names_bind() {
    let shared = SharedFake::quiet(tabs_lanes());
    let worktree = Worktree::new("cue");
    shared.suggest("WEB-101", Some(&worktree.0));
    shared.ask("WEB-105", "agent-b");
    let (mut d, _) = board(&shared);
    let buf = buffer(&mut d, 80, 30);
    assert!(buffer_row(&buf, 27).contains("a binds it · x dismisses it"));
    press(&mut d, KeyCode::Char('j'));
    let buf = buffer(&mut d, 80, 30);
    assert!(!buffer_row(&buf, 27).contains("a binds"));
}

#[test]
fn requests_at_44_cells_show_the_oldest_first_then_a_count_of_the_rest() {
    let mut snapshot = tabs_lanes();
    snapshot.issues.get_mut("WEB-105").unwrap().title = NINETY.into();
    let shared = SharedFake::quiet(snapshot);
    shared.ask("WEB-105", "agent-b");
    shared.ask("WEB-106", "agent-c");
    shared.ask("WEB-107", "agent-c");
    let (mut d, _) = board(&shared);
    let frame = render_at(&mut d, 44, 30);
    let line = row_with(&frame, "◉ agent-b");
    assert!(line.contains("WEB-105"), "oldest first:\n{frame}");
    assert!(line.trim_end().ends_with("more"), "{frame}");
    assert!(!line.contains("WEB-107"), "{frame}");
    assert!(unicode_width::UnicodeWidthStr::width(line.trim_end()) <= 44);
    insta::assert_snapshot!("linear_requests_44", frame);
}

#[test]
fn marks_notes_and_a_request_render_together_at_80_cells() {
    let shared = SharedFake::quiet(tabs_lanes());
    let worktree = Worktree::new("render80");
    shared.mark_kind("WEB-101", MarkKind::Attention, "look", "agent-a");
    shared.mark_kind("WEB-101", MarkKind::Question, "which?", "agent-a");
    shared.suggest("WEB-102", Some(&worktree.0));
    shared.note("WEB-103", "waiting on review", "agent-a");
    shared.ask("WEB-105", "agent-b");
    let (mut d, _) = board(&shared);
    let frame = render_at(&mut d, 80, 30);
    insta::assert_snapshot!("linear_marks_80", frame);
}
