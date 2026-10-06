//! The issue view: the modal over a faded board, expanded and narrow, the
//! pinned panel, its sections, and the attachment and pull request cards.

use super::*;

use board_core::protocol::{LinearIssueDetail, LinearIssueDocument};
use board_tui::testkit::{buffer_at, UnknownMethods};
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

const OPEN_PR: &str = "https://github.com/acme/web/pull/2128";
const MERGED_PR: &str = "https://github.com/acme/web/pull/2100";

fn attachment(
    source: &str,
    title: &str,
    subtitle: &str,
    url: &str,
) -> board_core::protocol::LinearAttachment {
    board_core::protocol::LinearAttachment {
        id: Some(format!("a-{title}")),
        source_type: Some(source.into()),
        title: title.into(),
        subtitle: Some(subtitle.into()),
        url: url.into(),
        created_at: Some("2026-10-01T10:00:00.000Z".into()),
        metadata: serde_json::Value::Null,
    }
}

fn pr_attachment(
    url: &str,
    title: &str,
    status: &str,
    at: &str,
) -> board_core::protocol::LinearAttachment {
    board_core::protocol::LinearAttachment {
        id: Some(format!("pr-{url}")),
        source_type: Some("github".into()),
        title: title.into(),
        subtitle: None,
        url: url.into(),
        created_at: Some(at.into()),
        metadata: serde_json::json!({ "status": status, "branch": "task/export-folder", "targetBranch": "develop" }),
    }
}

fn doc_with_attachments() -> LinearIssueDocument {
    let mut d = doc("WEB-101");
    d.issue.as_mut().unwrap().attachments = vec![
        attachment(
            "github",
            "fix: remember the last export folder",
            "feature/web-101",
            "https://github.com/acme/web/commit/4f2a9c1",
        ),
        attachment(
            "slack",
            "Export folder keeps resetting",
            "#design-feedback",
            "https://acme.slack.com/archives/C0/p1",
        ),
        attachment(
            "upload",
            "Screen recording",
            "export-dialog.mp4",
            "https://uploads.linear.app/x/export-dialog.mp4",
        ),
        attachment(
            "notion",
            "Saved filters spec",
            "notion.so",
            "https://www.notion.so/acme/spec",
        ),
        pr_attachment(
            OPEN_PR,
            "Remember the export folder",
            "open",
            "2026-10-02T10:00:00.000Z",
        ),
        pr_attachment(
            MERGED_PR,
            "Add the export dialog",
            "merged",
            "2026-09-20T10:00:00.000Z",
        ),
    ];
    d
}

fn pr_doc(url: &str, number: u64) -> board_core::protocol::LinearPrDocument {
    use board_core::protocol::*;
    LinearPrDocument {
        schema: 1,
        status: "ok".into(),
        message: None,
        fetched_at: Some(1),
        pr: Some(LinearPrDetail {
            repo: "acme/web".into(),
            number,
            url: url.into(),
            title: Some("Remember the export folder per project".into()),
            state: Some("OPEN".into()),
            additions: Some(352),
            deletions: Some(87),
            changed_files: Some(14),
            head_ref: Some("task/export-folder".into()),
            ci: Some(LinearPrCi {
                state: "pending".into(),
                passed: 11,
                pending: 1,
                failed: 0,
                skipped: 3,
            }),
            blocker: Some("E2E Verification Required (required) is pending".into()),
            reviews: vec![LinearPrReview {
                author: Some("coderabbitai".into()),
                state: Some("COMMENTED".into()),
                submitted_at: None,
            }],
            preview: Some("https://pr-2128.preview.example.dev".into()),
            ..LinearPrDetail::default()
        }),
    }
}

fn merged_doc() -> board_core::protocol::LinearPrDocument {
    let mut d = pr_doc(MERGED_PR, 2100);
    let pr = d.pr.as_mut().unwrap();
    pr.state = Some("MERGED".into());
    pr.title = Some("Add the export dialog".into());
    pr.additions = Some(288);
    pr.deletions = Some(36);
    d
}

fn cards_client() -> FakeBoardClient {
    fake_with(tabs_lanes())
        .with_linear_list(spaces(vec![]))
        .with_linear_issue("WEB-101", doc_with_attachments())
}

fn expanded(c: impl BoardClient + 'static, h: u16) -> (Driver, String) {
    let (mut d, _, _) = linear_driver(c, linear_start());
    d.app.last_area = Rect::new(0, 0, WIDE, h);
    press(&mut d, KeyCode::Enter);
    press(&mut d, KeyCode::Char('e'));
    let frame = render_at(&mut d, WIDE, h);
    (d, frame)
}

fn row_of<'a>(frame: &'a str, needle: &str) -> &'a str {
    frame
        .lines()
        .find(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("no row with {needle:?}:\n{frame}"))
}

