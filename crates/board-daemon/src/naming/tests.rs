use super::*;

use std::time::Duration;

use board_core::db::{BoardPanePlacement, NewBoardPane, SpaceBinding};
use board_core::protocol::{LinearBindParams, LinearUnbindParams};
use serde_json::{json, Value};

use crate::linear::{FetchPlan, SpaceRead};
use crate::testkit::{self, FakeHerdr};

/// A fake herdr session whose tabs and panes keep the labels it is renamed to.
struct Session {
    tabs: Arc<Mutex<Vec<Value>>>,
    panes: Arc<Mutex<Vec<Value>>>,
    herdr: FakeHerdr,
}

fn tab(id: &str, workspace: &str, number: u64, label: &str) -> Value {
    json!({"tab_id": id, "workspace_id": workspace, "number": number, "label": label,
           "focused": false, "pane_count": 1, "agent_status": "unknown"})
}

fn pane(id: &str, tab: &str, cwd: &str, agent: Option<&str>, label: Option<&str>) -> Value {
    let mut pane = testkit::pane_info(id);
    pane["workspace_id"] = json!(tab.split(':').next().unwrap());
    pane["tab_id"] = json!(tab);
    pane["foreground_cwd"] = json!(cwd);
    pane["agent"] = json!(agent);
    pane["label"] = json!(label);
    pane
}

fn session(tabs: Vec<Value>, panes: Vec<Value>) -> Session {
    let tabs = Arc::new(Mutex::new(tabs));
    let panes = Arc::new(Mutex::new(panes));
    let (t1, t2, p1, p2) = (tabs.clone(), tabs.clone(), panes.clone(), panes.clone());
    let herdr = testkit::herdr_server()
        .on("tab.list", move |req| {
            testkit::reply(
                req,
                json!({"type": "tab_list", "tabs": *t1.lock().unwrap()}),
            )
        })
        .on("pane.list", move |req| {
            testkit::reply(
                req,
                json!({"type": "pane_list", "panes": *p1.lock().unwrap()}),
            )
        })
        .on("tab.rename", move |req| {
            let mut tabs = t2.lock().unwrap();
            let tab = tabs
                .iter_mut()
                .find(|t| t["tab_id"] == req["params"]["tab_id"])
                .unwrap();
            tab["label"] = req["params"]["label"].clone();
            testkit::reply(req, json!({"type": "tab_info", "tab": tab.clone()}))
        })
        .on("pane.rename", move |req| {
            let mut panes = p2.lock().unwrap();
            let pane = panes
                .iter_mut()
                .find(|p| p["pane_id"] == req["params"]["pane_id"])
                .unwrap();
            pane["label"] = req["params"]["label"].clone();
            testkit::reply(req, json!({"type": "pane_info", "pane": pane.clone()}))
        })
        .serve();
    Session { tabs, panes, herdr }
}

impl Session {
    fn tab_label(&self, id: &str) -> Value {
        let tabs = self.tabs.lock().unwrap();
        tabs.iter().find(|t| t["tab_id"] == id).unwrap()["label"].clone()
    }

    fn pane_label(&self, id: &str) -> Value {
        let panes = self.panes.lock().unwrap();
        panes.iter().find(|p| p["pane_id"] == id).unwrap()["label"].clone()
    }

    fn set_tab_label(&self, id: &str, label: &str) {
        let mut tabs = self.tabs.lock().unwrap();
        tabs.iter_mut().find(|t| t["tab_id"] == id).unwrap()["label"] = json!(label);
    }

    fn renames(&self) -> usize {
        self.herdr.count("tab.rename") + self.herdr.count("pane.rename")
    }

    fn socket(&self) -> PathBuf {
        self.herdr.socket.canonicalize().unwrap()
    }
}

