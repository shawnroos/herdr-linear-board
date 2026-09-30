//! Rendering invariants that do not need a daemon.

use super::*;
use board_core::model::{Comment, CommentRecord};
use board_core::protocol::{CardStatus, SpaceKind};

fn row(cells: &[&str]) -> Vec<String> {
    cells.iter().map(|cell| cell.to_string()).collect()
}

fn rendered(rows: &[Vec<String>]) -> String {
    let mut out = Vec::new();
    table(&mut out, rows).expect("table renders");
    String::from_utf8(out).expect("table output is UTF-8")
}

#[test]
fn table_pads_every_column_to_its_widest_cell() {
    let text = rendered(&[
        row(&["#1", "short", "x"]),
        row(&["#100", "much longer", "y"]),
    ]);
    assert_eq!(text, "#1    short        x\n#100  much longer  y\n");
}

#[test]
fn table_trims_trailing_padding_from_ragged_rows() {
    let text = rendered(&[row(&["#1", "title", "session=a"]), row(&["#2", "t", ""])]);
    for line in text.lines() {
        assert_eq!(line, line.trim_end(), "no line may end in padding");
    }
    assert!(text.contains("#2  t\n"));
}

/// C2: `card show` and `card comment show` describe the same entity, so they
/// must not use two different line shapes.
#[test]
fn comment_lines_are_identical_in_card_show_and_comment_show() {
    let comment = Comment {
        id: 7,
        card_id: 3,
        author: "agent:9".into(),
        body: "hello".into(),
        created_at: "2026-01-01 00:00:00".into(),
    };
    let record = CommentRecord {
        id: 7,
        card_id: 3,
        author: "agent:9".into(),
        body: "hello".into(),
        created_at: "2026-01-01 00:00:00".into(),
        deleted_at: None,
    };
    let mut out = Vec::new();
    record.render(&mut out).expect("comment renders");
    let record_line = String::from_utf8(out).expect("UTF-8");
    assert_eq!(record_line.trim_end(), comment_row_for(&comment));
    assert!(record_line.starts_with("#7 card=3 agent:9 (2026-01-01 00:00:00): hello"));
}

#[test]
fn deleted_comments_keep_their_marker() {
    let record = CommentRecord {
        id: 1,
        card_id: 1,
        author: "user".into(),
        body: "gone".into(),
        created_at: "2026-01-01 00:00:00".into(),
        deleted_at: Some("2026-01-02 00:00:00".into()),
    };
    let mut out = Vec::new();
    record.render(&mut out).expect("comment renders");
    assert!(String::from_utf8(out).expect("UTF-8").contains("[deleted]"));
}

/// `emit_line`'s message must never leak into the JSON payload.
#[test]
fn a_message_serializes_as_its_payload_alone() {
    let payload = serde_json::json!({"id": 5});
    let message = Message {
        value: &payload,
        text: "Created card #5".into(),
    };
    assert_eq!(serde_json::to_value(&message).expect("serializes"), payload);
}

#[test]
fn card_lists_render_one_aligned_row_per_card() {
    let cards = vec![board_core::model::Card {
        id: 4,
        board_id: 1,
        column_id: 2,
        position: 0,
        title: "task".into(),
        description: String::new(),
        harness: "fake".into(),
        model: None,
        effort: None,
        permission_mode: None,
        session: Some("work".into()),
        space_kind: SpaceKind::Workspace,
        space_ref: None,
        space_cwd: None,
        status: CardStatus::Idle,
        awaiting_reason: None,
        session_id: None,
        created_at: "2026-01-01 00:00:00".into(),
        updated_at: "2026-01-01 00:00:00".into(),
        archived_at: None,
        labels: board_core::protocol::CardLabels::default(),
    }];
    let mut out = Vec::new();
    cards.render(&mut out).expect("cards render");
    let text = String::from_utf8(out).expect("UTF-8");
    assert_eq!(text, "#4  [idle]  col=2  task  session=work\n");
}

#[test]
fn an_import_renders_its_counts_then_one_row_per_record() {
    use board_core::protocol::LinearImportIgnored;
    let result = LinearImportResult {
        store_dir: "/s".into(),
        present: true,
        dry_run: false,
        imported: vec![LinearImportItem {
            kind: LinearImportKind::WorktreeBinding,
            key: "/w".into(),
            source: "bindings/a.json".into(),
            reason: None,
            dropped: vec!["consent".into()],
        }],
        skipped: vec![LinearImportItem {
            kind: LinearImportKind::SpaceBinding,
            key: "default/w\u{1b}1".into(),
            source: "workspaces/w1.json".into(),
            reason: Some("already in the board".into()),
            dropped: vec![],
        }],
        ignored: vec![LinearImportIgnored {
            path: "layouts".into(),
            reason: "herdr layout journals".into(),
        }],
    };
    let mut out = Vec::new();
    result.render(&mut out).expect("renders");
    assert_eq!(
        String::from_utf8(out).expect("UTF-8"),
        "imported 1, skipped 1, ignored 1 from /s\n\
         imported  worktree  /w          dropped consent\n\
         skipped   space     default/w1  already in the board\n\
         ignored             layouts     herdr layout journals\n"
    );
}

#[test]
fn an_absent_store_renders_nothing_to_import() {
    let result = LinearImportResult {
        store_dir: "/nowhere".into(),
        ..LinearImportResult::default()
    };
    let mut out = Vec::new();
    result.render(&mut out).expect("renders");
    assert_eq!(
        String::from_utf8(out).expect("UTF-8"),
        "nothing to import: no work store at /nowhere\n"
    );
}
