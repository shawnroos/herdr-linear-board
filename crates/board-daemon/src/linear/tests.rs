use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::fake::{FakeLinear, Recorded, Reply};
use super::{
    fetch, CredentialResolver, CredentialSource, FetchPlan, KeychainRead, KeychainReader,
    LinearClient, LinearConfig, LinearError, NoKeychain, SecurityCli,
};

const KEY: &str = "lin_api_SECRETtestKEY0123456789";

fn config(fake: &FakeLinear) -> LinearConfig {
    LinearConfig {
        api_url: fake.url(),
        timeout: Duration::from_secs(2),
        rate_limit_retries: 1,
        rate_limit_backoff: Duration::from_millis(50),
        page_cap: 10,
        page_size: 2,
    }
}

fn env_only() -> CredentialResolver {
    CredentialResolver::new(Box::new(NoKeychain), Some(KEY.to_string()), None)
}

fn client(fake: &FakeLinear) -> LinearClient {
    LinearClient::new(config(fake), env_only()).expect("client")
}

fn issues_page(ids: &[&str], next: Option<&str>) -> Value {
    let nodes: Vec<Value> = ids
        .iter()
        .map(|id| json!({"id": id, "title": format!("issue {id}")}))
        .collect();
    json!({"data": {"issues": {
        "nodes": nodes,
        "pageInfo": {"hasNextPage": next.is_some(), "endCursor": next}
    }}})
}

fn after(request: &Recorded) -> Option<String> {
    request.body["variables"]["after"]
        .as_str()
        .map(str::to_string)
}

#[test]
fn paged_read_follows_page_info_to_the_end() {
    let fake = FakeLinear::start(|request, _| match after(request).as_deref() {
        None => Reply::ok(issues_page(&["a", "b"], Some("c1"))),
        Some("c1") => Reply::ok(issues_page(&["c", "d"], Some("c2"))),
        Some("c2") => Reply::ok(issues_page(&["e"], None)),
        Some(other) => panic!("unexpected cursor {other}"),
    });
    let page = client(&fake).project_issues("proj-1").expect("page");

    let ids: Vec<&str> = page
        .nodes
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["a", "b", "c", "d", "e"]);
    assert!(!page.partial);

    let requests = fake.requests();
    assert_eq!(requests.len(), 3);
    for request in &requests {
        assert_eq!(request.authorization.as_deref(), Some(KEY), "no Bearer");
        assert_eq!(request.content_type.as_deref(), Some("application/json"));
        assert_eq!(request.body["variables"]["n"], 2);
        assert_eq!(
            request.body["variables"]["filter"]["project"]["id"]["eq"],
            "proj-1"
        );
    }
    assert_eq!(after(&requests[1]).as_deref(), Some("c1"));
    assert_eq!(after(&requests[2]).as_deref(), Some("c2"));
}

#[test]
fn paged_read_stops_at_the_page_cap_with_a_partial_flag() {
    let fake = FakeLinear::start(|_, index| {
        Reply::ok(issues_page(&["x"], Some(&format!("cursor-{index}"))))
    });
    let mut cfg = config(&fake);
    cfg.page_cap = 3;
    let page = LinearClient::new(cfg, env_only())
        .unwrap()
        .project_issues("p")
        .expect("page");

    assert!(page.partial);
    assert_eq!(page.nodes.len(), 3);
    assert_eq!(fake.requests().len(), 3);
}

#[test]
fn a_next_page_without_a_cursor_is_partial() {
    let fake = FakeLinear::start(|_, _| {
        Reply::ok(json!({"data": {"teams": {
            "nodes": [{"id": "t1", "key": "ENG", "name": "Eng"}],
            "pageInfo": {"hasNextPage": true, "endCursor": null}
        }}}))
    });
    let page = client(&fake).teams().expect("page");
    assert!(page.partial);
    assert_eq!(page.nodes.len(), 1);
    assert_eq!(fake.requests().len(), 1);
}

#[test]
fn a_429_backs_off_once_then_reports_rate_limited() {
    let fake = FakeLinear::start(|_, _| Reply::status(429, json!({"errors": []})));
    let started = Instant::now();
    let error = client(&fake).teams().expect_err("rate limited");

    assert!(matches!(error, LinearError::RateLimited), "{error:?}");
    assert_eq!(fake.requests().len(), 2, "one retry, no more");
    assert!(started.elapsed() >= Duration::from_millis(50), "backed off");
}