struct Fx {
    d: Arc<Daemon>,
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Fx {
    fn new() -> Fx {
        let d = testkit::daemon().build_daemon();
        d.store
            .lock()
            .set_space_binding(&SpaceBinding {
                herdr_session: "default".into(),
                space: "w1".into(),
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

    fn worktree(&self, name: &str) -> String {
        let path = self.root.join(name);
        std::fs::create_dir_all(path.join(".git")).unwrap();
        std::fs::create_dir_all(path.join("src")).unwrap();
        path.join("src").to_str().unwrap().to_string()
    }

    fn bind(&self, cwd: &str, issue: &str) {
        self.d
            .store
            .lock()
            .linear_bind(&LinearBindParams {
                cwd: cwd.into(),
                issue: issue.into(),
                ..LinearBindParams::default()
            })
            .unwrap();
    }

    fn unbind(&self, cwd: &str) {
        self.d
            .store
            .lock()
            .linear_unbind(&LinearUnbindParams {
                cwd: cwd.into(),
                ..LinearUnbindParams::default()
            })
            .unwrap();
    }

    fn titles(&self, issues: &[(&str, &str)]) {
        let key = SpaceKey {
            session: "default".into(),
            space: "w1".into(),
        };
        let plan = FetchPlan {
            project_id: "project-1".into(),
            view_id: None,
            filter: None,
            label: "w1".into(),
        };
        let nodes = issues
            .iter()
            .map(|(id, title)| json!({"identifier": id, "title": title}))
            .collect();
        self.d.linear.cache.read(&key, &plan, Duration::ZERO, |_| {
            Ok(SpaceRead {
                project: None,
                view_status: "none",
                view: None,
                issues: nodes,
                partial: false,
            })
        });
    }

    fn sweep(&self, s: &Session) -> bool {
        sweep_socket(&self.d, &s.socket()).unwrap()
    }
}

#[test]
fn a_bound_tab_and_its_split_take_the_issue_title_and_the_agent() {
    let fx = Fx::new();
    let web = fx.worktree("web");
    fx.bind(&web, "WEB-101");
    fx.titles(&[("WEB-101", "Fix login")]);
    let s = session(
        vec![tab("w1:t2", "w1", 2, "2")],
        vec![
            pane("w1:p2", "w1:t2", &web, Some("claude"), None),
            pane("w1:p3", "w1:t2", &web, None, Some("")),
        ],
    );
    assert!(fx.sweep(&s));
    assert_eq!(s.tab_label("w1:t2"), "WEB-101 · Fix login");
    assert_eq!(s.pane_label("w1:p2"), "WEB-101 · claude");
    assert_eq!(s.pane_label("w1:p3"), "WEB-101");
    let renames = s.renames();
    assert!(fx.sweep(&s));
    assert_eq!(s.renames(), renames, "a second pass writes nothing");
}

#[test]
fn an_unbound_pane_is_named_by_its_repo_and_agent() {
    let fx = Fx::new();
    let repo = fx.worktree("herdr-linear-board");
    let s = session(
        vec![tab("w1:t1", "w1", 1, "1")],
        vec![
            pane("w1:p1", "w1:t1", &repo, Some("codex"), None),
            pane("w1:p2", "w1:t1", &repo, None, None),
        ],
    );
    fx.sweep(&s);
    assert_eq!(s.pane_label("w1:p1"), "herdr-linear-board · codex");
    assert_eq!(s.pane_label("w1:p2"), "herdr-linear-board");
    assert_eq!(s.tab_label("w1:t1"), "herdr-linear-board · codex");
}

#[test]
fn a_hand_set_tab_label_survives_a_binding_change_while_the_split_follows() {
    let fx = Fx::new();
    let web = fx.worktree("web");
    fx.bind(&web, "WEB-101");
    let s = session(
        vec![tab("w1:t2", "w1", 2, "2")],
        vec![pane("w1:p2", "w1:t2", &web, Some("claude"), None)],
    );
    fx.sweep(&s);
    assert_eq!(s.tab_label("w1:t2"), "WEB-101");
    s.set_tab_label("w1:t2", "scratch");
    fx.unbind(&web);
    fx.bind(&web, "WEB-202");
    fx.sweep(&s);
    assert_eq!(s.tab_label("w1:t2"), "scratch");
    assert_eq!(s.pane_label("w1:p2"), "WEB-202 · claude");
    let mine =
        fx.d.store
            .lock()
            .generated_labels(&s.socket().to_string_lossy())
            .unwrap();
    assert!(
        !mine.iter().any(|(kind, _, _)| *kind == LabelKind::Tab),
        "a hand-set tab label is no longer the board's: {mine:?}"
    );
}

#[test]
fn a_tab_still_carrying_the_boards_label_follows_its_binding() {
    let fx = Fx::new();
    let web = fx.worktree("web");
    fx.bind(&web, "WEB-101");
    fx.titles(&[("WEB-101", "Fix login"), ("WEB-202", "Ship it")]);
    let s = session(
        vec![tab("w1:t2", "w1", 2, "2")],
        vec![pane("w1:p2", "w1:t2", &web, Some("claude"), None)],
    );
    fx.sweep(&s);
    assert_eq!(s.tab_label("w1:t2"), "WEB-101 · Fix login");
    fx.unbind(&web);
    fx.bind(&web, "WEB-202");
    fx.sweep(&s);
    assert_eq!(s.tab_label("w1:t2"), "WEB-202 · Ship it");
}

#[test]
fn a_tab_in_an_unbound_space_is_never_renamed() {
    let fx = Fx::new();
    let web = fx.worktree("web");
    fx.bind(&web, "WEB-101");
    let s = session(
        vec![tab("w1:t1", "w1", 1, "1"), tab("w2:t1", "w2", 1, "1")],
        vec![
            pane("w1:p1", "w1:t1", &web, None, None),
            pane("w2:p1", "w2:t1", &web, None, None),
        ],
    );
    fx.sweep(&s);
    assert_eq!(s.tab_label("w1:t1"), "WEB-101");
    assert_eq!(s.tab_label("w2:t1"), "1");
    assert_eq!(s.pane_label("w2:p1"), Value::Null);
}

#[test]
fn a_session_with_no_bound_space_is_not_touched() {
    let fx = Fx::new();
    fx.d.store
        .lock()
        .remove_space_binding("default", "w1")
        .unwrap();
    let s = session(vec![tab("w1:t1", "w1", 1, "1")], vec![]);
    assert!(!fx.sweep(&s));
    assert!(s.herdr.methods().is_empty(), "{:?}", s.herdr.methods());
}

#[test]
fn board_panes_keep_their_own_titles_and_a_board_only_tab_is_left_alone() {
    let fx = Fx::new();
    let web = fx.worktree("web");
    let s = session(
        vec![tab("w1:t1", "w1", 1, "1"), tab("w1:t2", "w1", 2, "2")],
        vec![
            pane("w1:p1", "w1:t1", &web, None, None),
            pane("w1:p2", "w1:t2", &web, None, None),
            pane("w1:p3", "w1:t1", &web, None, None),
        ],
    );
    let key = s.socket().to_string_lossy().into_owned();
    fx.d.store
        .lock()
        .record_board_pane(&NewBoardPane {
            herdr_socket: &key,
            pane_id: "w1:p2",
            context_key: "space=w1",
            placement: BoardPanePlacement::Tab,
            workspace_id: Some("w1"),
            origin_pane_id: None,
        })
        .unwrap();
    fx.sweep(&s);
    assert_eq!(s.pane_label("w1:p2"), Value::Null);
    assert_eq!(s.tab_label("w1:t2"), "2");
    assert_eq!(s.pane_label("w1:p1"), "web");

    // A board the daemon did not record titles itself through pane.set_title;
    // its label is its own from then on, even one equal to the generated name.
    assert_eq!(s.pane_label("w1:p3"), "web");
    crate::ops::handle_request(
        &fx.d,
        "pane.set_title",
        json!({"pane_id": "w1:p3", "title": "web", "origin_socket": key}),
    )
    .unwrap();
    fx.bind(&web, "WEB-9");
    fx.sweep(&s);
    assert_eq!(s.pane_label("w1:p1"), "WEB-9");
    assert_eq!(s.pane_label("w1:p3"), "web");
}

#[test]
fn labels_of_closed_tabs_and_panes_are_dropped() {
    let fx = Fx::new();
    let web = fx.worktree("web");
    let s = session(
        vec![tab("w1:t1", "w1", 1, "1")],
        vec![pane("w1:p1", "w1:t1", &web, None, None)],
    );
    fx.sweep(&s);
    let key = s.socket().to_string_lossy().into_owned();
    assert_eq!(fx.d.store.lock().generated_labels(&key).unwrap().len(), 2);
    s.tabs.lock().unwrap().clear();
    s.panes.lock().unwrap().clear();
    fx.sweep(&s);
    assert!(fx.d.store.lock().generated_labels(&key).unwrap().is_empty());
}

#[test]
fn folder_of_names_the_repo_of_a_linked_worktree() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("herdr-linear-board/.git/worktrees/u11")).unwrap();
    let linked = root.join("herdr-linear-board/worktrees/u11");
    std::fs::create_dir_all(linked.join("src")).unwrap();
    std::fs::write(
        linked.join(".git"),
        format!(
            "gitdir: {}\n",
            root.join("herdr-linear-board/.git/worktrees/u11").display()
        ),
    )
    .unwrap();
    assert_eq!(
        folder_of(linked.join("src").to_str().unwrap()).as_deref(),
        Some("herdr-linear-board")
    );
    assert_eq!(
        folder_of(root.join("herdr-linear-board").to_str().unwrap()).as_deref(),
        Some("herdr-linear-board")
    );
    let plain = root.join("notes");
    std::fs::create_dir_all(&plain).unwrap();
    assert_eq!(folder_of(plain.to_str().unwrap()).as_deref(), Some("notes"));
}