#[test]
fn the_main_column_runs_title_description_attachments_pull_requests_then_activity() {
    let c = cards_client()
        .with_linear_pr(OPEN_PR, pr_doc(OPEN_PR, 2128))
        .with_linear_pr(MERGED_PR, merged_doc());
    let (_, frame) = expanded(c, 120);
    let at = |needle: &str| {
        frame
            .find(needle)
            .unwrap_or_else(|| panic!("no {needle:?}:\n{frame}"))
    };
    assert!(at("body of WEB-101") < at("ATTACHMENTS  4"));
    assert!(at("ATTACHMENTS  4") < at("PULL REQUESTS  2"));
    assert!(at("PULL REQUESTS  2") < at("ACTIVITY"));
    assert!(row_of(&frame, "PULL REQUESTS").contains("1 open · 1 merged"));
    assert!(
        row_of(&frame, "ATTACHMENTS").starts_with("\"  ─ ATTACHMENTS"),
        "a lane-style section line"
    );
}

#[test]
fn an_open_pr_card_shows_checks_the_blocker_diff_reviews_and_preview() {
    let c = cards_client()
        .with_linear_pr(OPEN_PR, pr_doc(OPEN_PR, 2128))
        .with_linear_pr(MERGED_PR, merged_doc());
    let (_, frame) = expanded(c, 120);
    assert!(row_of(&frame, "#2128").contains("OPEN"), "{frame}");
    assert!(
        frame.contains("Remember the export folder per project"),
        "gh's title"
    );
    assert!(row_of(&frame, "task/export-folder").contains("→ develop"));
    let diff = row_of(&frame, "+352");
    assert!(diff.contains("−87") && diff.contains("14 files"), "{diff}");
    let checks = row_of(&frame, "11 passed");
    assert!(
        checks.contains("1 pending") && checks.contains("3 skipped"),
        "{checks}"
    );
    assert!(
        frame.contains("■ Blocked: E2E Verification Required (required) is pending"),
        "{frame}"
    );
    assert!(row_of(&frame, "coderabbitai").contains("commented"));
    assert!(frame.contains("pr-2128.preview.example.dev"), "{frame}");

    let merged = row_of(&frame, "#2100");
    assert!(
        merged.contains("acme/web") && merged.contains("Add the export dialog"),
        "{merged}"
    );
    assert!(
        merged.contains("+288") && merged.contains("−36"),
        "one line, diff aligned: {merged}"
    );
}

#[test]
fn with_gh_unavailable_the_pr_card_shows_linears_status_and_says_why() {
    let c = cards_client()
        .with_linear_pr_unavailable(OPEN_PR, "gh is not installed")
        .with_linear_pr_unavailable(MERGED_PR, "gh is not installed");
    let (d, frame) = expanded(c, 120);
    assert_eq!(d.app.screen, Screen::LinearDetail);
    assert!(
        row_of(&frame, "#2128").contains("OPEN"),
        "Linear's status: {frame}"
    );
    assert!(
        frame.contains("Remember the export folder"),
        "Linear's title"
    );
    assert!(frame.contains("Linear: open"), "{frame}");
    assert!(
        frame.contains("gh unavailable: gh is not installed"),
        "{frame}"
    );
    assert!(!frame.contains("passed"), "no checks without gh: {frame}");
}

#[test]
fn an_older_daemon_without_the_pr_read_shows_linear_only_cards_not_the_stale_screen() {
    let (c, log) = RecordingClient::new(UnknownMethods(cards_client(), "linear.pr"));
    let (mut d, frame) = expanded(c, 120);
    assert_eq!(d.app.screen, Screen::LinearDetail);
    assert!(state(&d).pr_read_missing);
    assert!(row_of(&frame, "#2128").contains("OPEN"), "{frame}");
    assert!(
        frame.contains("checks need a newer board daemon"),
        "{frame}"
    );
    assert!(!frame.contains("gh unavailable"), "{frame}");
    let reads = methods(&log).iter().filter(|m| *m == "linear.pr").count();
    // Back and in again: no further PR read once the daemon said it has none.
    press(&mut d, KeyCode::Esc);
    press(&mut d, KeyCode::Enter);
    assert_eq!(
        methods(&log).iter().filter(|m| *m == "linear.pr").count(),
        reads
    );
}

