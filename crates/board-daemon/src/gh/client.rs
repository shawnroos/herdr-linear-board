//! Runs `gh` as a child: an allowlisted environment, drained and capped
//! pipes, and a deadline. Never logs argv or env.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use board_core::cli_child::{kill_group_and_reap, spawn_in_own_group};
use board_core::protocol::LinearPrDetail;

use super::parse;
use super::PrRef;

const POLL: Duration = Duration::from_millis(10);

// A `gh pr view` with a long check rollup is tens of KiB; a cap far above
// that only stops a runaway child from exhausting memory.
pub const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

/// gh's documented exit code for "authentication required".
const EXIT_AUTH: i32 = 4;

#[derive(Clone, PartialEq, Eq)]
pub struct GhConfig {
    pub bin: String,
    pub host: Option<String>,
    /// The whole child environment; nothing else is inherited.
    pub env: Vec<(String, String)>,
    pub timeout: Duration,
}

impl std::fmt::Debug for GhConfig {
    // The env carries tokens, so only its keys are printed.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GhConfig")
            .field("bin", &self.bin)
            .field("host", &self.host)
            .field(
                "env",
                &self.env.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
            )
            .field("timeout", &self.timeout)
            .finish()
    }
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum GhError {
    #[error("{0}")]
    Unavailable(String),
    #[error("gh timed out after {0:?} and was killed")]
    Timeout(Duration),
    #[error("gh failed: {0}")]
    Failed(String),
    #[error("gh printed more than {MAX_OUTPUT_BYTES} bytes")]
    TooLarge,
    #[error("gh printed output that is not the expected JSON")]
    Unreadable,
}

pub struct Output {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl Output {
    fn stderr_line(&self) -> String {
        let text = String::from_utf8_lossy(&self.stderr);
        let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        let line: String = line.chars().filter(|c| !c.is_control()).take(200).collect();
        match (line.trim(), self.code) {
            ("", Some(code)) => format!("exit status {code}"),
            ("", None) => "killed by a signal".into(),
            (line, _) => line.to_string(),
        }
    }
}

pub struct GhClient {
    config: GhConfig,
}

impl GhClient {
    pub fn new(config: GhConfig) -> GhClient {
        GhClient { config }
    }

    pub fn config(&self) -> &GhConfig {
        &self.config
    }

    /// One `gh` run. A non-zero exit is an `Output`, not an error: `gh pr
    /// checks` exits 8 for pending checks and still prints its JSON.
    pub fn run(&self, args: &[String]) -> Result<Output, GhError> {
        let mut child = spawn_in_own_group(
            Command::new(&self.config.bin)
                .args(args)
                .env_clear()
                .envs(self.config.env.iter().map(|(k, v)| (k, v)))
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .map_err(|e| {
            GhError::Unavailable(match e.kind() {
                std::io::ErrorKind::NotFound => {
                    format!("`{}` is not installed or not on PATH", self.config.bin)
                }
                _ => format!("`{}` could not start: {e}", self.config.bin),
            })
        })?;
        let stdout = drain(child.stdout.take());
        let stderr = drain(child.stderr.take());
        let deadline = Instant::now() + self.config.timeout;
        let timed_out = |child: &mut Child| {
            kill_group_and_reap(child);
            GhError::Timeout(self.config.timeout)
        };
        // Pipes before the exit status: the child stays unreaped until both
        // pipes close, so its group id cannot be reused when the group is killed.
        while !(stdout.is_finished() && stderr.is_finished()) {
            if Instant::now() >= deadline {
                return Err(timed_out(&mut child));
            }
            std::thread::sleep(POLL);
        }
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => std::thread::sleep(POLL),
                _ => return Err(timed_out(&mut child)),
            }
        };
        let (stdout, out_over) = stdout.join().unwrap_or_default();
        let (stderr, _) = stderr.join().unwrap_or_default();
        if out_over {
            return Err(GhError::TooLarge);
        }
        Ok(Output {
            code: status.code(),
            stdout,
            stderr,
        })
    }

