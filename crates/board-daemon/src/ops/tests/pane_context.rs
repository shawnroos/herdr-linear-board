//! `pane.context`: where the board's own pane sits in its herdr tab, and the
//! issue each other pane in that tab is bound to.

use super::*;

const SPACE: &str = "w1";
const BOARD: &str = "w1:p1";

/// One fake tab: `panes` are `(pane_id, cwd)` in layout order, the board first.
struct Tab {
    panes: Vec<(&'static str, Option<String>)>,
    zoomed: bool,
    focused: &'static str,
}

fn herdr_for(tab: Tab) -> FakeHerdr {
    let Tab {
        panes,
        zoomed,
        focused,
    } = tab;
    let listed = panes.clone();
    testkit::herdr_server()
        .on("pane.layout", move |req| {
            let rows: Vec<Value> = panes
                .iter()
                .map(|(id, _)| json!({"pane_id": id, "focused": *id == focused, "rect": {"x":0,"y":0,"width":60,"height":40}}))
                .collect();
            testkit::reply(
                req,
                json!({"type": "pane_layout", "layout": {
                    "workspace_id": SPACE, "tab_id": "w1:t1", "zoomed": zoomed,
                    "area": {"x":0,"y":0,"width":120,"height":40},
                    "focused_pane_id": focused, "panes": rows, "splits": []
                }}),
            )
        })
        .on("pane.list", move |req| {
            let mut rows: Vec<Value> = listed
                .iter()
                .map(|(id, cwd)| {
                    let mut pane = testkit::pane_info(id);
                    pane["foreground_cwd"] = json!(cwd);
                    pane
                })
                .collect();
            let mut elsewhere = testkit::pane_info("w1:p9");
            elsewhere["tab_id"] = json!("w1:t2");
            rows.push(elsewhere);
            testkit::reply(req, json!({"type": "pane_list", "panes": rows}))
        })
        .serve()
}

struct Fx {
    d: Arc<Daemon>,
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Fx {
    fn new() -> Fx {
        let d = test_daemon(Config::default());
        d.store
            .lock()
            .set_space_binding(&board_core::db::SpaceBinding {
                herdr_session: "default".into(),
                space: SPACE.into(),
                project_id: "project-1".into(),
                display_name: None,
                team_ids: vec![],
                view: None,
            })
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        Fx { d, _dir: dir, root }
    }

    fn bound_worktree(&self, name: &str, issue: &str) -> String {
        let path = self.root.join(name);
        std::fs::create_dir_all(path.join(".git")).unwrap();
        std::fs::create_dir_all(path.join("src")).unwrap();
        handle_request(
            &self.d,
            "linear.bind",
            json!({"cwd": path.to_str().unwrap(), "issue": issue}),
        )
        .unwrap();
        path.join("src").to_str().unwrap().to_string()
    }

    fn context(&self, herdr: &FakeHerdr, plugin_pane: bool) -> PaneContext {
        let v = handle_request(
            &self.d,
            "pane.context",
            json!({
                "origin_socket": herdr.socket,
                "pane_id": BOARD,
                "space": SPACE,
                "plugin_pane": plugin_pane,
            }),
        )
        .unwrap();
        serde_json::from_value(v).unwrap()
    }
}

#[test]
fn a_board_alone_in_its_tab_reads_as_alone_with_no_other_panes() {
    let fx = Fx::new();
    let herdr = herdr_for(Tab {
        panes: vec![(BOARD, None)],
        zoomed: false,
        focused: BOARD,
    });
    let ctx = fx.context(&herdr, false);
    assert!(ctx.alone);
    assert!(!ctx.overlay);
    assert!(ctx.panes.is_empty());
    assert_eq!(ctx.tab_id, "w1:t1");
    assert_eq!(herdr.methods()[0], "ping", "the gated connect runs first");
}

#[test]
fn a_split_lists_the_tabs_other_panes_in_layout_order_with_their_bound_issue() {
    let fx = Fx::new();
    let web101 = fx.bound_worktree("wt-101", "WEB-101");
    let herdr = herdr_for(Tab {
        panes: vec![
            (BOARD, None),
            ("w1:p2", Some(web101)),
            ("w1:p3", Some("/tmp".into())),
        ],
        zoomed: false,
        focused: "w1:p2",
    });
    let ctx = fx.context(&herdr, false);
    assert!(!ctx.alone);
    assert_eq!(ctx.focused_pane_id.as_deref(), Some("w1:p2"));
    let panes: Vec<_> = ctx
        .panes
        .iter()
        .map(|p| (p.pane_id.as_str(), p.issue.as_deref()))
        .collect();
    assert_eq!(panes, vec![("w1:p2", Some("WEB-101")), ("w1:p3", None)]);
}

#[test]
fn a_zoomed_board_fills_its_tab() {
    let fx = Fx::new();
    let herdr = herdr_for(Tab {
        panes: vec![(BOARD, None), ("w1:p2", None)],
        zoomed: true,
        focused: BOARD,
    });
    assert!(fx.context(&herdr, false).alone);
}

#[test]
fn a_plugin_board_the_daemon_did_not_place_is_an_overlay_and_a_placed_one_is_not() {
    let fx = Fx::new();
    // Herdr 0.9.3 keeps the tab zoomed on the pane beneath when focus leaves
    // an overlay, so the layout alone would read the overlay as a split.
    let herdr = herdr_for(Tab {
        panes: vec![(BOARD, None), ("w1:p2", None)],
        zoomed: true,
        focused: "w1:p2",
    });
    let ctx = fx.context(&herdr, true);
    assert!(ctx.overlay);
    assert!(!fx.context(&herdr, false).overlay, "a plain pane is a tile");

    let socket = herdr
        .socket
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    fx.d.store
        .lock()
        .record_board_pane(&board_core::db::NewBoardPane {
            herdr_socket: &socket,
            pane_id: BOARD,
            context_key: "space=w1",
            placement: board_core::db::BoardPanePlacement::Split,
            workspace_id: Some(SPACE),
            origin_pane_id: Some("w1:p2"),
        })
        .unwrap();
    assert!(!fx.context(&herdr, true).overlay);
}

#[test]
fn reading_pane_context_watches_that_session_for_focus_and_layout() {
    let fx = Fx::new();
    let herdr = herdr_for(Tab {
        panes: vec![(BOARD, None)],
        zoomed: false,
        focused: BOARD,
    });
    let _subscriber = fx.d.subscriber();
    fx.context(&herdr, false);
    let socket = herdr.socket.canonicalize().unwrap();
    assert!(
        fx.d.watch.lock().unwrap().level_sockets.contains(&socket),
        "the session is not watched for focus changes"
    );
}

#[test]
fn pane_context_refuses_an_empty_pane_before_touching_herdr() {
    let fx = Fx::new();
    let herdr = herdr_for(Tab {
        panes: vec![(BOARD, None)],
        zoomed: false,
        focused: BOARD,
    });
    let err = handle_request(
        &fx.d,
        "pane.context",
        json!({"origin_socket": herdr.socket, "pane_id": " ", "space": SPACE}),
    )
    .unwrap_err();
    assert_eq!(err.code(), 1);
    assert!(herdr.methods().is_empty());
}
