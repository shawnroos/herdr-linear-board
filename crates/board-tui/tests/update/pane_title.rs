//! Herdr pane-border title sync, through the daemon's `pane.set_title` RPC.
//!
//! The TUI owns no Herdr connection of its own (see `AGENTS.md`), so the only
//! observable effect of `Effect::SetPaneTitle` and `Effect::SetLinearPaneTitle`
//! is that exact client call — and
//! it must stay a silent no-op everywhere the TUI is not the `herdr-board`
//! plugin pane.

use std::sync::{Arc, Mutex};

use board_core::client::{BoardClient, FakeBoardClient};
use board_core::protocol::{Event, LinearSnapshot};
use board_tui::app::{CardFilter, Mode, Screen};
use board_tui::testkit::{driver_with_origin, key, linear_driver, linear_fixture, linear_start};
use board_tui::view::pane_title;
use board_tui::{Driver, LinearStart, OriginContext};
use crossterm::event::KeyCode;
use serde_json::Value;

const EDITED: &str = "x";

/// Records every `pane.set_title` request and can make it fail, so a test can
/// prove the TUI treats a failed rename as harmless.
struct TitleClient {
    inner: FakeBoardClient,
    titles: Arc<Mutex<Vec<Value>>>,
    fail: bool,
}

impl TitleClient {
    fn new(fail: bool) -> (TitleClient, Arc<Mutex<Vec<Value>>>) {
        let titles = Arc::new(Mutex::new(Vec::new()));
        let client = TitleClient {
            inner: FakeBoardClient::new().unwrap(),
            titles: Arc::clone(&titles),
            fail,
        };
        (client, titles)
    }
}

impl BoardClient for TitleClient {
    fn call(&mut self, method: &str, params: Value) -> anyhow::Result<Value> {
        if method == "pane.set_title" {
            self.titles.lock().unwrap().push(params.clone());
            if self.fail {
                anyhow::bail!("herdr unavailable: pane.rename w1:p1: pane not found");
            }
        }
        self.inner.call(method, params)
    }

    fn subscribe(&mut self) -> anyhow::Result<Box<dyn Iterator<Item = Event> + Send>> {
        self.inner.subscribe()
    }
}

/// The context a real `herdr-board` plugin pane is launched with.
fn plugin_origin() -> OriginContext {
    OriginContext {
        origin_socket: Some("/run/herdr/sessions/work/herdr.sock".into()),
        session: Some("work".into()),
        plugin_id: Some("herdr-board".into()),
        pane_id: Some("w1:p1".into()),
        plugin_root: None,
    }
}

fn titles(recorded: &Arc<Mutex<Vec<Value>>>) -> Vec<Value> {
    recorded.lock().unwrap().clone()
}

/// The current toast text, if any (`Toast` itself is not `Debug`).
fn toast(d: &Driver) -> Option<String> {
    d.app.toast.as_ref().map(|t| t.text.clone())
}

#[test]
fn a_plugin_pane_sets_its_title_through_the_daemon_and_tracks_the_filter() {
    let (client, recorded) = TitleClient::new(false);
    let mut d = driver_with_origin(client, EDITED, plugin_origin());

    // Building the driver syncs the border once, before any input.
    let active = pane_title(&d.app.board.board, CardFilter::Active);
    assert_eq!(
        titles(&recorded),
        vec![serde_json::json!({
            "pane_id": "w1:p1",
            "title": active,
            "origin_socket": "/run/herdr/sessions/work/herdr.sock",
        })],
    );

    // `v` cycles the archive filter, and the border follows it.
    d.handle(key(KeyCode::Char('v')));
    let all = pane_title(&d.app.board.board, CardFilter::All);
    assert_ne!(all, active, "the filter must be visible in the title");
    let sent = titles(&recorded);
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1]["title"], Value::String(all));
    assert_eq!(sent[1]["pane_id"], "w1:p1");
}

#[test]
fn a_failed_rename_is_swallowed_and_never_toasts_over_the_board() {
    let (client, recorded) = TitleClient::new(true);
    let mut d = driver_with_origin(client, EDITED, plugin_origin());

    assert_eq!(titles(&recorded).len(), 1);
    assert!(toast(&d).is_none(), "toast: {:?}", toast(&d));

    d.handle(key(KeyCode::Char('v')));
    assert_eq!(titles(&recorded).len(), 2);
    assert_eq!(d.app.card_filter, CardFilter::All);
    assert!(toast(&d).is_none(), "toast: {:?}", toast(&d));
}