    fn json(&self, args: &[String]) -> Result<serde_json::Value, GhError> {
        let out = self.run(args)?;
        match out.code {
            Some(0) => serde_json::from_slice(&out.stdout).map_err(|_| GhError::Unreadable),
            Some(EXIT_AUTH) => Err(self.logged_out(&out)),
            _ => Err(GhError::Failed(out.stderr_line())),
        }
    }

    fn logged_out(&self, out: &Output) -> GhError {
        GhError::Unavailable(format!(
            "gh is not logged in ({}); run `gh auth login`",
            out.stderr_line()
        ))
    }

    /// Up to four calls: the PR, its required checks, and the newest
    /// deployment's statuses for an open PR's preview, skipped when the
    /// checks call timed out. Past the first call, only a missing or
    /// logged-out `gh` fails the read.
    pub fn fetch(&self, pr: &PrRef) -> Result<LinearPrDetail, GhError> {
        let number = pr.number.to_string();
        let repo = pr.repo_arg();
        let arg = |s: &str| s.to_string();
        let view = self.json(&[
            arg("pr"),
            arg("view"),
            number.clone(),
            arg("-R"),
            repo.clone(),
            arg("--json"),
            arg(parse::VIEW_FIELDS),
        ])?;
        let checks = self.run(&[
            arg("pr"),
            arg("checks"),
            number,
            arg("-R"),
            repo,
            arg("--required"),
            arg("--json"),
            arg(parse::CHECKS_FIELDS),
        ]);
        // Each preview call waits the full timeout too; a gh that just hung
        // would stall this read for two more of them.
        let timed_out = matches!(checks, Err(GhError::Timeout(_)));
        let required = match checks {
            Ok(out) if out.code == Some(EXIT_AUTH) => return Err(self.logged_out(&out)),
            Ok(out) => parse::required_checks(&out.stdout),
            Err(error @ GhError::Unavailable(_)) => return Err(error),
            Err(_) => None,
        };
        let mut detail = parse::pr_detail(&view, required.as_deref());
        detail.repo = pr.repo();
        detail.number = pr.number;
        if detail.url.is_empty() {
            detail.url = pr.url();
        }
        if !timed_out && detail.state.as_deref() == Some("OPEN") {
            if let Some(branch) = detail.head_ref.clone() {
                detail.preview = self.preview(pr, &branch);
            }
        }
        Ok(detail)
    }

    fn preview(&self, pr: &PrRef, branch: &str) -> Option<String> {
        let api = |path: String, fields: &[String]| -> Option<Vec<u8>> {
            let mut args = vec![
                "api".to_string(),
                "--method".into(),
                "GET".into(),
                "--hostname".into(),
                pr.host.clone(),
                path,
            ];
            for field in fields {
                args.push("-f".into());
                args.push(field.clone());
            }
            let out = self.run(&args).ok()?;
            (out.code == Some(0)).then_some(out.stdout)
        };
        let base = format!("repos/{}/{}/deployments", pr.owner, pr.repo);
        let deployments = api(
            base.clone(),
            &[format!("ref={branch}"), "per_page=10".into()],
        )?;
        let id = parse::newest_deployment(&deployments)?;
        let statuses = api(format!("{base}/{id}/statuses"), &["per_page=20".into()])?;
        parse::preview_url(&statuses)
    }
}

/// Reads a pipe to EOF, keeping at most [`MAX_OUTPUT_BYTES`]; the flag says
/// output was dropped. Reading on past the cap keeps the child from blocking
/// on a full pipe until the deadline.
fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> JoinHandle<(Vec<u8>, bool)> {
    std::thread::spawn(move || {
        let mut kept = Vec::new();
        let mut over = false;
        let Some(mut pipe) = pipe else {
            return (kept, over);
        };
        let mut chunk = [0_u8; 8 * 1024];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) if kept.len() + n <= MAX_OUTPUT_BYTES => kept.extend_from_slice(&chunk[..n]),
                Ok(_) => over = true,
            }
        }
        (kept, over)
    })
}