#[test]
fn attachment_cards_show_kind_title_detail_and_link() {
    let (_, frame) = expanded(
        cards_client().with_linear_pr(OPEN_PR, pr_doc(OPEN_PR, 2128)),
        140,
    );
    for (kind, chip, title, detail, link) in [
        (
            "Commit",
            "COMMIT",
            "fix: remember the last export folder",
            "feature/web-101",
            "github.com/acme/web/commit/4f2a9c1",
        ),
        (
            "Slack thread",
            "SLACK",
            "Export folder keeps resetting",
            "#design-feedback",
            "acme.slack.com/archives/C0/p1",
        ),
        (
            "Upload",
            "FILE",
            "Screen recording",
            "export-dialog.mp4",
            "uploads.linear.app/x/export-dialog.mp4",
        ),
        (
            "Link",
            "LINK",
            "Saved filters spec",
            "notion.so",
            "notion.so/acme/spec",
        ),
    ] {
        let head = row_of(&frame, &format!(" {kind} "));
        assert!(head.contains(chip), "{kind}: {head}");
        assert!(frame.contains(title), "{kind} title");
        assert!(frame.contains(detail), "{kind} detail");
        assert!(frame.contains(link), "{kind} link:\n{frame}");
    }
}

#[test]
fn return_on_a_card_opens_its_link() {
    let (mut d, opened, _) = linear_driver(cards_client(), linear_start());
    d.app.last_area = Rect::new(0, 0, 120, TALL);
    press(&mut d, KeyCode::Enter);
    assert_eq!(
        state(&d).detail_selection,
        None,
        "no pane to start on, so the first row: the commit card"
    );
    press(&mut d, KeyCode::Enter);
    assert_eq!(
        opened.lock().unwrap().as_slice(),
        ["https://github.com/acme/web/commit/4f2a9c1".to_string()]
    );
}

#[test]
fn pr_text_with_escape_sequences_reaches_the_frame_as_plain_text() {
    let mut hostile = pr_doc(OPEN_PR, 2128);
    let pr = hostile.pr.as_mut().unwrap();
    pr.title = Some("Fix\u{1b}[31m red\u{1b}]8;;https://evil\u{7}link".into());
    pr.blocker = Some("Unit\u{1b}[2J tests".into());
    pr.checks = vec![board_core::protocol::LinearPrCheck {
        name: "lint\u{1b}]0;owned\u{7}".into(),
        bucket: "fail".into(),
        ..Default::default()
    }];
    let c = cards_client().with_linear_pr(OPEN_PR, hostile);
    let (mut d, _) = expanded(c, 120);
    let buf = buffer_at(&mut d, WIDE, 120);
    for cell in buf.content() {
        assert!(
            !cell.symbol().chars().any(|c| c.is_control()),
            "control byte reached the frame: {:?}",
            cell.symbol()
        );
    }
    let read = state(&d).prs.get(OPEN_PR).cloned();
    let Some(board_tui::app::PrRead::Read(doc)) = read else {
        panic!("the PR read landed: {read:?}");
    };
    let pr = doc.pr.unwrap();
    assert!(!pr.title.unwrap().contains('\u{1b}'));
    assert!(!pr.checks[0].name.contains('\u{1b}'));
}

#[test]
fn two_columns_from_100_cells_and_one_below() {
    let mut d = driver_with(client(), 100);
    press(&mut d, KeyCode::Enter);
    let wide = render_at(&mut d, 100, TALL);
    let props = row_of(&wide, "PROPERTIES");
    assert!(
        props.find("PROPERTIES").unwrap() > 50,
        "sidebar on the right at 100: {props}"
    );
    let narrow = render_at(&mut d, 99, TALL);
    let title_row = narrow.lines().position(|l| l.contains("ACTIVITY")).unwrap();
    let props_row = narrow
        .lines()
        .position(|l| l.contains("PROPERTIES"))
        .unwrap();
    assert!(
        props_row > title_row,
        "one column at 99: the sidebar follows:\n{narrow}"
    );
}

const BOARD: u16 = 144;

fn long_parent() -> LinearIssueDocument {
    let mut parent = doc("WEB-101");
    let issue = parent.issue.as_mut().unwrap();
    issue.description = Some(
        (1..=60)
            .map(|n| format!("line {n} of a long description"))
            .collect::<Vec<_>>()
            .join("\n\n"),
    );
    issue.children = vec![board_core::protocol::LinearLinkedIssue {
        identifier: "WEB-900".into(),
        title: "child".into(),
        ..Default::default()
    }];
    parent
}

fn pin_client() -> FakeBoardClient {
    fake_with(tabs_lanes())
        .with_linear_list(spaces(vec![]))
        .with_linear_issue("WEB-101", long_parent())
        .with_linear_issue("WEB-102", doc("WEB-102"))
        .with_linear_issue("WEB-900", doc("WEB-900"))
}

/// WEB-101 opened at 144 cells and pinned with `o`.
fn pinned_driver() -> Driver {
    let mut d = driver_with(pin_client(), BOARD);
    press(&mut d, KeyCode::Enter);
    press(&mut d, KeyCode::Char('o'));
    d
}

fn pin(d: &Driver) -> &board_tui::app::PinnedIssue {
    state(d).pinned.as_ref().expect("an issue is pinned")
}

