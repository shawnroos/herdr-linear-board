// herdr 0.9.3 sends `foreground_cwd` on `pane.list`; older payloads omit it.

use board_herdr::{AgentStatus, PaneInfo};
use serde_json::{json, Value};

fn pane_list_payload(foreground_cwd: Option<&str>) -> Value {
    let mut pane = json!({
        "pane_id": "w1:p2",
        "terminal_id": "term-2",
        "workspace_id": "w1",
        "tab_id": "w1:t1",
        "label": "card-7",
        "agent": "claude",
        "agent_status": "working",
        "title": "claude",
        "cwd": "/home/user/repo",
        "focused": true,
        "revision": 9
    });
    if let Some(dir) = foreground_cwd {
        pane["foreground_cwd"] = json!(dir);
    }
    json!({ "type": "pane_list", "panes": [pane] })
}

fn decode_panes(payload: Value) -> Vec<PaneInfo> {
    serde_json::from_value(payload["panes"].clone()).unwrap()
}

#[test]
fn pane_list_decodes_foreground_cwd_when_present() {
    let panes = decode_panes(pane_list_payload(Some("/home/user/repo/worktrees/feat")));

    assert_eq!(panes.len(), 1);
    assert_eq!(
        panes[0].foreground_cwd.as_deref(),
        Some("/home/user/repo/worktrees/feat")
    );
    assert_eq!(panes[0].cwd.as_deref(), Some("/home/user/repo"));
}

#[test]
fn pane_list_without_foreground_cwd_decodes_to_none_and_keeps_other_fields() {
    let with = decode_panes(pane_list_payload(Some("/home/user/repo/worktrees/feat")));
    let without = decode_panes(pane_list_payload(None));

    let pane = &without[0];
    assert_eq!(pane.foreground_cwd, None);
    assert_eq!(pane.pane_id, "w1:p2");
    assert_eq!(pane.terminal_id, "term-2");
    assert_eq!(pane.workspace_id, "w1");
    assert_eq!(pane.tab_id, "w1:t1");
    assert_eq!(pane.label.as_deref(), Some("card-7"));
    assert_eq!(pane.agent.as_deref(), Some("claude"));
    assert_eq!(pane.agent_status, AgentStatus::Working);
    assert_eq!(pane.title.as_deref(), Some("claude"));
    assert_eq!(pane.cwd.as_deref(), Some("/home/user/repo"));
    assert!(pane.focused);
    assert_eq!(pane.revision, 9);

    let mut expected = with[0].clone();
    expected.foreground_cwd = None;
    assert_eq!(*pane, expected);
}
