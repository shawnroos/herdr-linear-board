use std::time::Duration;

use board_core::text::{sanitise_json, strip_control_and_format};
use serde_json::{json, Value};

use super::credential::{ApiKey, CredentialResolver};

pub const DEFAULT_API_URL: &str = "https://api.linear.app/graphql";
pub const API_URL_ENV: &str = "BOARD_LINEAR_API_URL";

// Carried from the script era (`ops/linear.rs`): 8 s per call, and a page cap
// so one listing cannot run past the snapshot deadline.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(8);
const DEFAULT_PAGE_CAP: usize = 10;
const DEFAULT_PAGE_SIZE: u32 = 50;
// The plugin's backoff base. Linear allows 2,500 requests an hour per key, so
// a longer wait would not clear an hourly limit and only stalls the caller.
const DEFAULT_RATE_LIMIT_BACKOFF: Duration = Duration::from_millis(500);
const MAX_RATE_LIMIT_BACKOFF: Duration = Duration::from_secs(5);
const BODY_LIMIT_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct LinearConfig {
    pub api_url: String,
    pub timeout: Duration,
    pub rate_limit_retries: u32,
    pub rate_limit_backoff: Duration,
    pub page_cap: usize,
    pub page_size: u32,
}

impl Default for LinearConfig {
    fn default() -> Self {
        LinearConfig {
            api_url: DEFAULT_API_URL.to_string(),
            timeout: DEFAULT_TIMEOUT,
            rate_limit_retries: 1,
            rate_limit_backoff: DEFAULT_RATE_LIMIT_BACKOFF,
            page_cap: DEFAULT_PAGE_CAP,
            page_size: DEFAULT_PAGE_SIZE,
        }
    }
}

impl LinearConfig {
    pub fn from_env() -> Self {
        let mut config = LinearConfig::default();
        if let Ok(url) = std::env::var(API_URL_ENV) {
            if !url.trim().is_empty() {
                config.api_url = url.trim().to_string();
            }
        }
        config
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LinearError {
    #[error("no Linear API key: none in the keychain, LINEAR_API_KEY or ~/.secrets")]
    NoCredential,
    #[error("Linear refused the API key")]
    Auth,
    #[error("Linear has no such item")]
    NotFound,
    #[error("Linear rate limit reached; try again later")]
    RateLimited,
    #[error("Linear is unavailable: {0}")]
    Unavailable(String),
    #[error("refusing Linear API URL {0}: plain http is allowed only to a loopback host")]
    InsecureUrl(String),
}

#[derive(Clone, Debug, Default)]
pub struct Page {
    pub nodes: Vec<Value>,
    pub partial: bool,
}

pub struct LinearClient {
    config: LinearConfig,
    agent: ureq::Agent,
    credentials: CredentialResolver,
}

impl std::fmt::Debug for LinearClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinearClient")
            .field("config", &self.config)
            .field("credentials", &self.credentials)
            .finish_non_exhaustive()
    }
}

enum Attempt {
    RateLimited(Option<Duration>),
    Failed(LinearError),
}

impl LinearClient {
    pub fn new(config: LinearConfig, credentials: CredentialResolver) -> Result<Self, LinearError> {
        let loopback = check_url(&config.api_url)?;
        let mut builder = ureq::Agent::config_builder()
            .timeout_global(Some(config.timeout))
            .http_status_as_error(false)
            .max_redirects(0);
        if loopback {
            builder = builder.proxy(None);
        }
        let agent = builder.build().new_agent();
        Ok(LinearClient {
            config,
            agent,
            credentials,
        })
    }

    pub fn from_env() -> Result<Self, LinearError> {
        LinearClient::new(LinearConfig::from_env(), CredentialResolver::from_process())
    }

    pub(super) fn key(&self) -> Result<ApiKey, LinearError> {
        self.credentials
            .resolve()
            .map(|(key, _)| key)
            .ok_or(LinearError::NoCredential)
    }

    /// The sanitised `data` object of one GraphQL call.
    pub(super) fn execute(
        &self,
        key: &ApiKey,
        query: &str,
        variables: Value,
    ) -> Result<Value, LinearError> {
        let body = json!({"query": query, "variables": variables}).to_string();
        let mut retries = 0;
        loop {
            match self.post(key, &body) {
                Ok(data) => return Ok(data),
                Err(Attempt::Failed(error)) => return Err(error),
                Err(Attempt::RateLimited(retry_after)) => {
                    if retries >= self.config.rate_limit_retries {
                        return Err(LinearError::RateLimited);
                    }
                    retries += 1;
                    let wait = retry_after
                        .unwrap_or(self.config.rate_limit_backoff)
                        .min(MAX_RATE_LIMIT_BACKOFF);
                    std::thread::sleep(wait);
                }
            }
        }
    }

