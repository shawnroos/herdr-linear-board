//! boardd's `gh` client and parsing, against hand-written `gh` JSON and a
//! fake `GH_BIN` script. No test runs the real `gh`.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use board_daemon::gh::parse::{self, RequiredCheck};
use board_daemon::gh::{GhClient, GhConfig, GhError, PrRef};
use serde_json::{json, Value};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/gh")
            .join(name),
    )
    .unwrap()
}

fn open_view() -> Value {
    serde_json::from_slice(&fixture("pr-view-open.json")).unwrap()
}

fn required() -> Vec<RequiredCheck> {
    parse::required_checks(&fixture("pr-checks-required.json")).unwrap()
}

#[test]
fn an_open_pr_maps_its_latest_checks_and_the_required_check_that_blocks_it() {
    let detail = parse::pr_detail(&open_view(), Some(&required()));

    let ci = detail.ci.clone().unwrap();
    assert_eq!(
        (ci.passed, ci.pending, ci.failed, ci.skipped),
        (11, 1, 0, 11)
    );
    assert_eq!(ci.state, "pending");
    assert_eq!(
        detail.blocker.as_deref(),
        Some("E2E Verification Required (required) is pending")
    );
    assert_eq!((detail.additions, detail.deletions), (Some(412), Some(88)));
    assert_eq!(detail.changed_files, Some(17));
    assert_eq!(
        detail.head_ref.as_deref(),
        Some("feature/web-101-frame-effects")
    );
    assert_eq!(detail.state.as_deref(), Some("OPEN"));
    assert_eq!(detail.reviews.len(), 1);
    assert_eq!(detail.reviews[0].author.as_deref(), Some("ada"));
    assert_eq!(detail.reviews[0].state.as_deref(), Some("APPROVED"));
    let e2e = detail
        .checks
        .iter()
        .find(|c| c.name == "E2E Verification Required")
        .unwrap();
    assert!(e2e.required);
    assert_eq!(e2e.bucket, "pending");
}

#[test]
fn two_runs_of_one_check_name_count_once_as_the_latest() {
    let detail = parse::pr_detail(&open_view(), None);

    let runs: Vec<_> = detail
        .checks
        .iter()
        .filter(|c| c.name == "unit-tests")
        .collect();
    assert_eq!(runs.len(), 1);
    assert_eq!(
        runs[0].bucket, "pass",
        "the later SUCCESS replaces the FAILURE"
    );
    assert_eq!(detail.checks.len(), 23);
}

#[test]
fn null_review_decision_merged_at_and_rollup_map_without_failing() {
    let view = json!({
        "number": 7, "state": "MERGED", "isDraft": false, "mergedAt": null,
        "reviewDecision": null, "mergeStateStatus": null, "mergeable": null,
        "statusCheckRollup": null, "latestReviews": null, "additions": null,
        "headRefName": null, "title": null, "url": null
    });
    let detail = parse::pr_detail(&view, None);
    assert_eq!(detail.review_decision, None);
    assert_eq!(detail.merged_at, None);
    assert_eq!(detail.ci.unwrap().state, "none");
    assert!(detail.checks.is_empty() && detail.reviews.is_empty());
    assert_eq!(detail.blocker, None);
}

#[test]
fn a_blocked_pr_with_no_failing_required_check_names_its_review() {
    let view = json!({
        "state": "OPEN", "mergeStateStatus": "BLOCKED", "reviewDecision": "REVIEW_REQUIRED",
        "statusCheckRollup": []
    });
    let required = vec![RequiredCheck {
        name: "build".into(),
        state: "SUCCESS".into(),
        bucket: "pass".into(),
    }];
    let detail = parse::pr_detail(&view, Some(&required));
    assert_eq!(detail.blocker.as_deref(), Some("review required"));
}

#[test]
fn a_clean_pr_has_no_blocker() {
    let mut view = open_view();
    view["mergeStateStatus"] = json!("CLEAN");
    assert_eq!(parse::pr_detail(&view, Some(&required())).blocker, None);
}

#[test]
fn checks_output_that_is_not_a_json_array_is_unknown_not_empty() {
    assert_eq!(
        parse::required_checks(b"no required checks reported on the 'x' branch"),
        None
    );
    assert_eq!(parse::required_checks(b"[]"), Some(vec![]));
}

