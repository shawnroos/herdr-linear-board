use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const KEYCHAIN_SERVICE: &str = "work-linear";
pub const KEYCHAIN_ACCOUNT: &str = "linear-api-key";
pub const SECURITY_BIN: &str = "/usr/bin/security";
pub const KEYCHAIN_TIMEOUT: Duration = Duration::from_secs(5);
const ENV_KEY: &str = "LINEAR_API_KEY";

#[derive(Clone)]
pub struct ApiKey(String);

impl ApiKey {
    fn new(value: &str) -> Option<Self> {
        let value = value.trim();
        (!value.is_empty()).then(|| ApiKey(value.to_string()))
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiKey([redacted])")
    }
}

#[derive(Debug)]
pub enum KeychainRead {
    Found(ApiKey),
    Absent,
    TimedOut,
    Unsupported,
}

pub trait KeychainReader: Send + Sync + std::fmt::Debug {
    fn read(&self) -> KeychainRead;
}

#[derive(Debug)]
pub struct NoKeychain;

impl KeychainReader for NoKeychain {
    fn read(&self) -> KeychainRead {
        KeychainRead::Unsupported
    }
}

/// Shells out to `/usr/bin/security` rather than linking a keychain crate: the
/// item's default ACL trusts that binary, so an unsigned `board` reading the
/// item directly could raise an access prompt.
#[derive(Debug)]
pub struct SecurityCli {
    bin: PathBuf,
    timeout: Duration,
}

impl SecurityCli {
    pub fn new(bin: PathBuf, timeout: Duration) -> Self {
        SecurityCli { bin, timeout }
    }

    pub fn system() -> Self {
        SecurityCli::new(PathBuf::from(SECURITY_BIN), KEYCHAIN_TIMEOUT)
    }
}

impl KeychainReader for SecurityCli {
    fn read(&self) -> KeychainRead {
        // `-w` prints only the password on stdout. `-g` is never used: it
        // writes the secret to stderr.
        let child = Command::new(&self.bin)
            .args([
                "find-generic-password",
                "-a",
                KEYCHAIN_ACCOUNT,
                "-s",
                KEYCHAIN_SERVICE,
                "-w",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut child) = child else {
            return KeychainRead::Absent;
        };
        // A locked keychain holds this call on an unlock prompt nobody can
        // answer from the daemon, so the read is bounded and then killed.
        let deadline = Instant::now() + self.timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(None) | Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return KeychainRead::TimedOut;
                }
            }
        };
        if !status.success() {
            return KeychainRead::Absent;
        }
        let mut out = String::new();
        if let Some(mut stdout) = child.stdout.take() {
            if stdout.read_to_string(&mut out).is_err() {
                return KeychainRead::Absent;
            }
        }
        match ApiKey::new(&out) {
            Some(key) => KeychainRead::Found(key),
            None => KeychainRead::Absent,
        }
    }
}

pub fn platform_keychain() -> Box<dyn KeychainReader> {
    if cfg!(target_os = "macos") {
        Box::new(SecurityCli::system())
    } else {
        Box::new(NoKeychain)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialSource {
    Keychain,
    Environment,
    SecretsFile,
}

impl CredentialSource {
    pub fn as_str(self) -> &'static str {
        match self {
            CredentialSource::Keychain => "keychain",
            CredentialSource::Environment => "environment",
            CredentialSource::SecretsFile => "secrets-file",
        }
    }
}

/// Keychain first, then `LINEAR_API_KEY` from boardd's startup environment,
/// then `~/.secrets`. The keychain and the file are read on every call.
#[derive(Debug)]
pub struct CredentialResolver {
    keychain: Box<dyn KeychainReader>,
    env_key: Option<ApiKey>,
    secrets_file: Option<PathBuf>,
    logged: Mutex<Option<CredentialSource>>,
}

impl CredentialResolver {
    pub fn new(
        keychain: Box<dyn KeychainReader>,
        env_key: Option<String>,
        secrets_file: Option<PathBuf>,
    ) -> Self {
        CredentialResolver {
            keychain,
            env_key: env_key.as_deref().and_then(ApiKey::new),
            secrets_file,
            logged: Mutex::new(None),
        }
    }

    pub fn from_process() -> Self {
        let secrets = std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".secrets"));
        CredentialResolver::new(platform_keychain(), std::env::var(ENV_KEY).ok(), secrets)
    }

    pub fn resolve(&self) -> Option<(ApiKey, CredentialSource)> {
        let found = self.find();
        if let Some((_, source)) = &found {
            self.note(*source);
        }
        found
    }

    fn find(&self) -> Option<(ApiKey, CredentialSource)> {
        match self.keychain.read() {
            KeychainRead::Found(key) => return Some((key, CredentialSource::Keychain)),
            KeychainRead::TimedOut => {
                tracing::warn!("linear: keychain read timed out; trying the fallbacks");
            }
            KeychainRead::Absent | KeychainRead::Unsupported => {}
        }
        if let Some(key) = &self.env_key {
            return Some((key.clone(), CredentialSource::Environment));
        }
        let contents = std::fs::read_to_string(self.secrets_file.as_ref()?).ok()?;
        parse_secrets(&contents).map(|key| (key, CredentialSource::SecretsFile))
    }

    // The first client to start boardd decides its environment, so which
    // source answered is logged; the value never is.
    fn note(&self, source: CredentialSource) {
        let Ok(mut logged) = self.logged.lock() else {
            return;
        };
        if *logged != Some(source) {
            tracing::info!(source = source.as_str(), "linear: using API key");
            *logged = Some(source);
        }
    }
}

// Same reading as the plugin's `lib/linear.sh`: the first `LINEAR_API_KEY=`
// line, with quotes, spaces and carriage returns removed.
fn parse_secrets(contents: &str) -> Option<ApiKey> {
    let line = contents
        .lines()
        .find(|line| line.starts_with("LINEAR_API_KEY="))?;
    let value: String = line["LINEAR_API_KEY=".len()..]
        .chars()
        .filter(|c| !matches!(c, '"' | '\'' | ' ' | '\r'))
        .collect();
    ApiKey::new(&value)
}
