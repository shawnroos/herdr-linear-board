//! The vendored snapshot fixtures: documents in the shape `linear.snapshot`
//! returns, which the TUI's Linear-mode tests also read.

use std::path::{Path, PathBuf};

use board_core::protocol::LinearSnapshot;

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/linear-snapshot")
}

#[test]
fn every_fixture_deserialises_into_linear_snapshot() {
    let mut parsed = 0;
    for entry in std::fs::read_dir(fixture_dir()).unwrap() {
        let path = entry.unwrap().path();
        let file = path.display();
        let text = std::fs::read_to_string(&path).unwrap();
        let snapshot: LinearSnapshot = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("fixture {file} does not parse: {e}"));
        assert_eq!(snapshot.schema, 1, "fixture {file}");
        assert_eq!(snapshot.workspace.id, "wA", "fixture {file}");
        assert!(snapshot.pane_status.is_empty(), "fixture {file}");
        parsed += 1;
    }
    assert_eq!(parsed, 9);
}

#[test]
fn bound_with_view_carries_bindings_panes_and_unmapped_tabs() {
    let text = std::fs::read_to_string(fixture_dir().join("bound-with-view.json")).unwrap();
    let snapshot: LinearSnapshot = serde_json::from_str(&text).unwrap();
    let issue = &snapshot.issues["WEB-3302"];
    assert_eq!(issue.state.kind.as_deref(), Some("started"));
    assert_eq!(issue.bindings[0].tab.as_ref().unwrap().id, "wA:t1");
    assert_eq!(issue.bindings[0].panes, vec!["wA:p1", "wA:p2"]);
    assert_eq!(snapshot.unmapped[0].reason, "no_binding");
    assert_eq!(
        snapshot.view.layout.as_ref().unwrap().grouping,
        "workflowState"
    );
    assert_eq!(snapshot.pane_ids(), vec!["wA:p1", "wA:p2", "wA:p9"]);
}

#[test]
fn a_document_missing_unmapped_still_parses_with_an_empty_list() {
    let text = std::fs::read_to_string(fixture_dir().join("bound-with-view.json")).unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&text).unwrap();
    value.as_object_mut().unwrap().remove("unmapped");
    let snapshot: LinearSnapshot = serde_json::from_value(value).unwrap();
    assert!(snapshot.unmapped.is_empty());
    assert_eq!(snapshot.issues.len(), 3);
}
