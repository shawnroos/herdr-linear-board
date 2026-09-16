//! The keys a person has set in their herdr config, for the Linear `?` sheet
//! (R7, KTD14). Read once at start; any failure means no section, never an
//! error. The rows never join `view::HELP_KEYS`.

use std::ffi::OsString;
use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::de::{Deserializer, MapAccess, Visitor};
use serde::Deserialize;

pub const MAX_CONFIG_BYTES: u64 = 64 * 1024;
const MAX_FIELD_CHARS: usize = 120;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HerdrKey {
    pub key: String,
    pub label: String,
}

pub fn resolve_path(xdg_config_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let non_empty = |v: Option<OsString>| v.filter(|v| !v.is_empty()).map(PathBuf::from);
    non_empty(xdg_config_home)
        .or_else(|| non_empty(home).map(|home| home.join(".config")))
        .map(|base| base.join("herdr").join("config.toml"))
}

pub fn from_environment() -> Vec<HerdrKey> {
    resolve_path(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )
    .map(|path| read_file(&path))
    .unwrap_or_default()
}

pub fn read_file(path: &Path) -> Vec<HerdrKey> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Vec::new();
    };
    if meta.len() > MAX_CONFIG_BYTES {
        return Vec::new();
    }
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    // The file can grow between the metadata call and the read.
    let mut text = String::new();
    match file.take(MAX_CONFIG_BYTES + 1).read_to_string(&mut text) {
        Ok(n) if n as u64 <= MAX_CONFIG_BYTES => parse(&text),
        _ => Vec::new(),
    }
}

pub fn parse(text: &str) -> Vec<HerdrKey> {
    let Ok(config) = toml::from_str::<Config>(text) else {
        return Vec::new();
    };
    config
        .keys
        .map(|keys| keys.0)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(key, label)| {
            let key = clean(&key);
            let label = clean(&label);
            (!key.is_empty() && !label.is_empty()).then_some(HerdrKey { key, label })
        })
        .collect()
}

fn clean(s: &str) -> String {
    board_core::text::strip_control_and_format(s)
        .trim()
        .chars()
        .take(MAX_FIELD_CHARS)
        .collect()
}

#[derive(Deserialize)]
struct Config {
    keys: Option<Keys>,
}

#[derive(Deserialize)]
struct Command {
    key: Option<String>,
    command: Option<String>,
    description: Option<String>,
}

/// `(key, label)` rows in file order. A `toml::Table` is sorted, so the
/// `[keys]` table is walked with a visitor, which sees entries as written.
struct Keys(Vec<(String, String)>);

impl<'de> Deserialize<'de> for Keys {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Keys, D::Error> {
        struct KeysVisitor;
        impl<'de> Visitor<'de> for KeysVisitor {
            type Value = Keys;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a keys table")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Keys, A::Error> {
                let mut rows = Vec::new();
                while let Some(name) = map.next_key::<String>()? {
                    if name == "command" {
                        let commands: Vec<toml::Value> = map.next_value()?;
                        rows.extend(commands.into_iter().filter_map(command_row));
                        continue;
                    }
                    if let toml::Value::String(key) = map.next_value::<toml::Value>()? {
                        rows.push((key, name.replace('_', " ")));
                    }
                }
                Ok(Keys(rows))
            }
        }
        deserializer.deserialize_map(KeysVisitor)
    }
}

fn command_row(value: toml::Value) -> Option<(String, String)> {
    let entry = Command::deserialize(value).ok()?;
    let key = entry.key?;
    if let Some(description) = entry.description.filter(|d| !d.trim().is_empty()) {
        return Some((key, description));
    }
    let command = entry.command?;
    Some((key, command_label(&command)))
}