    /// Follows `pageInfo` until the connection ends or `page_cap` pages are
    /// read. `variables` gets `n` and `after` added.
    pub(super) fn paged(
        &self,
        key: &ApiKey,
        query: &str,
        connection: &str,
        mut variables: Value,
    ) -> Result<Page, LinearError> {
        let mut page = Page::default();
        let mut after: Option<String> = None;
        for _ in 0..self.config.page_cap {
            variables["n"] = json!(self.config.page_size);
            variables["after"] = json!(after);
            let data = self.execute(key, query, variables.clone())?;
            let conn = &data[connection];
            let Some(nodes) = conn["nodes"].as_array() else {
                return Err(malformed());
            };
            page.nodes.extend(nodes.iter().cloned());
            if conn["pageInfo"]["hasNextPage"].as_bool() != Some(true) {
                return Ok(page);
            }
            match conn["pageInfo"]["endCursor"].as_str() {
                Some(cursor) if !cursor.is_empty() => after = Some(cursor.to_string()),
                _ => {
                    page.partial = true;
                    return Ok(page);
                }
            }
        }
        page.partial = true;
        Ok(page)
    }

    fn post(&self, key: &ApiKey, body: &str) -> Result<Value, Attempt> {
        let response = self
            .agent
            .post(&self.config.api_url)
            .header("Authorization", key.expose())
            .header("Content-Type", "application/json")
            .send(body);
        let mut response = match response {
            Ok(response) => response,
            Err(error) => return Err(Attempt::Failed(transport_error(&error, &self.config))),
        };
        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        if status == 429 {
            return Err(Attempt::RateLimited(retry_after));
        }
        let text = response
            .body_mut()
            .with_config()
            .limit(BODY_LIMIT_BYTES)
            .read_to_string()
            .map_err(|error| Attempt::Failed(transport_error(&error, &self.config)))?;
        let parsed: Value = match serde_json::from_str(&text) {
            Ok(value) => value,
            Err(_) if !(200..300).contains(&status) => {
                return Err(Attempt::Failed(http_status_error(status)))
            }
            Err(_) => return Err(Attempt::Failed(malformed())),
        };
        if let Some(first) = parsed["errors"]
            .as_array()
            .and_then(|errors| errors.first())
        {
            return Err(graphql_error(first, status));
        }
        if status == 401 || status == 403 {
            return Err(Attempt::Failed(LinearError::Auth));
        }
        if !(200..300).contains(&status) {
            return Err(Attempt::Failed(http_status_error(status)));
        }
        match parsed.get("data") {
            Some(data) if data.is_object() => Ok(sanitise_json(data.clone())),
            _ => Err(Attempt::Failed(malformed())),
        }
    }
}

fn graphql_error(error: &Value, status: u16) -> Attempt {
    let code = error["extensions"]["code"].as_str().unwrap_or("");
    match code {
        "RATELIMITED" => Attempt::RateLimited(None),
        "AUTHENTICATION_ERROR" | "FORBIDDEN" => Attempt::Failed(LinearError::Auth),
        "INPUT_ERROR" | "NOT_FOUND" => Attempt::Failed(LinearError::NotFound),
        _ if status == 401 || status == 403 => Attempt::Failed(LinearError::Auth),
        // The code comes from the server and can echo request text, so only a
        // short, identifier-shaped code is repeated back.
        _ if is_identifier(code) => Attempt::Failed(LinearError::Unavailable(format!(
            "Linear answered {}",
            strip_control_and_format(code)
        ))),
        _ => Attempt::Failed(LinearError::Unavailable(format!(
            "Linear answered an error (HTTP {status})"
        ))),
    }
}

fn is_identifier(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= 40
        && code
            .chars()
            .all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit())
}

fn http_status_error(status: u16) -> LinearError {
    LinearError::Unavailable(format!("HTTP {status}"))
}

fn malformed() -> LinearError {
    LinearError::Unavailable("unreadable response".to_string())
}

// Built from the error's kind only, so no request text can reach the message.
fn transport_error(error: &ureq::Error, config: &LinearConfig) -> LinearError {
    let reason = match error {
        ureq::Error::Timeout(_) => format!("no answer within {:?}", config.timeout),
        ureq::Error::Io(io) if io.kind() == std::io::ErrorKind::TimedOut => {
            format!("no answer within {:?}", config.timeout)
        }
        ureq::Error::Io(io) => format!("connection failed ({:?})", io.kind()),
        ureq::Error::HostNotFound => "host not found".to_string(),
        ureq::Error::ConnectionFailed => "connection failed".to_string(),
        ureq::Error::BodyExceedsLimit(_) => "response too large".to_string(),
        ureq::Error::Tls(_) | ureq::Error::Rustls(_) | ureq::Error::Pem(_) => {
            "TLS failed".to_string()
        }
        _ => "request failed".to_string(),
    };
    LinearError::Unavailable(reason)
}

/// Returns whether the URL is plain http to a loopback host. The key travels
/// in a header, so plain http anywhere else would send it in the clear.
fn check_url(url: &str) -> Result<bool, LinearError> {
    if url.starts_with("https://") {
        return Ok(false);
    }
    let Some(rest) = url.strip_prefix("http://") else {
        return Err(LinearError::InsecureUrl(strip_control_and_format(url)));
    };
    let authority = rest.split('/').next().unwrap_or("");
    let host = if let Some(bracketed) = authority.strip_prefix('[') {
        bracketed.split(']').next().unwrap_or("")
    } else {
        authority.split(':').next().unwrap_or("")
    };
    if matches!(host, "127.0.0.1" | "localhost" | "::1") {
        Ok(true)
    } else {
        Err(LinearError::InsecureUrl(strip_control_and_format(url)))
    }
}
