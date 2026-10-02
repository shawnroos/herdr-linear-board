use super::*;
use board_core::config::RootConfig;

fn env<'a>(values: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
    move |key| {
        values
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, value)| (*value).to_owned())
    }
}

#[test]
fn injected_environment_overrides_typed_config_without_process_env() {
    let root =
        RootConfig::from_toml("[daemon]\nspawner = \"herdr\"\ntimeout_unit_secs = 12\n").unwrap();
    let settings = DaemonSettings::from_root(
        &root,
        &env(&[("BOARD_SPAWNER", "local"), ("BOARD_TICK_MS", "7")]),
    )
    .unwrap();

    assert_eq!(settings.spawner, SpawnerKind::Local);
    assert_eq!(settings.timeout_unit_secs, 12);
    assert_eq!(settings.local_poll_ms, 2000);
    assert_eq!(settings.tick_ms, 7);
}

#[test]
fn missing_daemon_config_uses_runtime_defaults() {
    let settings = DaemonSettings::from_root(&RootConfig::default(), &env(&[])).unwrap();
    assert_eq!(settings, DaemonSettings::default());
}

#[test]
fn invalid_injected_environment_is_a_config_error() {
    let root = RootConfig::default();
    assert!(matches!(
        DaemonSettings::from_root(&root, &env(&[("BOARD_SPAWNER", "bogus")])),
        Err(Error::Config(_))
    ));
}

#[test]
fn malformed_file_is_not_replaced_with_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "[daemon\n").unwrap();

    assert!(matches!(
        DaemonSettings::load_with_env(&path, &env(&[])),
        Err(Error::Config(_))
    ));
}

#[test]
fn the_retired_plugin_root_settings_are_accepted_and_give_one_warning() {
    let root =
        RootConfig::from_toml("[daemon]\nwork_plugin_root = \"/opt/work-plugin\"\n").unwrap();
    let settings = DaemonSettings::from_root(
        &root,
        &env(&[("BOARD_WORK_PLUGIN_ROOT", "/opt/other-plugin")]),
    )
    .unwrap();
    let warning = settings.retired_warning().expect("one warning");
    assert!(warning.contains("[daemon] work_plugin_root"), "{warning}");
    assert!(warning.contains("BOARD_WORK_PLUGIN_ROOT"), "{warning}");
    assert!(
        !warning.contains("/opt/"),
        "the value is not echoed: {warning}"
    );

    let config_only = DaemonSettings::from_root(&root, &env(&[])).unwrap();
    assert_eq!(config_only.retired, ["[daemon] work_plugin_root"]);

    let blank = DaemonSettings::from_root(
        &RootConfig::default(),
        &env(&[("BOARD_WORK_PLUGIN_ROOT", " ")]),
    )
    .unwrap();
    assert_eq!(blank.retired_warning(), None);
}

#[test]
fn the_show_request_ttl_comes_from_the_linear_table_and_defaults_to_thirty_minutes() {
    let defaults = DaemonSettings::from_root(&RootConfig::default(), &env(&[])).unwrap();
    assert_eq!(defaults.show_request_ttl_secs, 1800);

    let root = RootConfig::from_toml("[linear]\nshow_request_ttl_secs = 600\n").unwrap();
    let settings = DaemonSettings::from_root(&root, &env(&[])).unwrap();
    assert_eq!(settings.show_request_ttl_secs, 600);

    let root = RootConfig::from_toml("[linear]\nshow_request_ttl_secs = 0\n").unwrap();
    let settings = DaemonSettings::from_root(&root, &env(&[])).unwrap();
    assert_eq!(settings.show_request_ttl_secs, 1, "a zero TTL is clamped");
}
