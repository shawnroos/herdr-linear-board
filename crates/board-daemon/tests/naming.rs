use board_daemon::naming::{decide, derive, Scope, Target, MAX_CELLS};
use unicode_width::UnicodeWidthStr;

fn scope(
    issue: Option<&str>,
    title: Option<&str>,
    agent: Option<&str>,
    folder: Option<&str>,
) -> Scope {
    Scope {
        issue: issue.map(str::to_string),
        title: title.map(str::to_string),
        agent: agent.map(str::to_string),
        folder: folder.map(str::to_string),
    }
}

#[test]
fn derive_names_each_scope() {
    let rows: &[(Target, Scope, &str)] = &[
        (
            Target::Tab,
            scope(
                Some("WEB-101"),
                Some("Fix login"),
                Some("claude"),
                Some("web"),
            ),
            "WEB-101 · Fix login",
        ),
        (
            Target::Split,
            scope(
                Some("WEB-101"),
                Some("Fix login"),
                Some("claude"),
                Some("web"),
            ),
            "WEB-101 · claude",
        ),
        (
            Target::Tab,
            scope(Some("WEB-101"), None, None, Some("web")),
            "WEB-101",
        ),
        (
            Target::Tab,
            scope(Some("WEB-101"), Some("   "), None, None),
            "WEB-101",
        ),
        (
            Target::Split,
            scope(Some("WEB-101"), Some("Fix login"), None, Some("web")),
            "WEB-101",
        ),
        (
            Target::Split,
            scope(None, None, Some("codex"), Some("herdr-linear-board")),
            "herdr-linear-board · codex",
        ),
        (
            Target::Split,
            scope(None, None, None, Some("herdr-linear-board")),
            "herdr-linear-board",
        ),
        (
            Target::Tab,
            scope(None, None, Some("codex"), Some("herdr-linear-board")),
            "herdr-linear-board · codex",
        ),
        (Target::Split, scope(None, None, Some("pi"), None), "pi"),
        (Target::Split, scope(None, None, None, None), "shell"),
        (
            Target::Split,
            scope(None, None, Some(" "), Some("")),
            "shell",
        ),
        (
            Target::Tab,
            scope(Some("WEB-7"), Some("Bad\u{1b}]0;x\u{7}\ttitle"), None, None),
            "WEB-7 · Bad]0;xtitle",
        ),
    ];
    for (target, scope, want) in rows {
        assert_eq!(derive(*target, scope), *want, "{target:?} {scope:?}");
    }
}

#[test]
fn derive_caps_long_names_at_forty_cells() {
    let long = "A very long issue title that keeps going well past the limit";
    let name = derive(Target::Tab, &scope(Some("WEB-101"), Some(long), None, None));
    assert!(
        (MAX_CELLS - 1..=MAX_CELLS).contains(&name.width()),
        "{name}"
    );
    assert!(name.starts_with("WEB-101 · A very long"), "{name}");
    assert!(name.ends_with('…'), "{name}");

    let wide = "宽".repeat(30);
    let name = derive(Target::Split, &scope(None, None, None, Some(&wide)));
    assert!(name.width() <= MAX_CELLS, "{name}");
    assert!(name.ends_with('…'), "{name}");

    let exact = "x".repeat(MAX_CELLS);
    assert_eq!(
        derive(Target::Split, &scope(None, None, None, Some(&exact))),
        exact
    );
}

#[test]
fn decide_renames_only_blank_default_or_generated_labels() {
    let derived = "WEB-101 · Fix login";
    type Row<'a> = (
        Option<&'a str>,
        Option<&'a str>,
        Option<&'a str>,
        bool,
        &'a str,
    );
    let rows: &[Row] = &[
        (None, None, None, true, "a pane with no label"),
        (Some(""), None, None, true, "an empty label"),
        (Some("  "), None, None, true, "a blank label"),
        (
            Some("2"),
            Some("2"),
            None,
            true,
            "herdr's default tab label",
        ),
        (
            Some("3"),
            Some("2"),
            None,
            false,
            "a number that is not this tab's default",
        ),
        (
            Some("WEB-9 · Old"),
            None,
            Some("WEB-9 · Old"),
            true,
            "the label the board last wrote",
        ),
        (
            Some("scratch"),
            None,
            Some("WEB-9 · Old"),
            false,
            "a label set by hand",
        ),
        (
            Some("scratch"),
            Some("2"),
            None,
            false,
            "a hand-set tab label",
        ),
        (
            Some("card-12"),
            Some("2"),
            None,
            false,
            "a run tab the spawner named",
        ),
        (
            Some("card-12-anchor"),
            None,
            None,
            false,
            "a run anchor the spawner named",
        ),
        (
            Some("Linear: Project"),
            None,
            None,
            false,
            "a board's own title",
        ),
        (
            Some(derived),
            None,
            Some(derived),
            false,
            "already carries the name",
        ),
        (
            Some(derived),
            None,
            None,
            false,
            "already carries the name, never stored",
        ),
    ];
    for (current, default, stored, want, why) in rows {
        assert_eq!(decide(*current, *default, *stored, derived), *want, "{why}");
    }
}
