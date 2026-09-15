//! The vendored snapshot fixtures are the contract between this repo and the
//! work plugin: `VERSION` pins a sha256 per file, and every file must parse.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use board_core::protocol::LinearSnapshot;
use sha2::{Digest, Sha256};

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/linear-snapshot")
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// `(plugin_version, {file_name: sha256})` from `VERSION`.
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

#[test]
fn version_names_the_plugin_release_and_at_least_one_fixture() {
    let (version, hashes) = pinned();
    assert_eq!(version, "0.3.0");
    assert!(!hashes.is_empty());
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
fn every_fixture_on_disk_is_pinned() {
    let (_, hashes) = pinned();
    for entry in std::fs::read_dir(fixture_dir()).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        if name == "VERSION" {
            continue;
        }
        assert!(
            hashes.contains_key(&name),
            "fixture {name} is not pinned in VERSION"
        );
    }
}

#[test]
fn every_pinned_fixture_deserialises_into_linear_snapshot() {
    let (_, hashes) = pinned();
    for file in hashes.keys() {
        let text = std::fs::read_to_string(fixture_dir().join(file)).unwrap();
        let snapshot: LinearSnapshot = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("fixture {file} does not parse: {e}"));
        assert_eq!(snapshot.schema, 1, "fixture {file}");
        assert_eq!(snapshot.workspace.id, "wA", "fixture {file}");
        assert!(snapshot.pane_status.is_empty(), "fixture {file}");
    }
}

#[test]
fn bound_with_view_carries_bindings_panes_and_unmapped_tabs() {
    let text = std::fs::read_to_string(fixture_dir().join("bound-with-view.json")).unwrap();
    let snapshot: LinearSnapshot = serde_json::from_str(&text).unwrap();
    let issue = &snapshot.issues["WEB-3312"];
    assert_eq!(issue.state.kind.as_deref(), Some("started"));
    assert_eq!(issue.bindings[0].tab.as_ref().unwrap().id, "wA:t1");
    assert_eq!(issue.bindings[0].panes, vec!["wA:p1", "wA:p2"]);
    assert_eq!(snapshot.unmapped[0].reason, "no_binding");
    assert_eq!(
        snapshot.view.layout.as_ref().unwrap().grouping,
        "workflowState"
    );
    assert_eq!(snapshot.pane_ids(), vec!["wA:p1", "wA:p2", "wA:p9"]);
}

#[test]
fn a_document_missing_unmapped_still_parses_with_an_empty_list() {
    let text = std::fs::read_to_string(fixture_dir().join("bound-with-view.json")).unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&text).unwrap();
    value.as_object_mut().unwrap().remove("unmapped");
    let snapshot: LinearSnapshot = serde_json::from_value(value).unwrap();
    assert!(snapshot.unmapped.is_empty());
    assert_eq!(snapshot.issues.len(), 3);
}

#[test]
fn a_one_byte_change_fails_the_pinned_hash() {
    let (_, hashes) = pinned();
    let file = "bound-with-view.json";
    let mut bytes = std::fs::read(fixture_dir().join(file)).unwrap();
    assert_eq!(&sha256_hex(&bytes), &hashes[file]);
    let last = bytes.len() - 1;
    bytes[last] ^= 0x01;
    assert_ne!(&sha256_hex(&bytes), &hashes[file]);
}

/// With `BOARD_WORK_PLUGIN_CHECKOUT` naming a work plugin checkout
/// (`plugins/work`), its fixtures must be these fixtures byte for byte. The
/// plugin's suite proves `bin/work-snapshot.sh` prints those files, so the two
/// checks together carry real plugin output to the board's types. Unset, the
/// test says so and passes: CI here has no plugin checkout.
#[test]
fn a_plugin_checkout_carries_exactly_these_fixtures() {
    let Some(checkout) = std::env::var_os("BOARD_WORK_PLUGIN_CHECKOUT") else {
        eprintln!("BOARD_WORK_PLUGIN_CHECKOUT unset: plugin fixture parity not checked");
        return;
    };
    let theirs = Path::new(&checkout).join("tests/fixtures/snapshot");
    let (_, hashes) = pinned();
    let mut on_their_side: Vec<String> = std::fs::read_dir(&theirs)
        .unwrap_or_else(|e| panic!("{} is not a plugin checkout: {e}", theirs.display()))
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".json"))
        .collect();
    on_their_side.sort();
    let pinned_names: Vec<String> = hashes.keys().cloned().collect();
    assert_eq!(
        on_their_side, pinned_names,
        "the two fixture sets name different files"
    );
    for file in &pinned_names {
        let mine = std::fs::read(fixture_dir().join(file)).unwrap();
        let other = std::fs::read(theirs.join(file)).unwrap();
        assert!(
            mine == other,
            "{file} differs from the plugin checkout's copy"
        );
        serde_json::from_slice::<LinearSnapshot>(&other)
            .unwrap_or_else(|e| panic!("the plugin's {file} does not parse: {e}"));
    }
    let manifest =
        std::fs::read_to_string(Path::new(&checkout).join(".claude-plugin/plugin.json")).unwrap();
    let (version, _) = pinned();
    assert!(
        manifest.contains(&format!("\"version\": \"{version}\""))
            || manifest.contains(&format!("\"version\":\"{version}\"")),
        "the checkout is not plugin {version}: {manifest}"
    );
}