#[test]
fn the_preview_is_the_newest_deployment_status_url() {
    let deployments = json!([
        {"id": 11, "created_at": "2026-09-30T10:00:00Z"},
        {"id": 12, "created_at": "2026-09-30T11:00:00Z"}
    ]);
    assert_eq!(
        parse::newest_deployment(deployments.to_string().as_bytes()),
        Some(12)
    );
    let statuses = json!([
        {"state": "in_progress", "environment_url": "", "created_at": "2026-09-30T11:01:00Z"},
        {"state": "success", "environment_url": "https://web-git-101.vercel.app", "created_at": "2026-09-30T11:03:00Z"},
        {"state": "success", "environment_url": "https://old.vercel.app", "created_at": "2026-09-30T10:03:00Z"}
    ]);
    assert_eq!(
        parse::preview_url(statuses.to_string().as_bytes()).as_deref(),
        Some("https://web-git-101.vercel.app")
    );
}

#[test]
fn only_github_or_the_configured_host_with_safe_parts_is_a_pr() {
    let ok = PrRef::parse("https://github.com/acme/web.app/pull/42", None).unwrap();
    assert_eq!(
        (ok.host.as_str(), ok.repo().as_str(), ok.number),
        ("github.com", "acme/web.app", 42)
    );
    let ghe = PrRef::parse(
        "https://ghe.corp.example/acme/web/pull/7/",
        Some("ghe.corp.example"),
    );
    assert_eq!(ghe.unwrap().repo_arg(), "ghe.corp.example/acme/web");

    for refused in [
        "https://evil.example/acme/web/pull/1",
        "https://ghe.corp.example/acme/web/pull/1",
        "http://github.com/acme/web/pull/1",
        "https://github.com@evil.example/acme/web/pull/1",
        "https://github.com:443/acme/web/pull/1",
        "https://github.com/-R/web/pull/1",
        "https://github.com/acme/--repo/pull/1",
        "https://github.com/../web/pull/1",
        "https://github.com/acme/../pull/1",
        "https://github.com/acme/web/pull/12a",
        "https://github.com/acme/web/pull/-1",
        "https://github.com/acme/web/pull/",
        "https://github.com/acme/web/pull/1/commits/abc",
        "https://github.com/ac me/web/pull/1",
        "https://github.com/acme/web;id/pull/1",
    ] {
        assert_eq!(PrRef::parse(refused, None), None, "{refused}");
    }
    assert_eq!(
        PrRef::parse(
            "https://evil.example/acme/web/pull/1",
            Some("-evil.example")
        ),
        None
    );
}

struct FakeGh {
    dir: tempfile::TempDir,
}

impl FakeGh {
    /// `body` runs under `/bin/sh` with `$LOG` naming the argv log. The
    /// environment is cleared, so the script names its paths itself.
    fn new(body: &str) -> FakeGh {
        let dir = tempfile::tempdir().unwrap();
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gh");
        let script = format!(
            "#!/bin/sh\nLOG='{log}'\nENVLOG='{envlog}'\nFIX='{fix}'\nprintf '%s\\n' \"$*\" >> \"$LOG\"\n{body}\n",
            log = dir.path().join("argv.log").display(),
            envlog = dir.path().join("env.log").display(),
            fix = fixtures.display(),
        );
        let path = dir.path().join("gh");
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        FakeGh { dir }
    }

    fn bin(&self) -> PathBuf {
        self.dir.path().join("gh")
    }

    fn client(&self, env: Vec<(&str, &str)>, timeout: Duration) -> GhClient {
        GhClient::new(GhConfig {
            bin: self.bin().display().to_string(),
            host: None,
            env: env
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            timeout,
        })
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.path().join("argv.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn env_keys(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.path().join("env.log"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.split_once('=').map(|(k, _)| k.to_string()))
            .collect()
    }
}

const SERVE_OPEN_PR: &str = r#"case "$1 $2" in
  "pr view") /bin/cat "$FIX/pr-view-open.json" ;;
  "pr checks") /bin/cat "$FIX/pr-checks-required.json"; exit 8 ;;
  "api --method")
    case "$*" in
      *statuses*) printf '%s' '[{"state":"success","environment_url":"https://web-git-101.vercel.app","created_at":"2026-09-30T11:03:00Z"}]' ;;
      *) printf '%s' '[{"id":99,"created_at":"2026-09-30T11:00:00Z"}]' ;;
    esac ;;
esac"#;

fn pr() -> PrRef {
    PrRef::parse("https://github.com/acme/web/pull/4212", None).unwrap()
}

