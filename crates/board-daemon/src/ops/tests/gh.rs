//! `gh` enrichment through the daemon: the snapshot merge, the debounced
//! pass, the periodic re-read and `linear.pr`, against a fake `GH_BIN`.

use super::*;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::gh::GhConfig;
use crate::linear::fake::{FakeLinear, Recorded, Reply};
use crate::linear::{CredentialResolver, LinearClient, LinearConfig, NoKeychain};

const SPACE: &str = "wS";
const SOCKET: &str = "/tmp/hb-u4-none/sessions/alpha/herdr.sock";
const PR_A: &str = "https://github.com/acme/web/pull/4212";
const PR_B: &str = "https://github.com/acme/web/pull/77";

fn linear_client(fake: &FakeLinear) -> LinearClient {
    LinearClient::new(
        LinearConfig {
            api_url: fake.url(),
            timeout: Duration::from_secs(3),
            rate_limit_retries: 0,
            rate_limit_backoff: Duration::from_millis(10),
            page_cap: 10,
            page_size: 50,
        },
        CredentialResolver::new(Box::new(NoKeychain), Some("lin_api_u4test".into()), None),
    )
    .unwrap()
}

fn issue(identifier: &str, pr: Option<&str>) -> Value {
    json!({
        "id": format!("id-{identifier}"),
        "identifier": identifier,
        "title": format!("title {identifier}"),
        "state": {"id": "s-doing", "name": "In Progress", "type": "started"},
        "team": {"id": "team-1", "key": "WEB", "name": "Web"},
        "labels": {"nodes": []},
        "attachments": {"nodes": pr.map(|url| vec![json!({
            "url": url, "sourceType": "github", "createdAt": "2026-09-20T00:00:00Z",
            "metadata": {"status": "open"}
        })]).unwrap_or_default()},
    })
}

fn project_reply() -> Reply {
    Reply::ok(json!({"data": {"project": {
        "id": "project-1", "name": "Frame Effects", "url": "https://linear.app/p",
        "teams": {"nodes": [{"id": "team-1", "key": "WEB", "name": "Web", "states": {"nodes": [
            {"id": "s-doing", "name": "In Progress", "type": "started"}
        ]}}]}
    }}}))
}

/// The first issues read shows `first`, every later one `later`.
fn linear(first: Vec<Value>, later: Vec<Value>) -> impl Fn(&Recorded, usize) -> Reply {
    let reads = AtomicUsize::new(0);
    move |r, _| {
        let q = r.body["query"].as_str().unwrap_or("");
        if q.contains("issues(") {
            let nodes = if reads.fetch_add(1, Ordering::SeqCst) == 0 {
                first.clone()
            } else {
                later.clone()
            };
            Reply::ok(json!({"data": {"issues": {
                "nodes": nodes, "pageInfo": {"hasNextPage": false, "endCursor": null}
            }}}))
        } else if q.contains("project(id") {
            project_reply()
        } else {
            Reply::status(500, json!({"errors": [{"message": "unexpected query"}]}))
        }
    }
}

/// `gh pr view` serves the open-PR fixture for every PR except #77, which
/// fails when `fail_77` is set; checks are pending (exit 8).
fn gh_script(dir: &Path, fail_77: bool) -> String {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gh");
    format!(
        r#"#!/bin/sh
printf '%s\n' "$*" >> '{log}'
case "$1 $2 $3" in
  "pr view 77") if [ {fail} = 1 ]; then echo 'GraphQL: Could not resolve to a PullRequest' >&2; exit 1; fi ;;
esac
case "$1 $2" in
  "pr view") /bin/cat '{fix}/pr-view-open.json' ;;
  "pr checks") /bin/cat '{fix}/pr-checks-required.json'; exit 8 ;;
  *) printf '[]' ;;
esac
"#,
        log = dir.join("argv.log").display(),
        fix = fixtures.display(),
        fail = u8::from(fail_77),
    )
}

struct Board {
    d: Arc<Daemon>,
    rx: broadcast::Receiver<Event>,
    _fake: FakeLinear,
    gh: tempfile::TempDir,
}

impl Board {
    fn new(handler: impl Fn(&Recorded, usize) -> Reply + Send + Sync + 'static) -> Board {
        let (d, rx, _dispatch) = testkit::daemon().events_capacity(64).build_parts();
        let fake = FakeLinear::start(handler);
        d.linear.set_client(linear_client(&fake));
        d.linear
            .set_timing(Duration::from_secs(60), Duration::from_millis(30));
        d.linear
            .set_store_dir(PathBuf::from("/nonexistent/hb-u4-store"));
        d.gh.set_debounce(Duration::from_millis(30));
        d.store
            .lock()
            .set_space_binding(&board_core::db::SpaceBinding {
                herdr_session: "alpha".into(),
                space: SPACE.into(),
                project_id: "project-1".into(),
                display_name: Some("Frame Effects".into()),
                team_ids: vec![],
                view: None,
            })
            .unwrap();
        Board {
            d,
            rx,
            _fake: fake,
            gh: tempfile::tempdir().unwrap(),
        }
    }

