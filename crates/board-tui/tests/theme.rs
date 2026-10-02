use std::path::Path;

use board_core::protocol::MarkKind;
use board_tui::herdr_keys::{parse, read_file, MAX_CONFIG_BYTES};
use board_tui::theme::{Theme, THEME_NAMES};
use ratatui::style::{Color, Modifier};

fn rgb(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

fn theme_of(text: &str) -> Theme {
    parse(text).theme
}

fn luminance(c: Color) -> u32 {
    match c {
        Color::Rgb(r, g, b) => 299 * r as u32 + 587 * g as u32 + 114 * b as u32,
        other => panic!("expected an RGB colour, got {other:?}"),
    }
}

#[test]
fn gruvbox_yields_herdr_tokens_and_the_pinned_tones() {
    let theme = theme_of("[theme]\nname = \"gruvbox\"\n");
    assert_eq!(theme.name, "gruvbox");
    assert_eq!(theme.palette.panel_bg, rgb(0x282828));
    assert_eq!(theme.palette.accent, rgb(0xd79921));
    assert_eq!(theme.palette.surface0, rgb(0x3c3836));
    assert_eq!(theme.palette.overlay0, rgb(0x928374));
    assert_eq!(theme.app_bg, rgb(0x1e1e1e));
    assert_eq!(theme.muted_card, rgb(0x262626));
    assert_eq!(theme.muted_text, rgb(0x928374));
    assert_eq!(theme.muted_detail, rgb(0x7c6f64));
}

#[test]
fn a_custom_accent_overrides_only_the_accent() {
    let base = Theme::gruvbox();
    let theme = theme_of("[theme]\nname = \"gruvbox\"\n\n[theme.custom]\naccent = \"#ff0000\"\n");
    assert_eq!(theme.palette.accent, rgb(0xff0000));
    let mut expected = base.palette;
    expected.accent = rgb(0xff0000);
    assert_eq!(theme.palette, expected);
    assert_eq!(theme.app_bg, base.app_bg);
    assert_eq!(theme.muted_card, base.muted_card);
}

#[test]
fn custom_colours_accept_herdr_spellings() {
    let theme = theme_of(
        "[theme]\nname = \"nord\"\n[theme.custom]\nred = \"rgb(1, 2, 3)\"\ngreen = \"#abc\"\nblue = \"reset\"\nteal = \"LightCyan\"\n",
    );
    assert_eq!(theme.palette.red, Color::Rgb(1, 2, 3));
    assert_eq!(theme.palette.green, rgb(0xaabbcc));
    assert_eq!(theme.palette.blue, Color::Reset);
    assert_eq!(theme.palette.teal, Color::LightCyan);
}

#[test]
fn an_unparseable_custom_colour_keeps_the_base_token() {
    let theme = theme_of("[theme]\nname = \"gruvbox\"\n[theme.custom]\nred = \"not-a-colour\"\n");
    assert_eq!(theme.palette.red, Theme::gruvbox().palette.red);
}

#[test]
fn an_unknown_theme_name_falls_back_to_gruvbox() {
    assert_eq!(
        theme_of("[theme]\nname = \"no-such-theme\"\n"),
        Theme::gruvbox()
    );
}

#[test]
fn a_config_without_a_theme_table_is_gruvbox() {
    assert_eq!(theme_of("[keys]\nprefix = \"ctrl+b\"\n"), Theme::gruvbox());
}

#[test]
fn a_missing_oversized_or_malformed_config_falls_back_to_gruvbox() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        read_file(&dir.path().join("absent.toml")).theme,
        Theme::gruvbox()
    );

    let big = dir.path().join("big.toml");
    let mut text = String::from("[theme]\nname = \"nord\"\n");
    text.push_str(&"#".repeat(MAX_CONFIG_BYTES as usize));
    std::fs::write(&big, text).unwrap();
    assert_eq!(read_file(&big).theme, Theme::gruvbox());

    assert_eq!(theme_of("[theme\nname = "), Theme::gruvbox());
    assert_eq!(theme_of("[theme]\nname = 5\n"), Theme::gruvbox());
}

#[test]
fn a_malformed_theme_table_keeps_the_herdr_keys() {
    let config = parse("[theme]\nname = 5\n\n[keys]\nprefix = \"ctrl+b\"\n");
    assert_eq!(config.keys.len(), 1);
    assert_eq!(config.theme, Theme::gruvbox());
}

#[test]
fn theme_names_resolve_through_herdr_aliases() {
    assert_eq!(
        theme_of("[theme]\nname = \"Gruvbox Dark\"\n").name,
        "gruvbox"
    );
    assert_eq!(
        theme_of("[theme]\nname = \"catppuccin_mocha\"\n").name,
        "catppuccin"
    );
    assert_eq!(
        theme_of("[theme]\nname = \"dawn\"\n").name,
        "rose-pine-dawn"
    );
    assert_eq!(
        theme_of("[theme]\nname = \"light\"\n").name,
        "catppuccin-latte"
    );
}