#[test]
fn a_graphql_ratelimited_error_backs_off_once_and_can_recover() {
    let fake = FakeLinear::start(|_, index| {
        if index == 0 {
            Reply::status(
                400,
                json!({"errors": [{"message": "Rate limit exceeded",
                                   "extensions": {"code": "RATELIMITED"}}]}),
            )
        } else {
            Reply::ok(json!({"data": {"issue": {"id": "i1", "title": "ok"}}}))
        }
    });
    let issue = client(&fake).issue("ENG-1").expect("recovered");
    assert_eq!(issue["title"], "ok");
    assert_eq!(fake.requests().len(), 2);
}

#[test]
fn a_timeout_reports_unavailable_within_the_budget() {
    let fake = FakeLinear::start(|_, _| Reply::Stall(Duration::from_secs(5)));
    let mut cfg = config(&fake);
    cfg.timeout = Duration::from_millis(300);
    let started = Instant::now();
    let error = LinearClient::new(cfg, env_only())
        .unwrap()
        .teams()
        .expect_err("timed out");

    assert!(matches!(error, LinearError::Unavailable(_)), "{error:?}");
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "took {:?}",
        started.elapsed()
    );
    assert_eq!(fake.requests().len(), 1, "a timeout is not retried");
}

#[test]
fn auth_and_not_found_errors_are_named() {
    let fake = FakeLinear::start(|request, _| {
        if request.body["query"]
            .as_str()
            .unwrap_or("")
            .contains("issue(")
        {
            Reply::ok(json!({"data": {"issue": null}}))
        } else {
            Reply::status(
                400,
                json!({"errors": [{"message": "bad key",
                                   "extensions": {"code": "AUTHENTICATION_ERROR"}}]}),
            )
        }
    });
    let client = client(&fake);
    assert!(matches!(client.teams(), Err(LinearError::Auth)));
    assert!(matches!(
        client.issue("ENG-404"),
        Err(LinearError::NotFound)
    ));
}

#[derive(Clone, Default)]
struct SharedBuf(Arc<Mutex<Vec<u8>>>);

impl Write for SharedBuf {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn the_key_never_appears_in_logs_or_error_text() {
    let fake = FakeLinear::start(|request, index| {
        let echoed = request.authorization.clone().unwrap_or_default();
        match index {
            0 => Reply::status(
                400,
                json!({"errors": [{"message": format!("bad key {echoed}"),
                                   "extensions": {"code": format!("WEIRD_{echoed}")}}]}),
            ),
            1 => Reply::status(500, json!({"message": echoed})),
            _ => Reply::status(
                401,
                json!({"errors": [{"message": echoed,
                    "extensions": {"code": "AUTHENTICATION_ERROR"}}]}),
            ),
        }
    });
    let buf = SharedBuf::default();
    let writer = buf.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();

    let client = client(&fake);
    let errors = tracing::subscriber::with_default(subscriber, || {
        vec![
            client.teams().expect_err("graphql error"),
            client.teams().expect_err("http 500"),
            client.teams().expect_err("auth"),
        ]
    });

