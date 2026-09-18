//! The vendored issue-page fixtures are the contract between this repo and the
//! work plugin's `bin/work-issue.sh`. They are that script's own output against
//! its fake Linear, so a field the plugin stops sending, or starts spelling
//! differently, fails here rather than at a reader's terminal.
//!
//! `VERSION` pins a sha256 per file and the plugin release they came from, the
//! same shape `linear-snapshot/VERSION` uses.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use board_core::protocol::LinearIssueDocument;
use sha2::{Digest, Sha256};

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/linear-issue")
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn pinned() -> (String, BTreeMap<String, String>) {
    let text = std::fs::read_to_string(fixture_dir().join("VERSION")).unwrap();
    let mut lines = text.lines();
    let version = lines
        .next()
        .and_then(|line| line.strip_prefix("plugin_version "))
        .expect("VERSION starts with `plugin_version <semver>`")
        .trim()
        .to_string();
    let hashes = lines
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let (hash, file) = line
                .split_once("  ")
                .unwrap_or_else(|| panic!("VERSION line is not `<sha256>  <file>`: {line}"));
            (file.trim().to_string(), hash.trim().to_string())
        })
        .collect();
    (version, hashes)
}

fn read(file: &str) -> LinearIssueDocument {
    let bytes = std::fs::read(fixture_dir().join(file)).unwrap();
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|e| panic!("fixture {file} does not parse as the board reads it: {e}"))
}

#[test]
fn version_names_the_plugin_release_and_every_fixture() {
    let (version, hashes) = pinned();
    assert_eq!(version, "0.5.0");
    assert_eq!(hashes.len(), 4, "{hashes:?}");
}

#[test]
fn every_pinned_fixture_exists_and_matches_its_hash() {
    let (_, hashes) = pinned();
    for (file, expected) in &hashes {
        let bytes = std::fs::read(fixture_dir().join(file))
            .unwrap_or_else(|e| panic!("fixture {file} named in VERSION is missing: {e}"));
        assert_eq!(
            &sha256_hex(&bytes),
            expected,
            "fixture {file} drifted from VERSION"
        );
    }
}

#[test]
fn every_fixture_in_the_directory_is_pinned() {
    let (_, hashes) = pinned();
    for entry in std::fs::read_dir(fixture_dir()).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        if name == "VERSION" {
            continue;
        }
        assert!(hashes.contains_key(&name), "{name} is not pinned in VERSION");
    }
}

/// The whole document, as the page reads it: nothing the plugin sends is lost
/// on the way into the board's own types.
#[test]
fn the_full_fixture_carries_every_section_the_page_draws() {
    let doc = read("full.json");
    assert_eq!(doc.schema, 1);
    assert_eq!(doc.status, "ok");
    assert!(doc.truncated.is_empty());

    let issue = doc.issue.expect("a document with status ok carries an issue");
    assert_eq!(issue.identifier, "WEB-3318");
    assert!(issue.description.unwrap().contains("What happens"));
    assert_eq!(issue.estimate, Some(3.0));
    assert_eq!(issue.due_date.as_deref(), Some("2026-09-30"));
    assert_eq!(issue.milestone.unwrap().name.as_deref(), Some("M2"));
    assert_eq!(issue.cycle.unwrap().number, Some(14));
    assert_eq!(issue.children.len(), 2);
    assert_eq!(issue.comments.len(), 2);
    assert!(!issue.history.is_empty());

    // R9 -- every linked row can open its own page from the row alone.
    let linked = [issue.parent.clone().unwrap()]
        .into_iter()
        .chain(issue.children.clone())
        .chain(issue.relations.iter().map(|r| r.issue.clone()));
    for row in linked {
        assert!(!row.identifier.is_empty(), "{row:?}");
        assert!(!row.title.is_empty(), "{row:?}");
        assert!(row.state.name.is_some(), "{row:?}");
    }

    // Both ends of a relation, which is what separates blocks from blocked by.
    let directions: Vec<&str> = issue
        .relations
        .iter()
        .map(|r| r.direction.as_str())
        .collect();
    assert!(directions.contains(&"outward"), "{directions:?}");
    assert!(directions.contains(&"inward"), "{directions:?}");

    // A reply names the comment it answers.
    assert!(issue.comments.iter().any(|c| c.parent_id.is_some()));
}

/// R4 -- an issue with nothing set parses, with every absent property null and
/// every empty connection an empty list rather than a missing key.
#[test]
fn the_empty_fixture_parses_with_nulls_not_errors() {
    let doc = read("empty.json");
    assert_eq!(doc.status, "ok");
    let issue = doc.issue.expect("an empty issue is still an issue");
    assert!(issue.description.is_none());
    assert!(issue.milestone.is_none());
    assert!(issue.cycle.is_none());
    assert!(issue.parent.is_none());
    assert!(issue.estimate.is_none());
    assert!(issue.children.is_empty());
    assert!(issue.relations.is_empty());
    assert!(issue.comments.is_empty());
    assert!(issue.history.is_empty());
}

/// R8a -- a read that stopped at its page cap says so, and names what it cut.
#[test]
fn the_truncated_fixture_is_partial_and_names_what_was_cut() {
    let doc = read("truncated.json");
    assert_eq!(doc.status, "partial");
    assert!(!doc.truncated.is_empty());
    for section in &doc.truncated {
        assert!(
            ["children", "comments", "history", "relations"].contains(&section.as_str()),
            "unknown truncated section {section:?}"
        );
    }
    assert!(doc.message.unwrap().contains("Linear"));
    assert!(doc.issue.is_some(), "a partial read still carries the issue");
}

/// A reachability failure arrives as a document, not an error code, so the page
/// can keep what it already shows and offer a retry.
#[test]
fn the_unavailable_fixture_is_a_document_with_no_issue() {
    let doc = read("unavailable.json");
    assert_eq!(doc.schema, 1);
    assert_eq!(doc.status, "unavailable");
    assert!(doc.issue.is_none());
    assert!(doc.message.is_some());
}
