pub(crate) mod board;
pub(crate) mod caller;
pub(crate) mod card;
pub(crate) mod column;
pub(crate) mod discovery;
pub(crate) mod import;
pub(crate) mod linear_report;
pub(crate) mod linear_session;
pub(crate) mod project;
pub(crate) mod run;
pub(crate) mod template;

/// Canonical when the path resolves, otherwise the path as given.
pub(crate) fn canonical_text(path: std::path::PathBuf) -> String {
    path.canonicalize()
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}
