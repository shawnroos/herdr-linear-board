//! The issue view: the modal over a faded board, expanded and narrow, the
//! pinned panel, its sections, and the attachment and pull request cards.

use super::*;

use board_core::protocol::{LinearIssueDetail, LinearIssueDocument};
use board_tui::testkit::buffer_at;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

const WIDE: u16 = 140;
const TALL: u16 = 40;

fn doc(id: &str) -> LinearIssueDocument {
    LinearIssueDocument {
        schema: 1,
        status: "ok".into(),
        message: None,
        truncated: vec![],
        issue: Some(LinearIssueDetail {
            identifier: id.into(),
            title: format!("{id} title"),
            description: Some(format!("body of {id}")),
            ..Default::default()
        }),
    }
}

fn client() -> FakeBoardClient {
    fake_with(tabs_lanes())
        .with_linear_list(spaces(vec![]))
        .with_linear_issue("WEB-101", doc("WEB-101"))
        .with_linear_issue("WEB-102", doc("WEB-102"))
}

fn driver_with(client: FakeBoardClient, w: u16) -> Driver {
    let (mut d, _, _) = linear_driver(client, linear_start());
    d.app.last_area = Rect::new(0, 0, w, TALL);
    d
}

fn state(d: &Driver) -> &board_tui::app::LinearState {
    d.app.linear.as_ref().unwrap()
}

fn theme(d: &Driver) -> board_tui::theme::Theme {
    state(d).theme.clone()
}

fn bar_rows(d: &Driver, w: u16) -> u16 {
    state(d).bar_layout(w).rows()
}

fn row_text(buf: &Buffer, y: u16) -> String {
    (0..buf.area.width)
        .map(|x| buf[(x, y)].symbol().to_string())
        .collect()
}

/// The modal's rect at `w` × `h`: the body between the board header and the
/// bar, one row in at the top and bottom, at most 128 wide and centred.
fn modal_rect(d: &Driver, w: u16, h: u16) -> Rect {
    let top = 2;
    let body_h = h - top - bar_rows(d, w);
    let mw = (w - 8).min(128);
    Rect::new((w - mw) / 2, top + 1, mw, body_h - 2)
}

const BOX: &[char] = &[
    '─', '│', '┌', '┐', '└', '┘', '╭', '╮', '╰', '╯', '━', '┃', '═', '║',
];

#[test]
fn return_at_140_opens_a_borderless_modal_and_fades_the_board_but_not_the_bar() {
    let mut d = driver_with(client(), WIDE);
    let before = buffer_at(&mut d, WIDE, TALL);
    press(&mut d, KeyCode::Enter);
    assert_eq!(d.app.screen, Screen::LinearDetail);
    let after = buffer_at(&mut d, WIDE, TALL);
    let t = theme(&d);
    let modal = modal_rect(&d, WIDE, TALL);
    assert!(modal.width <= 128);

    // The header row's ID chip starts at the left edge, as on a full view.
    for y in modal.y + 1..modal.bottom() {
        for x in [modal.x, modal.right() - 1] {
            let cell = &after[(x, y)];
            assert_eq!(
                cell.bg, t.palette.panel_bg,
                "modal edge ({x},{y}) is panel_bg"
            );
            assert!(
                !cell.symbol().chars().any(|c| BOX.contains(&c)),
                "no border at ({x},{y}): {:?}",
                cell.symbol()
            );
        }
    }
    let bar_top = TALL - bar_rows(&d, WIDE);
    for y in 0..bar_top {
        for x in 0..WIDE {
            if modal.contains((x, y).into()) {
                continue;
            }
            let (was, now) = (&before[(x, y)], &after[(x, y)]);
            assert_eq!(now.bg, t.fade(was.bg), "bg faded at ({x},{y})");
            assert_eq!(now.fg, t.fade(was.fg), "fg faded at ({x},{y})");
        }
    }
    assert_ne!(t.fade(t.palette.panel_bg), t.palette.panel_bg);
    let bar = row_text(&after, TALL - 1);
    let at = bar.find("quit").expect("the bar draws its controls") as u16;
    let quit = &after[(at, TALL - 1)];
    assert_eq!(quit.fg, t.palette.overlay0, "the bar is not faded");
}

#[test]
fn the_modal_leaves_one_row_above_and_below_inside_the_body_at_any_height() {
    for h in [30u16, 40, 57] {
        let mut d = driver_with(client(), WIDE);
        press(&mut d, KeyCode::Enter);
        let buf = buffer_at(&mut d, WIDE, h);
        let t = theme(&d);
        let modal = modal_rect(&d, WIDE, h);
        let mid = modal.x + modal.width / 2;
        let body_top = 2;
        let body_bottom = h - bar_rows(&d, WIDE);
        assert_ne!(
            buf[(mid, body_top)].bg,
            t.palette.panel_bg,
            "h={h}: row above"
        );
        assert_eq!(buf[(mid, body_top + 1)].bg, t.palette.panel_bg, "h={h}");
        assert_eq!(buf[(mid, body_bottom - 2)].bg, t.palette.panel_bg, "h={h}");
        assert_ne!(
            buf[(mid, body_bottom - 1)].bg,
            t.palette.panel_bg,
            "h={h}: row below"
        );
    }
}