    fn with_gh(self, fail_77: bool) -> Board {
        let bin = self.gh.path().join("gh");
        std::fs::write(&bin, gh_script(self.gh.path(), fail_77)).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        self.d.gh.configure(GhConfig {
            bin: bin.display().to_string(),
            host: None,
            env: vec![],
            timeout: Duration::from_secs(10),
        });
        self
    }

    fn snapshot_with(&self, force: bool) -> LinearSnapshot {
        let value = handle_request(
            &self.d,
            "linear.snapshot",
            json!({"workspace_id": SPACE, "origin_socket": SOCKET, "force": force}),
        )
        .unwrap();
        serde_json::from_value(value).unwrap()
    }

    fn snapshot(&self) -> LinearSnapshot {
        self.snapshot_with(false)
    }

    fn views_of(&self, number: &str) -> usize {
        std::fs::read_to_string(self.gh.path().join("argv.log"))
            .unwrap_or_default()
            .lines()
            .filter(|l| l.starts_with(&format!("pr view {number} ")))
            .count()
    }

    fn events(&mut self) -> Vec<Event> {
        let mut out = Vec::new();
        while let Ok(ev) = self.rx.try_recv() {
            out.push(ev);
        }
        out
    }

    fn wait_for_event(&mut self) -> Vec<Event> {
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            let events = self.events();
            if !events.is_empty() || Instant::now() >= until {
                return events;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn announced() -> Vec<Event> {
    vec![Event::LocalStateChanged {
        space: Some(SPACE.into()),
        snapshot: true,
    }]
}

#[test]
fn a_snapshot_after_enrichment_carries_each_cards_ci_diff_preview_and_blocker() {
    let rows = vec![issue("WEB-1", Some(PR_A))];
    let mut board = Board::new(linear(rows.clone(), rows)).with_gh(false);

    let before = board.snapshot();
    assert_eq!(before.issues["WEB-1"].pr.as_ref().unwrap().ci, None);
    assert_eq!(before.gh.unwrap().status, "unknown");
    assert_eq!(board.wait_for_event(), announced());

    let after = board.snapshot();
    let pr = after.issues["WEB-1"].pr.clone().unwrap();
    let ci = pr.ci.unwrap();
    assert_eq!(
        (ci.state.as_str(), ci.passed, ci.pending, ci.skipped),
        ("pending", 11, 1, 11)
    );
    assert_eq!((pr.additions, pr.deletions), (Some(412), Some(88)));
    assert_eq!(
        pr.blocker.as_deref(),
        Some("E2E Verification Required (required) is pending")
    );
    assert_eq!(pr.state.as_deref(), Some("OPEN"));
    assert_eq!(pr.status.as_deref(), Some("open"), "Linear's field stays");
    assert_eq!(after.gh.unwrap().status, "ok");
    assert_eq!(
        board.views_of("4212"),
        1,
        "the second snapshot reads the cache"
    );
}

#[test]
fn a_snapshot_change_inside_the_debounce_window_joins_the_one_pass() {
    let mut board = Board::new(linear(
        vec![issue("WEB-1", Some(PR_A))],
        vec![issue("WEB-1", Some(PR_A)), issue("WEB-2", Some(PR_B))],
    ))
    .with_gh(false);
    board.d.gh.set_debounce(Duration::from_millis(300));

    board.snapshot();
    board.snapshot_with(true);
    assert_eq!(board.wait_for_event(), announced());
    std::thread::sleep(Duration::from_millis(500));

    assert!(board.events().is_empty(), "a second pass announced");
    assert_eq!((board.views_of("4212"), board.views_of("77")), (1, 1));
}

#[test]
fn a_pr_gh_cannot_read_records_its_error_and_leaves_the_others_intact() {
    let rows = vec![issue("WEB-1", Some(PR_A)), issue("WEB-2", Some(PR_B))];
    let mut board = Board::new(linear(rows.clone(), rows)).with_gh(true);

    board.snapshot();
    assert_eq!(board.wait_for_event(), announced());
    let doc = board.snapshot();

    let good = doc.issues["WEB-1"].pr.clone().unwrap();
    assert!(good.ci.is_some() && good.gh_error.is_none());
    let bad = doc.issues["WEB-2"].pr.clone().unwrap();
    assert_eq!(bad.ci, None);
    assert_eq!(
        bad.gh_error.as_deref(),
        Some("gh failed: GraphQL: Could not resolve to a PullRequest")
    );
    assert_eq!(bad.repo, "acme/web", "Linear's PR data stays");
}

#[test]
fn gh_missing_is_code_8_on_the_pr_read_and_the_board_read_still_succeeds() {
    let rows = vec![issue("WEB-1", Some(PR_A))];
    let mut board = Board::new(linear(rows.clone(), rows));
    board.d.gh.configure(GhConfig {
        bin: "/nonexistent/hb-gh".into(),
        host: None,
        env: vec![],
        timeout: Duration::from_secs(5),
    });

    board.snapshot();
    assert_eq!(board.wait_for_event(), announced());
    let doc = board.snapshot();
    let pr = doc.issues["WEB-1"].pr.clone().unwrap();
    assert_eq!(pr.number, 4212);
    assert_eq!(pr.ci, None);
    let gh = doc.gh.unwrap();
    assert_eq!(gh.status, "unavailable");
    assert!(gh.message.unwrap().contains("not installed"));

    let error = handle_request(&board.d, "linear.pr", json!({"url": PR_A})).unwrap_err();
    assert_eq!(error.code(), 8, "{error}");
}

#[test]
fn the_pr_read_returns_full_detail_then_answers_from_the_cache() {
    let rows = vec![issue("WEB-1", None)];
    let board = Board::new(linear(rows.clone(), rows)).with_gh(false);

    let read = || -> LinearPrDocument {
        serde_json::from_value(handle_request(&board.d, "linear.pr", json!({"url": PR_A})).unwrap())
            .unwrap()
    };
    let doc = read();
    assert_eq!(doc.status, "ok");
    let pr = doc.pr.unwrap();
    assert_eq!((pr.repo.as_str(), pr.number), ("acme/web", 4212));
    assert_eq!(pr.checks.len(), 23);
    assert_eq!(pr.reviews.len(), 1);
    read();
    assert_eq!(board.views_of("4212"), 1);
    handle_request(&board.d, "linear.pr", json!({"url": PR_A, "force": true})).unwrap();
    assert_eq!(board.views_of("4212"), 2);
}

#[test]
fn a_refused_url_starts_no_gh_child() {
    let rows = vec![issue("WEB-1", None)];
    let board = Board::new(linear(rows.clone(), rows)).with_gh(false);

    for url in [
        "https://evil.example/acme/web/pull/1",
        "https://github.com/-R/web/pull/1",
        "https://github.com/acme/../pull/1",
        "https://github.com/acme/web/pull/1x",
    ] {
        let error = handle_request(&board.d, "linear.pr", json!({"url": url})).unwrap_err();
        assert_eq!(error.code(), 1, "{url}");
    }
    assert!(!board.gh.path().join("argv.log").exists());
}

#[test]
fn a_card_pr_on_a_foreign_host_is_not_enriched() {
    let rows = vec![issue("WEB-1", Some("https://evil.example/acme/web/pull/1"))];
    let board = Board::new(linear(rows.clone(), rows)).with_gh(false);

    let doc = board.snapshot();
    assert!(doc.issues["WEB-1"].pr.is_none());
    std::thread::sleep(Duration::from_millis(200));
    assert!(!board.gh.path().join("argv.log").exists());
}

#[test]
fn a_pending_pr_is_read_again_after_two_minutes_only_while_a_board_is_subscribed() {
    let rows = vec![issue("WEB-1", Some(PR_A))];
    let mut board = Board::new(linear(rows.clone(), rows)).with_gh(false);
    board.snapshot();
    assert_eq!(board.wait_for_event(), announced());
    let pr = crate::gh::PrRef::parse(PR_A, None).unwrap();
    let read_at = board
        .d
        .gh
        .state()
        .current_document(&pr)
        .and_then(|doc| doc.fetched_at)
        .unwrap();

    super::super::linear::pr::tick(&board.d, read_at + 120);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(board.views_of("4212"), 1, "no board subscribed");

    let _subscriber = board.d.subscriber();
    super::super::linear::pr::tick(&board.d, read_at + 119);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(board.views_of("4212"), 1, "not yet two minutes");

    super::super::linear::pr::tick(&board.d, read_at + 120);
    assert_eq!(board.wait_for_event(), announced());
    assert_eq!(board.views_of("4212"), 2);
}

#[test]
fn a_missing_gh_is_tried_once_not_again_on_every_snapshot_it_announces() {
    let rows = vec![issue("WEB-1", Some(PR_A))];
    let mut board = Board::new(linear(rows.clone(), rows)).with_gh(false);
    let log = board.gh.path().join("argv.log");
    board.d.gh.configure(GhConfig {
        bin: "/nonexistent/hb-gh".into(),
        host: None,
        env: vec![],
        timeout: Duration::from_secs(5),
    });

    board.snapshot();
    assert_eq!(
        board.wait_for_event(),
        announced(),
        "the board learns gh is missing"
    );
    for _ in 0..3 {
        board.snapshot();
        std::thread::sleep(Duration::from_millis(150));
    }

    assert!(
        board.events().is_empty(),
        "each announcement re-queued the PR"
    );
    assert_eq!(board.snapshot().gh.unwrap().status, "unavailable");
    assert!(!log.exists());
}