/// `plugin: entrypoint` or `plugin: action` for a herdr plugin command,
/// otherwise the command itself.
fn command_label(command: &str) -> String {
    let words: Vec<&str> = command.split_whitespace().collect();
    let after = |flag: &str| {
        words
            .iter()
            .position(|w| *w == flag)
            .and_then(|i| words.get(i + 1))
            .copied()
    };
    let plugin = after("--plugin");
    let target = after("--entrypoint").or_else(|| after("invoke"));
    match (plugin, target) {
        (Some(plugin), Some(target)) => format!("{plugin}: {target}"),
        (Some(plugin), None) => plugin.to_string(),
        _ => command.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/linear/fixtures/herdr-config.toml")
    }

    fn row(key: &str, label: &str) -> HerdrKey {
        HerdrKey {
            key: key.into(),
            label: label.into(),
        }
    }

    #[test]
    fn the_fixture_yields_named_actions_then_commands_in_file_order() {
        assert_eq!(
            read_file(&fixture()),
            vec![
                row("ctrl+b", "prefix"),
                row("ctrl+alt+1..9", "switch tab"),
                row("ctrl+alt+t", "new tab"),
                row("ctrl+alt+q", "close tab"),
                row("ctrl+alt+w", "workspace picker"),
                row("ctrl+alt+m", "example-viewer: open-example"),
                row("ctrl+alt+k", "example.picker: pick"),
                row("ctrl+alt+d", "open the example dashboard"),
            ]
        );
    }

    #[test]
    fn a_missing_config_yields_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_file(&dir.path().join("absent.toml")).is_empty());
    }

    #[test]
    fn an_unreadable_config_yields_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_file(dir.path()).is_empty());
    }

    #[test]
    fn a_malformed_config_yields_nothing() {
        assert!(parse("[keys\nprefix = ").is_empty());
        assert!(parse("keys = 3").is_empty());
        assert!(parse("[keys]\ncommand = \"not a table\"").is_empty());
    }

    #[test]
    fn an_empty_keys_table_yields_nothing() {
        assert!(parse("[keys]\n").is_empty());
        assert!(parse("[ui]\nprefix = \"ctrl+b\"\n").is_empty());
    }

    #[test]
    fn entries_missing_a_key_or_a_string_value_are_skipped() {
        let text = "[keys]\nprefix = 3\nnew_tab = \"ctrl+t\"\n\
                    [[keys.command]]\ncommand = \"herdr plugin pane open --plugin a --entrypoint b\"\n";
        assert_eq!(parse(text), vec![row("ctrl+t", "new tab")]);
    }

    #[test]
    fn control_and_bidi_characters_are_stripped_from_every_field() {
        let text = "[keys]\nnew_tab = \"ctrl+\\u001b[2Jt\"\n\
                    [[keys.command]]\nkey = \"ctrl+\\u202Ed\"\ndescription = \"open\\u202E the\\u0007 panel\\n\"\n";
        let rows = parse(text);
        assert_eq!(
            rows,
            vec![row("ctrl+[2Jt", "new tab"), row("ctrl+d", "open the panel")]
        );
        assert!(rows
            .iter()
            .flat_map(|r| r.key.chars().chain(r.label.chars()))
            .all(|c| !c.is_control() && !board_core::text::is_format_char(c)));
    }

    #[test]
    fn a_config_larger_than_the_cap_yields_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut text = String::from("[keys]\nprefix = \"ctrl+b\"\n#");
        text.push_str(&"x".repeat(MAX_CONFIG_BYTES as usize));
        std::fs::write(&path, text).unwrap();
        assert!(read_file(&path).is_empty());
    }

    #[test]
    fn a_config_just_under_the_cap_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut text = String::from("[keys]\nprefix = \"ctrl+b\"\n#");
        text.push_str(&"x".repeat(MAX_CONFIG_BYTES as usize - text.len()));
        std::fs::write(&path, &text).unwrap();
        assert_eq!(text.len() as u64, MAX_CONFIG_BYTES);
        assert_eq!(read_file(&path), vec![row("ctrl+b", "prefix")]);
    }

    #[test]
    fn the_path_prefers_xdg_config_home_then_home() {
        assert_eq!(
            resolve_path(Some("/xdg".into()), Some("/home/example".into())),
            Some(PathBuf::from("/xdg/herdr/config.toml"))
        );
        assert_eq!(
            resolve_path(Some("".into()), Some("/home/example".into())),
            Some(PathBuf::from("/home/example/.config/herdr/config.toml"))
        );
        assert_eq!(resolve_path(None, None), None);
    }
}