#[test]
fn every_herdr_theme_is_built_in() {
    assert_eq!(THEME_NAMES.len(), 18);
    for name in THEME_NAMES {
        let theme = Theme::named(name).unwrap_or_else(|| panic!("{name} resolves"));
        assert_eq!(theme.name, *name);
    }
    assert_eq!(
        Theme::named("tokyo-night").unwrap().palette.panel_bg,
        rgb(0x1a1b26)
    );
    assert_eq!(
        Theme::named("vesper").unwrap().palette.accent,
        rgb(0xffc799)
    );
}

#[test]
fn the_terminal_theme_builds_without_rgb() {
    let theme = Theme::named("terminal").unwrap();
    assert_eq!(theme.palette.panel_bg, Color::Reset);
    assert_eq!(theme.app_bg, Color::Reset);
    assert_eq!(theme.muted_text, Color::Gray);
    assert_ne!(theme.muted_detail, theme.muted_text);
}

#[test]
fn derived_muted_tones_differ_from_the_card_surface_on_every_rgb_theme() {
    for name in THEME_NAMES.iter().filter(|n| **n != "terminal") {
        let theme = Theme::named(name).unwrap();
        let card = theme.palette.panel_bg;
        assert_ne!(theme.app_bg, card, "{name}: app background");
        assert_ne!(theme.muted_card, card, "{name}: muted card");
        assert_ne!(theme.muted_card, theme.app_bg, "{name}: muted card vs app");
        assert_ne!(theme.muted_text, card, "{name}: muted text");
        assert_ne!(theme.muted_detail, card, "{name}: muted detail");
        assert_ne!(
            theme.muted_detail, theme.muted_text,
            "{name}: muted detail vs text"
        );
        assert_ne!(
            theme.muted_detail, theme.muted_card,
            "{name}: detail vs muted card"
        );
    }
}

#[test]
fn a_light_custom_panel_still_derives_distinct_muted_tones() {
    for panel in ["#ffffff", "#fbf1c7", "#000000"] {
        let theme = theme_of(&format!(
            "[theme]\nname = \"gruvbox-light\"\n[theme.custom]\npanel_bg = \"{panel}\"\n"
        ));
        assert_ne!(theme.muted_card, theme.palette.panel_bg, "{panel}");
        assert_ne!(theme.muted_card, theme.app_bg, "{panel}");
        assert_ne!(theme.app_bg, theme.palette.panel_bg, "{panel}");
    }
}

#[test]
fn the_app_background_sits_below_the_card_on_dark_themes() {
    let theme = Theme::named("tokyo-night").unwrap();
    assert!(luminance(theme.app_bg) < luminance(theme.palette.panel_bg));
}

#[test]
fn role_styles_follow_the_design_tokens() {
    let t = Theme::gruvbox();
    let p = &t.palette;
    assert_eq!(t.app().bg, Some(t.app_bg));
    assert_eq!(t.card().bg, Some(p.panel_bg));
    assert_eq!(t.card_active().bg, Some(p.surface0));
    assert_eq!(t.card_muted().bg, Some(t.muted_card));
    assert_eq!(t.card_title().fg, Some(p.text));
    assert!(t.card_title().add_modifier.contains(Modifier::BOLD));
    assert_eq!(t.card_title_active().fg, Some(p.yellow));
    assert_eq!(t.card_title_muted().fg, Some(t.muted_text));
    assert_eq!(t.card_detail().fg, Some(p.subtext0));
    assert_eq!(t.card_detail_muted().fg, Some(t.muted_detail));
    assert_eq!(t.rule().fg, Some(p.surface1));
    let active = t.column_title_active();
    assert_eq!((active.fg, active.bg), (Some(p.panel_bg), Some(p.accent)));
    assert!(active.add_modifier.contains(Modifier::BOLD));
    let inactive = t.column_title_inactive();
    assert_eq!(
        (inactive.fg, inactive.bg),
        (Some(p.overlay0), Some(p.panel_bg))
    );
    assert_eq!(t.lane().fg, Some(p.overlay0));
    assert_eq!(t.lane_holding().fg, Some(p.accent));
    assert_eq!(t.lane_selected().bg, Some(p.surface0));
    assert_eq!(t.surface_above(p.panel_bg), p.surface0);
    assert_eq!(t.surface_above(t.app_bg), p.panel_bg);
}

#[test]
fn marks_keep_their_colours() {
    let t = Theme::gruvbox();
    let p = &t.palette;
    assert_eq!(t.mark(MarkKind::Attention).fg, Some(p.red));
    assert_eq!(t.mark(MarkKind::Question).fg, Some(p.peach));
    assert_eq!(t.mark(MarkKind::Done).fg, Some(p.green));
    assert_eq!(t.mark(MarkKind::Suggestion).fg, Some(p.mauve));
    assert_eq!(t.show_mark().fg, Some(p.blue));
}

#[test]
fn the_fixture_names_gruvbox_with_a_custom_accent() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/linear/fixtures/herdr-config.toml");
    let theme = read_file(&fixture).theme;
    assert_eq!(theme.name, "gruvbox");
    assert_eq!(theme.palette.accent, rgb(0xfe8019));
    assert_eq!(theme.palette.panel_bg, rgb(0x282828));
}