#[test]
fn a_fetch_reads_the_pr_its_required_checks_and_the_preview_from_validated_parts() {
    let gh = FakeGh::new(SERVE_OPEN_PR);
    let detail = gh
        .client(vec![], Duration::from_secs(10))
        .fetch(&pr())
        .unwrap();

    assert_eq!(detail.repo, "acme/web");
    assert_eq!(detail.number, 4212);
    assert_eq!(
        detail.blocker.as_deref(),
        Some("E2E Verification Required (required) is pending"),
        "pending checks exit 8 and their JSON still counts"
    );
    assert_eq!(
        detail.preview.as_deref(),
        Some("https://web-git-101.vercel.app")
    );
    assert_eq!(
        gh.calls(),
        vec![
            format!("pr view 4212 -R github.com/acme/web --json {}", parse::VIEW_FIELDS),
            "pr checks 4212 -R github.com/acme/web --required --json name,state,bucket".to_string(),
            "api --method GET --hostname github.com repos/acme/web/deployments -f ref=feature/web-101-frame-effects -f per_page=10".to_string(),
            "api --method GET --hostname github.com repos/acme/web/deployments/99/statuses -f per_page=20".to_string(),
        ]
    );
}

#[test]
fn the_child_environment_is_only_what_the_config_names() {
    let gh = FakeGh::new(r#"/usr/bin/env > "$ENVLOG"; printf '[]'"#);
    let client = gh.client(
        vec![("HOME", "/home/x"), ("GH_TOKEN", "t0ken")],
        Duration::from_secs(10),
    );
    let _ = client.run(&["pr".to_string()]).unwrap();

    let mut keys = gh.env_keys();
    // `/bin/sh` itself exports these; they are not inherited from boardd.
    keys.retain(|k| !matches!(k.as_str(), "PWD" | "OLDPWD" | "SHLVL" | "_"));
    keys.sort();
    assert_eq!(keys, vec!["GH_TOKEN", "HOME"]);
    assert!(
        std::env::vars().any(|(k, _)| k.starts_with("CARGO")),
        "the test process has variables a leak would show"
    );
}

#[test]
fn a_missing_gh_is_unavailable() {
    let client = GhClient::new(GhConfig {
        bin: "/nonexistent/hb-gh".into(),
        host: None,
        env: vec![],
        timeout: Duration::from_secs(5),
    });
    assert!(matches!(client.fetch(&pr()), Err(GhError::Unavailable(_))));
}

#[test]
fn a_logged_out_gh_is_unavailable() {
    let gh = FakeGh::new(
        "echo 'To get started with GitHub CLI, please run:  gh auth login' >&2; exit 4",
    );
    let error = gh
        .client(vec![], Duration::from_secs(10))
        .fetch(&pr())
        .unwrap_err();
    assert!(
        matches!(error, GhError::Unavailable(ref m) if m.contains("gh auth login")),
        "{error:?}"
    );
}

#[test]
fn a_gh_that_exits_non_zero_is_that_reads_failure() {
    let gh = FakeGh::new("echo 'GraphQL: Could not resolve to a PullRequest' >&2; exit 1");
    let error = gh
        .client(vec![], Duration::from_secs(10))
        .fetch(&pr())
        .unwrap_err();
    assert_eq!(
        error,
        GhError::Failed("GraphQL: Could not resolve to a PullRequest".into())
    );
    assert_eq!(gh.calls().len(), 1, "no further calls after the view fails");
}

#[test]
fn a_child_past_its_deadline_is_killed_and_reported_as_timed_out() {
    let gh = FakeGh::new("exec /bin/sleep 30");
    let started = Instant::now();
    let error = gh
        .client(vec![], Duration::from_millis(200))
        .fetch(&pr())
        .unwrap_err();
    assert_eq!(error, GhError::Timeout(Duration::from_millis(200)));
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn output_past_the_cap_is_refused() {
    let gh = FakeGh::new("/usr/bin/head -c 5000000 /dev/zero");
    let error = gh
        .client(vec![], Duration::from_secs(20))
        .run(&["pr".to_string()])
        .err();
    assert_eq!(error, Some(GhError::TooLarge));
}

fn space() -> board_daemon::linear::SpaceKey {
    board_daemon::linear::SpaceKey {
        session: "alpha".into(),
        space: "wS".into(),
    }
}

#[test]
fn a_settled_pr_is_due_after_ten_minutes_and_a_pending_one_after_two() {
    use board_daemon::gh::GhState;

    let mut state = GhState::default();
    let settled = pr();
    let pending = PrRef::parse("https://github.com/acme/web/pull/9", None).unwrap();
    assert!(state.observe(space(), [settled.clone(), pending.clone()].into(), 1_000));
    assert_eq!(state.take_queue().len(), 2);
    let mut clean = parse::pr_detail(&open_view(), None);
    clean.ci.as_mut().unwrap().state = "pass".into();
    state.record(&settled, &Ok(clean), 1_000);
    state.record(&pending, &Ok(parse::pr_detail(&open_view(), None)), 1_000);
    assert!(
        !state.observe(space(), [settled.clone(), pending.clone()].into(), 1_000),
        "same PRs"
    );

    assert!(!state.queue_due(1_119));
    assert!(state.queue_due(1_120));
    assert_eq!(state.take_queue(), vec![pending.clone()]);
    assert!(state.queue_due(1_600));
    assert_eq!(state.take_queue().len(), 2);
}

#[test]
fn while_gh_is_unavailable_the_periodic_pass_waits_ten_minutes() {
    use board_daemon::gh::GhState;

    let mut state = GhState::default();
    state.observe(space(), [pr()].into(), 1_000);
    state.take_queue();
    state.record(&pr(), &Err(GhError::Unavailable("no gh".into())), 1_000);

    assert_eq!(state.source().status, "unavailable");
    let new = PrRef::parse("https://github.com/acme/web/pull/9", None).unwrap();
    assert!(
        !state.observe(space(), [pr(), new].into(), 1_100),
        "a snapshot answering the announcement queues nothing"
    );
    assert!(!state.queue_due(1_599));
    assert!(state.queue_due(1_600));
}

#[test]
fn a_checks_call_that_times_out_keeps_the_pr_view() {
    let gh = FakeGh::new(
        r#"case "$1 $2" in
  "pr checks") exec /bin/sleep 30 ;;
  "pr view") /bin/cat "$FIX/pr-view-open.json" ;;
  "api --method") printf '[]' ;;
esac"#,
    );
    let detail = gh
        .client(vec![], Duration::from_millis(500))
        .fetch(&pr())
        .unwrap();

    assert_eq!(detail.state.as_deref(), Some("OPEN"));
    assert_eq!(detail.number, 4212);
    assert_eq!(
        detail.blocker,
        parse::pr_detail(&open_view(), None).blocker,
        "read as if no required checks were known"
    );
    assert_eq!(
        gh.calls().len(),
        3,
        "view, checks, then the deployments read"
    );
}