#[test]
fn outside_a_herdr_board_plugin_pane_no_title_request_is_ever_sent() {
    // Every way the guard can fail: not the plugin at all, the plugin without a
    // pane id, and the plugin without an invoking Herdr socket to rename in.
    let cases = [
        ("default (standalone TUI, tests)", OriginContext::default()),
        (
            "another plugin",
            OriginContext {
                plugin_id: Some("herdr-file-viewer".into()),
                ..plugin_origin()
            },
        ),
        (
            "no pane id",
            OriginContext {
                pane_id: None,
                ..plugin_origin()
            },
        ),
        (
            "no origin socket",
            OriginContext {
                origin_socket: None,
                ..plugin_origin()
            },
        ),
    ];

    for (case, origin) in cases {
        let (client, recorded) = TitleClient::new(false);
        let mut d = driver_with_origin(client, EDITED, origin);
        d.handle(key(KeyCode::Char('v')));
        assert_eq!(d.app.card_filter, CardFilter::All, "{case}");
        assert!(
            titles(&recorded).is_empty(),
            "{case}: {:?}",
            titles(&recorded)
        );
    }
}

// -- Linear mode ---------------------------------------------------------------

fn linear_title_driver(
    snapshot: LinearSnapshot,
    fail: bool,
    origin: OriginContext,
) -> (Driver, Arc<Mutex<Vec<Value>>>) {
    let (mut client, recorded) = TitleClient::new(fail);
    client.inner = client.inner.with_linear_snapshot(snapshot);
    let (d, _, _) = linear_driver(
        client,
        LinearStart {
            origin,
            ..linear_start()
        },
    );
    (d, recorded)
}

fn sent_titles(recorded: &Arc<Mutex<Vec<Value>>>) -> Vec<String> {
    titles(recorded)
        .iter()
        .map(|params| params["title"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn a_bound_linear_space_titles_the_pane_with_the_project_name() {
    let (d, recorded) =
        linear_title_driver(linear_fixture("bound-with-view"), false, plugin_origin());
    assert_eq!(d.app.mode, Mode::Linear);
    assert_eq!(
        titles(&recorded),
        vec![serde_json::json!({
            "pane_id": "w1:p1",
            "title": "Linear: Frame Effects",
            "origin_socket": "/run/herdr/sessions/work/herdr.sock",
        })],
    );
}

#[test]
fn an_unbound_linear_space_titles_the_pane_with_the_space_label() {
    let (_, recorded) = linear_title_driver(linear_fixture("unbound"), false, plugin_origin());
    assert_eq!(sent_titles(&recorded), vec!["Linear: Plugins"]);
}

#[test]
fn a_linear_space_with_no_label_falls_back_to_the_space_id() {
    let mut snapshot = linear_fixture("unbound");
    snapshot.workspace.label = "[\u{202E}]".into();
    let (_, recorded) = linear_title_driver(snapshot, false, plugin_origin());
    assert_eq!(sent_titles(&recorded), vec!["Linear: wA"]);
}

#[test]
fn a_hostile_project_name_reaches_the_title_stripped() {
    let mut snapshot = linear_fixture("bound-with-view");
    snapshot.project.name = Some("Example [Launch]\nBoard\u{202E} [ALL]\u{1b}".into());
    let (_, recorded) = linear_title_driver(snapshot, false, plugin_origin());
    assert_eq!(
        sent_titles(&recorded),
        vec!["Linear: Example LaunchBoard ALL"]
    );
}

#[test]
fn a_failed_linear_rename_is_swallowed_and_never_toasts() {
    let (mut d, recorded) =
        linear_title_driver(linear_fixture("bound-with-view"), true, plugin_origin());
    assert_eq!(sent_titles(&recorded), vec!["Linear: Frame Effects"]);
    assert!(toast(&d).is_none(), "toast: {:?}", toast(&d));
    assert_eq!(d.app.screen, Screen::LinearBoard);

    d.handle(key(KeyCode::Char('r')));
    assert_eq!(sent_titles(&recorded).len(), 2);
    assert!(toast(&d).is_none(), "toast: {:?}", toast(&d));
    assert_eq!(d.app.screen, Screen::LinearBoard);
}

#[test]
fn outside_a_herdr_board_plugin_pane_linear_mode_sends_no_title() {
    for origin in [
        OriginContext::default(),
        OriginContext {
            plugin_id: Some("herdr-file-viewer".into()),
            ..plugin_origin()
        },
        OriginContext {
            pane_id: None,
            ..plugin_origin()
        },
        OriginContext {
            origin_socket: None,
            ..plugin_origin()
        },
    ] {
        let (mut d, recorded) =
            linear_title_driver(linear_fixture("bound-with-view"), false, origin);
        d.handle(key(KeyCode::Char('r')));
        assert!(titles(&recorded).is_empty(), "{:?}", titles(&recorded));
    }
}