#[test]
fn e_toggles_modal_and_expanded_and_a_narrow_pane_drops_the_buttons_until_it_widens() {
    let mut d = driver_with(client(), WIDE);
    press(&mut d, KeyCode::Enter);
    let modal = render_at(&mut d, WIDE, TALL);
    assert!(
        modal.contains("Expand e") && modal.contains("Overlay o"),
        "{modal}"
    );

    press(&mut d, KeyCode::Char('e'));
    let expanded = buffer_at(&mut d, WIDE, TALL);
    let top = row_text(&expanded, 0);
    assert!(
        top.contains("Window e") && top.contains("Overlay o"),
        "{top}"
    );
    assert!(
        top.contains("WEB-101"),
        "the issue's header is the top row: {top}"
    );
    assert_eq!(
        expanded[(0, 4)].bg,
        theme(&d).app_bg,
        "nothing faded behind"
    );

    press(&mut d, KeyCode::Char('e'));
    let narrow = render_at(&mut d, 120, TALL);
    assert!(
        !narrow.contains("Expand e")
            && !narrow.contains("Overlay o")
            && !narrow.contains("Window e"),
        "{narrow}"
    );
    press(&mut d, KeyCode::Char('e'));
    assert!(
        toast(&d).contains("136"),
        "the tip says why e does nothing: {}",
        toast(&d)
    );
    press(&mut d, KeyCode::Char('o'));
    assert!(toast(&d).contains("136"), "{}", toast(&d));
    d.app.toast = None;

    let back = render_at(&mut d, WIDE, TALL);
    assert!(
        back.contains("Expand e"),
        "the modal returns at 140: {back}"
    );
}

#[test]
fn the_header_and_bar_span_the_width_and_the_body_starts_two_cells_in() {
    for w in [WIDE, 120, 80] {
        let mut d = driver_with(client(), w);
        press(&mut d, KeyCode::Enter);
        if w >= 136 {
            press(&mut d, KeyCode::Char('e'));
        }
        let buf = buffer_at(&mut d, w, TALL);
        let t = theme(&d);
        assert_eq!(
            buf[(0, 0)].bg,
            t.palette.accent,
            "w={w}: the ID chip starts the header"
        );
        assert!(row_text(&buf, 0).contains("WEB-101"), "w={w}");
        let name = state(&d).issue("WEB-101").unwrap().title.clone();
        let head: String = name.chars().take(12).collect();
        let title = (1..TALL)
            .find(|&y| row_text(&buf, y).contains(&head))
            .unwrap_or_else(|| panic!("w={w}: no title row"));
        let text = row_text(&buf, title);
        assert!(text.starts_with(&format!("  {head}")), "w={w}: {text:?}");
        let bar = row_text(&buf, TALL - 1);
        assert!(
            bar.contains("quit"),
            "w={w}: the bar draws under the issue: {bar}"
        );
    }
}

#[test]
fn esc_backspace_and_left_step_back_through_linked_issues_then_reselect_the_card() {
    let mut parent = doc("WEB-101");
    parent.issue.as_mut().unwrap().children = vec![board_core::protocol::LinearLinkedIssue {
        identifier: "WEB-900".into(),
        title: "child".into(),
        ..Default::default()
    }];
    for back in [KeyCode::Esc, KeyCode::Backspace, KeyCode::Left] {
        let c = fake_with(tabs_lanes())
            .with_linear_list(spaces(vec![]))
            .with_linear_issue("WEB-101", parent.clone())
            .with_linear_issue("WEB-900", doc("WEB-900"));
        let mut d = driver_with(c, 120);
        press(&mut d, KeyCode::Enter);
        press(&mut d, KeyCode::Enter);
        assert_eq!(state(&d).detail.as_deref(), Some("WEB-900"), "{back:?}");
        press(&mut d, back);
        assert_eq!(state(&d).detail.as_deref(), Some("WEB-101"), "{back:?}");
        press(&mut d, back);
        assert_eq!(d.app.screen, Screen::LinearBoard, "{back:?}");
        assert_eq!(state(&d).selected_identifier(), Some("WEB-101"), "{back:?}");
    }
}

#[test]
fn return_on_a_pane_row_focuses_that_pane() {
    let (c, log) = RecordingClient::new(client());
    let (mut d, _, _) = linear_driver(c, start_with_socket());
    d.app.last_area = Rect::new(0, 0, 120, TALL);
    press(&mut d, KeyCode::Char('l'));
    press(&mut d, KeyCode::Enter);
    assert_eq!(state(&d).detail.as_deref(), Some("WEB-105"));
    assert_eq!(
        state(&d).detail_selection,
        Some(board_tui::view::IssueRowKind::Pane(0)),
        "the page opens on the working pane"
    );
    press(&mut d, KeyCode::Enter);
    let requests = log.lock().unwrap();
    let (method, params) = requests.last().unwrap();
    assert_eq!(method, "pane.focus");
    assert_eq!(params["pane_id"], "wA:p1");
}

#[test]
fn the_faded_backdrop_takes_no_clicks() {
    let mut d = driver_with(client(), WIDE);
    press(&mut d, KeyCode::Enter);
    render_at(&mut d, WIDE, TALL);
    d.handle(left_down(1, 6));
    assert_eq!(state(&d).detail.as_deref(), Some("WEB-101"));
    assert_eq!(d.app.screen, Screen::LinearDetail);
}