    let logs = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert!(
        logs.contains("environment"),
        "the credential source is logged on first use: {logs}"
    );
    assert!(!logs.contains(KEY), "key leaked into logs: {logs}");
    for error in errors {
        assert!(!error.to_string().contains(KEY), "{error}");
        assert!(!format!("{error:?}").contains(KEY), "{error:?}");
    }
    assert!(!format!("{client:?}").contains(KEY));
}

#[test]
fn no_credential_is_reported_without_a_request() {
    let fake = FakeLinear::start(|_, _| Reply::ok(json!({"data": {}})));
    let resolver = CredentialResolver::new(Box::new(NoKeychain), None, None);
    let error = LinearClient::new(config(&fake), resolver)
        .unwrap()
        .teams()
        .expect_err("no key");
    assert!(matches!(error, LinearError::NoCredential), "{error:?}");
    assert!(fake.requests().is_empty());
}

#[test]
fn plain_http_is_refused_off_loopback() {
    let fake = FakeLinear::start(|_, _| Reply::ok(json!({})));
    let mut cfg = config(&fake);
    cfg.api_url = "http://linear.example.com/graphql".to_string();
    assert!(matches!(
        LinearClient::new(cfg, env_only()),
        Err(LinearError::InsecureUrl(_))
    ));
    for url in [
        "https://api.linear.app/graphql",
        "http://127.0.0.1:9/graphql",
        "http://localhost:9/graphql",
        "http://[::1]:9/graphql",
    ] {
        let mut cfg = config(&fake);
        cfg.api_url = url.to_string();
        assert!(LinearClient::new(cfg, env_only()).is_ok(), "{url}");
    }
}

#[test]
fn control_characters_and_bidi_bytes_are_sanitised() {
    let fake = FakeLinear::start(|_, _| {
        Reply::ok(json!({"data": {"issue": {
            "id": "i1",
            "title": "evil\u{1b}[31m red\u{202E}txet\u{200B}",
            "description": "line one\nline\ttwo\u{7}",
            "labels": {"nodes": [{"name": "a\u{2066}b"}]}
        }}}))
    });
    let issue = client(&fake).issue("ENG-1").expect("issue");
    assert_eq!(issue["title"], "evil[31m redtxet");
    assert_eq!(issue["description"], "line one\nline\ttwo");
    assert_eq!(issue["labels"]["nodes"][0]["name"], "ab");
}

#[test]
fn view_issues_pages_with_the_view_filter_and_views_carry_grouping() {
    let filter = json!({"and": [{"project": {"id": {"in": ["p1"]}}}]});
    let view = json!({
        "id": "v1", "name": "Board", "modelName": "Issue", "archivedAt": null,
        "filterData": filter,
        "viewPreferencesValues": {"layout": "board", "issueGrouping": "workflowState",
                                  "issueSubGrouping": "assignee"}
    });
    let view_for_handler = view.clone();
    let fake = FakeLinear::start(move |request, _| {
        let query = request.body["query"].as_str().unwrap_or("");
        if query.contains("customViews(") {
            Reply::ok(json!({"data": {"customViews": {
                "nodes": [view_for_handler.clone()],
                "pageInfo": {"hasNextPage": false, "endCursor": null}
            }}}))
        } else if query.contains("customView(") {
            Reply::ok(json!({"data": {"customView": view_for_handler.clone()}}))
        } else {
            Reply::ok(issues_page(&["a"], None))
        }
    });
    let client = client(&fake);

    let views = client.views().expect("views");
    assert_eq!(
        views.nodes[0]["viewPreferencesValues"]["issueSubGrouping"],
        "assignee"
    );
    let query = fake.requests()[0].body["query"]
        .as_str()
        .unwrap()
        .to_string();
    for field in ["issueGrouping", "issueSubGrouping", "filterData"] {
        assert!(query.contains(field), "views query lacks {field}");
    }

    let page = client.view_issues("v1").expect("issues");
    assert_eq!(page.nodes.len(), 1);
    let requests = fake.requests();
    let issues_request = requests.last().unwrap();
    assert_eq!(issues_request.body["variables"]["filter"], filter);
}

#[test]
fn projects_ask_for_membership_and_page() {
    let fake = FakeLinear::start(|_, _| {
        Reply::ok(json!({"data": {"projects": {
            "nodes": [{"id": "p1", "name": "Proj"}],
            "pageInfo": {"hasNextPage": false, "endCursor": null}
        }}}))
    });
    let page = client(&fake).projects().expect("projects");
    assert_eq!(page.nodes[0]["name"], "Proj");
    assert_eq!(
        fake.requests()[0].body["variables"]["filter"]["members"]["some"]["isMe"]["eq"],
        true
    );
}

fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[test]
fn the_keychain_read_uses_the_exact_argv_and_never_dash_g() {
    let dir = tempfile::tempdir().unwrap();
    let argv = dir.path().join("argv");
    let bin = script(
        dir.path(),
        "security",
        &format!(
            "printf '%s\\n' \"$@\" > '{}'\nprintf 'from-keychain\\n'",
            argv.display()
        ),
    );
    let reader = SecurityCli::new(bin, Duration::from_secs(5));
    match reader.read() {
        KeychainRead::Found(key) => assert_eq!(key.expose(), "from-keychain"),
        other => panic!("expected a key, got {other:?}"),
    }
    let args = std::fs::read_to_string(argv).unwrap();
    assert_eq!(
        args.lines().collect::<Vec<_>>(),
        [
            "find-generic-password",
            "-a",
            "linear-api-key",
            "-s",
            "work-linear",
            "-w"
        ]
    );

    let resolver = CredentialResolver::new(Box::new(reader), Some("env-key".into()), None);
    let (key, source) = resolver.resolve().expect("key");
    assert_eq!(key.expose(), "from-keychain");
    assert_eq!(source, CredentialSource::Keychain);
}

#[test]
fn a_blocking_keychain_read_times_out_and_falls_back_to_the_environment() {
    let dir = tempfile::tempdir().unwrap();
    let bin = script(dir.path(), "security", "exec sleep 30");
    let reader = SecurityCli::new(bin, Duration::from_millis(200));

    let started = Instant::now();
    assert!(matches!(reader.read(), KeychainRead::TimedOut));
    assert!(started.elapsed() < Duration::from_secs(2));

    let resolver = CredentialResolver::new(Box::new(reader), Some("env-key".into()), None);
    let started = Instant::now();
    let (key, source) = resolver.resolve().expect("fallback key");
    assert_eq!(key.expose(), "env-key");
    assert_eq!(source, CredentialSource::Environment);
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn a_missing_keychain_item_falls_through_to_the_secrets_file() {
    let dir = tempfile::tempdir().unwrap();
    let bin = script(dir.path(), "security", "exit 44");
    let secrets = dir.path().join("secrets");
    std::fs::write(
        &secrets,
        "OTHER=1\nLINEAR_API_KEY=\"lin_api_from file\"\r\nLINEAR_API_KEY=second\n",
    )
    .unwrap();
    let resolver = CredentialResolver::new(
        Box::new(SecurityCli::new(bin, Duration::from_secs(5))),
        None,
        Some(secrets),
    );
    let (key, source) = resolver.resolve().expect("key");
    assert_eq!(key.expose(), "lin_api_fromfile");
    assert_eq!(source, CredentialSource::SecretsFile);
}

#[cfg(not(target_os = "macos"))]
#[test]
fn off_macos_the_seam_reports_no_keychain_and_uses_the_fallbacks() {
    assert!(matches!(
        super::platform_keychain().read(),
        KeychainRead::Unsupported
    ));

    let dir = tempfile::tempdir().unwrap();
    let secrets = dir.path().join("secrets");
    std::fs::write(&secrets, "LINEAR_API_KEY=file-key\n").unwrap();

    let env_first =
        CredentialResolver::new(super::platform_keychain(), Some("env-key".into()), None);
    assert_eq!(
        env_first
            .resolve()
            .map(|(k, s)| (k.expose().to_string(), s)),
        Some(("env-key".to_string(), CredentialSource::Environment))
    );

    let file_only = CredentialResolver::new(super::platform_keychain(), None, Some(secrets));
    assert_eq!(
        file_only
            .resolve()
            .map(|(k, s)| (k.expose().to_string(), s)),
        Some(("file-key".to_string(), CredentialSource::SecretsFile))
    );
}

#[test]
fn userinfo_or_a_query_cannot_pass_a_remote_host_off_as_loopback() {
    let fake = FakeLinear::start(|_, _| Reply::ok(json!({})));
    for url in [
        "http://localhost:1@example.com/graphql",
        "http://127.0.0.1@example.com/graphql",
        "http://[::1]@example.com/graphql",
        "http://example.com?x@localhost:1/graphql",
        "http://example.com#@localhost:1/graphql",
    ] {
        let mut cfg = config(&fake);
        cfg.api_url = url.to_string();
        assert!(
            matches!(
                LinearClient::new(cfg, env_only()),
                Err(LinearError::InsecureUrl(_))
            ),
            "{url}"
        );
    }
}

#[test]
fn a_redirect_is_refused_and_the_key_never_reaches_the_target() {
    let target = FakeLinear::start(|_, _| Reply::ok(json!({"data": {"teams": {}}})));
    let location = target.url();
    let fake = FakeLinear::start(move |_, _| Reply::Json {
        status: 302,
        body: json!({}),
        headers: vec![("Location".to_string(), location.clone())],
    });
    let error = client(&fake).teams().expect_err("redirect refused");

    assert!(matches!(error, LinearError::Unavailable(_)), "{error:?}");
    assert_eq!(fake.requests().len(), 1);
    assert!(target.requests().is_empty(), "the redirect was followed");
}

#[test]
fn a_long_retry_after_is_clamped() {
    let fake = FakeLinear::start(|_, _| Reply::Json {
        status: 429,
        body: json!({"errors": []}),
        headers: vec![("Retry-After".to_string(), "3600".to_string())],
    });
    let started = Instant::now();
    let error = client(&fake).teams().expect_err("rate limited");
    let elapsed = started.elapsed();

    assert!(matches!(error, LinearError::RateLimited), "{error:?}");
    assert_eq!(fake.requests().len(), 2);
    assert!(
        elapsed >= Duration::from_millis(4500),
        "Retry-After was not read: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(8),
        "Retry-After was not clamped: {elapsed:?}"
    );
}

#[test]
fn an_oversized_body_is_refused() {
    let limit = usize::try_from(super::client::BODY_LIMIT_BYTES).unwrap();
    let fake = FakeLinear::start(move |_, _| Reply::Bytes {
        status: 200,
        body: vec![b' '; limit + 1],
    });
    let error = client(&fake).teams().expect_err("too large");
    match error {
        LinearError::Unavailable(reason) => assert_eq!(reason, "response too large"),
        other => panic!("expected Unavailable, got {other:?}"),
    }
}

#[derive(Debug)]
struct SlowTimedOutKeychain {
    reads: Arc<AtomicUsize>,
}

impl KeychainReader for SlowTimedOutKeychain {
    fn read(&self) -> KeychainRead {
        self.reads.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(50));
        KeychainRead::TimedOut
    }
}

fn space_fake() -> FakeLinear {
    let view = json!({
        "id": "v1", "name": "Board", "modelName": "Issue", "archivedAt": null,
        "filterData": {"project": {"id": {"eq": "p1"}}},
        "viewPreferencesValues": {"issueGrouping": "workflowState"}
    });
    FakeLinear::start(move |request, _| {
        let query = request.body["query"].as_str().unwrap_or("");
        if query.contains("customView(") {
            Reply::ok(json!({"data": {"customView": view.clone()}}))
        } else if query.contains("project(") {
            Reply::ok(json!({"data": {"project": {"id": "p1", "name": "P"}}}))
        } else {
            Reply::ok(issues_page(&["a"], None))
        }
    })
}

fn view_plan() -> FetchPlan {
    FetchPlan {
        project_id: "p1".to_string(),
        view_id: Some("v1".to_string()),
        filter: None,
        label: "space".to_string(),
    }
}

#[test]
fn one_space_fetch_reads_the_keychain_once() {
    let fake = space_fake();
    let dir = tempfile::tempdir().unwrap();
    let count = dir.path().join("count");
    let bin = script(
        dir.path(),
        "security",
        &format!("echo x >> '{}'\nprintf 'from-keychain\\n'", count.display()),
    );
    let resolver = CredentialResolver::new(
        Box::new(SecurityCli::new(bin, Duration::from_secs(5))),
        None,
        None,
    );
    let client = LinearClient::new(config(&fake), resolver).unwrap();

    let read = fetch(&client, &view_plan()).expect("fetch");
    assert_eq!(read.view_status, "ok");
    assert_eq!(fake.requests().len(), 3, "view, issues, project");
    let reads = std::fs::read_to_string(&count).unwrap().lines().count();
    assert_eq!(reads, 1, "one keychain read per Linear call");
    for request in fake.requests() {
        assert_eq!(request.authorization.as_deref(), Some("from-keychain"));
    }
}

#[test]
fn a_timed_out_keychain_is_skipped_inside_the_backoff_window() {
    let fake = space_fake();
    let reads = Arc::new(AtomicUsize::new(0));
    let resolver = CredentialResolver::new(
        Box::new(SlowTimedOutKeychain {
            reads: reads.clone(),
        }),
        Some(KEY.to_string()),
        None,
    );
    let client = LinearClient::new(config(&fake), resolver).unwrap();

    fetch(&client, &view_plan()).expect("first fetch");
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    fetch(&client, &view_plan()).expect("second fetch");
    assert_eq!(
        reads.load(Ordering::SeqCst),
        1,
        "the keychain was retried inside the backoff window"
    );
    for request in fake.requests() {
        assert_eq!(request.authorization.as_deref(), Some(KEY));
    }
}