#[test]
fn a_helper_that_keeps_the_pipe_open_after_gh_exits_is_a_timeout() {
    let gh = FakeGh::new("/bin/sleep 30 &\nprintf '[]'");
    let started = Instant::now();
    let error = gh
        .client(vec![], Duration::from_millis(300))
        .run(&["pr".to_string()])
        .err();
    assert_eq!(error, Some(GhError::Timeout(Duration::from_millis(300))));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "returned after {:?}",
        started.elapsed()
    );
}

#[test]
fn a_board_not_seen_again_stops_being_read_and_its_prs_are_dropped() {
    use board_daemon::gh::GhState;

    let gone = board_daemon::linear::SpaceKey {
        session: "alpha".into(),
        space: "wGone".into(),
    };
    let other = PrRef::parse("https://github.com/acme/web/pull/9", None).unwrap();
    let mut state = GhState::default();
    state.observe(space(), [pr()].into(), 1_000);
    state.observe(gone.clone(), [other.clone()].into(), 1_000);
    for p in state.take_queue() {
        state.record(&p, &Ok(parse::pr_detail(&open_view(), None)), 1_000);
    }

    // The live board is seen again by the snapshot each pass announces, late
    // by a tick plus the pass itself.
    assert!(state.queue_due(1_630));
    assert_eq!(
        state.take_queue().len(),
        2,
        "a board seen 630s ago still counts"
    );
    for p in [pr(), other.clone()] {
        state.record(&p, &Ok(parse::pr_detail(&open_view(), None)), 1_632);
    }
    state.observe(space(), [pr()].into(), 1_633);

    assert!(state.queue_due(2_262));
    assert_eq!(
        state.take_queue(),
        vec![pr()],
        "the board not seen again is skipped"
    );
    assert!(
        state.current_document(&other).is_none(),
        "its PR entry is pruned"
    );
    assert!(state.current_document(&pr()).is_some());
    assert!(state
        .spaces_showing(std::slice::from_ref(&other))
        .is_empty());
    assert_eq!(
        state.spaces_showing(&[pr()]),
        ["wS".to_string()].into(),
        "the re-observed board keeps refreshing"
    );
}