#[test]
fn o_pins_the_issue_into_a_column_slot_the_board_keeps_the_keys_and_o_unpins() {
    let mut d = driver_with(pin_client(), BOARD);
    assert_eq!(state(&d).page(BOARD).visible, 3);
    press(&mut d, KeyCode::Enter);
    press(&mut d, KeyCode::Char('o'));
    assert_eq!(d.app.screen, Screen::LinearBoard);
    assert_eq!(pin(&d).issue, "WEB-101");
    assert!(
        pin(&d).doc.is_some(),
        "the open issue's document moved to the pin"
    );
    assert_eq!(
        state(&d).page(BOARD).visible,
        2,
        "the board dropped a column"
    );
    let frame = render_at(&mut d, BOARD, TALL);
    let top = row_of(&frame, "Unpin o");
    assert!(
        top.find("WEB-101").unwrap() > 90,
        "the panel sits at the right: {top}"
    );
    assert_eq!(state(&d).selected_identifier(), Some("WEB-101"));
    press(&mut d, KeyCode::Char('l'));
    assert_eq!(state(&d).sel_group, 1, "the board keeps the keys");

    press(&mut d, KeyCode::Char('o'));
    assert!(state(&d).pinned.is_none());
    assert_eq!(state(&d).page(BOARD).visible, 3);
    assert!(!render_at(&mut d, BOARD, TALL).contains("Unpin o"));
}

#[test]
fn p_focuses_the_panel_down_scrolls_return_opens_a_linked_issue_there_and_esc_returns() {
    let mut d = pinned_driver();
    press(&mut d, KeyCode::Char('p'));
    assert_eq!(state(&d).focus, Focus::Pinned);
    render_at(&mut d, BOARD, TALL);
    let before = pin(&d).scroll;
    press(&mut d, KeyCode::Down);
    press(&mut d, KeyCode::Down);
    assert!(
        pin(&d).scroll > before,
        "↓ past the last row scrolls the panel"
    );
    press(&mut d, KeyCode::PageUp);
    assert_eq!(pin(&d).scroll, 0);

    press(&mut d, KeyCode::Enter);
    assert_eq!(
        pin(&d).issue,
        "WEB-900",
        "the linked issue opened in the panel"
    );
    assert!(pin(&d).doc.is_some(), "its read landed in the panel's slot");
    assert_eq!(d.app.screen, Screen::LinearBoard);
    press(&mut d, KeyCode::Backspace);
    assert_eq!(
        pin(&d).issue,
        "WEB-101",
        "Backspace steps back in the panel"
    );

    press(&mut d, KeyCode::Esc);
    assert!(
        matches!(state(&d).focus, Focus::Card(_)),
        "{:?}",
        state(&d).focus
    );
    assert_eq!(state(&d).selected_identifier(), Some("WEB-101"));
    assert!(state(&d).pinned.is_some(), "Esc keeps the pin");
}

#[test]
fn with_a_pin_return_opens_another_card_as_a_modal_and_both_documents_stay_loaded() {
    let mut d = pinned_driver();
    press(&mut d, KeyCode::Down);
    assert_eq!(state(&d).selected_identifier(), Some("WEB-102"));
    press(&mut d, KeyCode::Enter);
    assert_eq!(d.app.screen, Screen::LinearDetail);
    let frame = render_at(&mut d, BOARD, TALL);
    assert!(frame.contains("Expand e"), "a modal: {frame}");
    let open = state(&d).detail_doc.as_ref().map(|(id, _)| id.as_str());
    assert_eq!(open, Some("WEB-102"));
    assert_eq!(pin(&d).issue, "WEB-101");
    assert!(pin(&d).doc.is_some());

    press(&mut d, KeyCode::Char('o'));
    assert_eq!(pin(&d).issue, "WEB-102", "o in a modal replaces the pin");
    assert_eq!(d.app.screen, Screen::LinearBoard);
}

#[test]
fn below_two_columns_the_panel_hides_the_tip_says_it_is_pinned_and_its_focus_returns() {
    let mut d = pinned_driver();
    press(&mut d, KeyCode::Char('p'));
    assert_eq!(state(&d).focus, Focus::Pinned);
    d.app.last_area = Rect::new(0, 0, 90, TALL);
    press(&mut d, KeyCode::Down);
    assert!(
        matches!(state(&d).focus, Focus::Card(_)),
        "{:?}",
        state(&d).focus
    );
    d.app.toast = None;
    let frame = render_at(&mut d, 90, TALL);
    assert!(!frame.contains("Unpin o"), "{frame}");
    assert!(frame.contains("WEB-101 is still pinned"), "{frame}");
    press(&mut d, KeyCode::Char('p'));
    assert!(toast(&d).contains("wider pane"), "{}", toast(&d));
    let back = render_at(&mut d, BOARD, TALL);
    assert!(back.contains("Unpin o"), "the panel returns at 144: {back}");
}
