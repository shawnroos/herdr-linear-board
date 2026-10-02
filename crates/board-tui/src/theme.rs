//! herdr's theme as the board's colours. Palettes are transcribed from herdr
//! v0.9.3 `src/app/state.rs`; names and aliases from `src/config/theme.rs`.

use board_core::protocol::MarkKind;
use ratatui::style::{Color, Modifier, Style};
use serde::Deserialize;

pub const THEME_NAMES: &[&str] = &[
    "catppuccin",
    "catppuccin-latte",
    "terminal",
    "tokyo-night",
    "tokyo-night-day",
    "dracula",
    "nord",
    "gruvbox",
    "gruvbox-light",
    "one-dark",
    "one-light",
    "solarized",
    "solarized-light",
    "kanagawa",
    "kanagawa-lotus",
    "rose-pine",
    "rose-pine-dawn",
    "vesper",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub accent: Color,
    pub panel_bg: Color,
    pub sidebar_bg: Color,
    pub active_row_bg: Color,
    pub selection_bg: Color,
    pub surface0: Color,
    pub surface1: Color,
    pub surface_dim: Color,
    pub overlay0: Color,
    pub overlay1: Color,
    pub text: Color,
    pub subtext0: Color,
    pub mauve: Color,
    pub green: Color,
    pub yellow: Color,
    pub red: Color,
    pub blue: Color,
    pub teal: Color,
    pub peach: Color,
}

const CATPPUCCIN: Palette = Palette {
    accent: Color::Rgb(137, 180, 250),
    panel_bg: Color::Rgb(24, 24, 37),
    sidebar_bg: Color::Reset,
    active_row_bg: Color::Rgb(30, 30, 46),
    selection_bg: Color::Rgb(49, 50, 68),
    surface0: Color::Rgb(49, 50, 68),
    surface1: Color::Rgb(69, 71, 90),
    surface_dim: Color::Rgb(30, 30, 46),
    overlay0: Color::Rgb(108, 112, 134),
    overlay1: Color::Rgb(127, 132, 156),
    text: Color::Rgb(205, 214, 244),
    subtext0: Color::Rgb(166, 173, 200),
    mauve: Color::Rgb(203, 166, 247),
    green: Color::Rgb(166, 227, 161),
    yellow: Color::Rgb(249, 226, 175),
    red: Color::Rgb(243, 139, 168),
    blue: Color::Rgb(137, 180, 250),
    teal: Color::Rgb(148, 226, 213),
    peach: Color::Rgb(250, 179, 135),
};

const CATPPUCCIN_LATTE: Palette = Palette {
    accent: Color::Rgb(30, 102, 245),
    panel_bg: Color::Rgb(239, 241, 245),
    sidebar_bg: Color::Reset,
    active_row_bg: Color::Rgb(230, 233, 239),
    selection_bg: Color::Rgb(189, 208, 245),
    surface0: Color::Rgb(204, 208, 218),
    surface1: Color::Rgb(188, 192, 204),
    surface_dim: Color::Rgb(230, 233, 239),
    overlay0: Color::Rgb(156, 160, 176),
    overlay1: Color::Rgb(140, 143, 161),
    text: Color::Rgb(76, 79, 105),
    subtext0: Color::Rgb(108, 111, 133),
    mauve: Color::Rgb(136, 57, 239),
    green: Color::Rgb(64, 160, 43),
    yellow: Color::Rgb(223, 142, 29),
    red: Color::Rgb(210, 15, 57),
    blue: Color::Rgb(30, 102, 245),
    teal: Color::Rgb(23, 146, 153),
    peach: Color::Rgb(254, 100, 11),
};

const TERMINAL: Palette = Palette {
    accent: Color::Blue,
    panel_bg: Color::Reset,
    sidebar_bg: Color::Reset,
    active_row_bg: Color::DarkGray,
    selection_bg: Color::Reset,
    surface0: Color::Reset,
    surface1: Color::DarkGray,
    surface_dim: Color::DarkGray,
    overlay0: Color::Gray,
    overlay1: Color::White,
    text: Color::Reset,
    subtext0: Color::Gray,
    mauve: Color::Gray,
    green: Color::Green,
    yellow: Color::Yellow,
    red: Color::LightRed,
    blue: Color::Blue,
    teal: Color::Cyan,
    peach: Color::Yellow,
};

const TOKYO_NIGHT: Palette = Palette {
    accent: Color::Rgb(122, 162, 247),
    panel_bg: Color::Rgb(26, 27, 38),
    sidebar_bg: Color::Reset,
    active_row_bg: Color::Rgb(35, 38, 54),
    selection_bg: Color::Rgb(45, 54, 80),
    surface0: Color::Rgb(36, 40, 59),
    surface1: Color::Rgb(65, 72, 104),
    surface_dim: Color::Rgb(26, 27, 38),
    overlay0: Color::Rgb(86, 95, 137),
    overlay1: Color::Rgb(105, 113, 150),
    text: Color::Rgb(192, 202, 245),
    subtext0: Color::Rgb(169, 177, 214),
    mauve: Color::Rgb(187, 154, 247),
    green: Color::Rgb(158, 206, 106),
    yellow: Color::Rgb(224, 175, 104),
    red: Color::Rgb(247, 118, 142),
    blue: Color::Rgb(122, 162, 247),
    teal: Color::Rgb(125, 207, 255),
    peach: Color::Rgb(255, 158, 100),
};

const TOKYO_NIGHT_DAY: Palette = Palette {
    accent: Color::Rgb(46, 125, 233),
    panel_bg: Color::Rgb(225, 226, 231),
    sidebar_bg: Color::Reset,
    active_row_bg: Color::Rgb(210, 211, 218),
    selection_bg: Color::Rgb(182, 202, 231),
    surface0: Color::Rgb(196, 200, 218),
    surface1: Color::Rgb(168, 174, 203),
    surface_dim: Color::Rgb(210, 211, 218),
    overlay0: Color::Rgb(137, 144, 179),
    overlay1: Color::Rgb(104, 112, 154),
    text: Color::Rgb(55, 96, 191),
    subtext0: Color::Rgb(97, 114, 176),
    mauve: Color::Rgb(120, 71, 189),
    green: Color::Rgb(88, 117, 57),
    yellow: Color::Rgb(140, 108, 62),
    red: Color::Rgb(245, 42, 101),
    blue: Color::Rgb(46, 125, 233),
    teal: Color::Rgb(17, 140, 116),
    peach: Color::Rgb(177, 92, 0),
};

const DRACULA: Palette = Palette {
    accent: Color::Rgb(189, 147, 249),
    panel_bg: Color::Rgb(40, 42, 54),
    sidebar_bg: Color::Reset,
    active_row_bg: Color::Rgb(55, 60, 82),
    selection_bg: Color::Rgb(70, 63, 93),
    surface0: Color::Rgb(68, 71, 90),
    surface1: Color::Rgb(98, 114, 164),
    surface_dim: Color::Rgb(40, 42, 54),
    overlay0: Color::Rgb(98, 114, 164),
    overlay1: Color::Rgb(130, 140, 180),
    text: Color::Rgb(248, 248, 242),
    subtext0: Color::Rgb(210, 210, 220),
    mauve: Color::Rgb(255, 121, 198),
    green: Color::Rgb(80, 250, 123),
    yellow: Color::Rgb(241, 250, 140),
    red: Color::Rgb(255, 85, 85),
    blue: Color::Rgb(139, 233, 253),
    teal: Color::Rgb(139, 233, 253),
    peach: Color::Rgb(255, 184, 108),
};

const NORD: Palette = Palette {
    accent: Color::Rgb(136, 192, 208),
    panel_bg: Color::Rgb(46, 52, 64),
    sidebar_bg: Color::Reset,
    active_row_bg: Color::Rgb(67, 76, 94),
    selection_bg: Color::Rgb(64, 80, 93),
    surface0: Color::Rgb(59, 66, 82),
    surface1: Color::Rgb(67, 76, 94),
    surface_dim: Color::Rgb(46, 52, 64),
    overlay0: Color::Rgb(76, 86, 106),
    overlay1: Color::Rgb(100, 110, 130),
    text: Color::Rgb(236, 239, 244),
    subtext0: Color::Rgb(216, 222, 233),
    mauve: Color::Rgb(180, 142, 173),
    green: Color::Rgb(163, 190, 140),
    yellow: Color::Rgb(235, 203, 139),
    red: Color::Rgb(191, 97, 106),
    blue: Color::Rgb(129, 161, 193),
    teal: Color::Rgb(143, 188, 187),
    peach: Color::Rgb(208, 135, 112),
};

const GRUVBOX: Palette = Palette {
    accent: Color::Rgb(215, 153, 33),
    panel_bg: Color::Rgb(40, 40, 40),
    sidebar_bg: Color::Reset,
    active_row_bg: Color::Rgb(50, 49, 48),
    selection_bg: Color::Rgb(75, 63, 39),
    surface0: Color::Rgb(60, 56, 54),
    surface1: Color::Rgb(80, 73, 69),
    surface_dim: Color::Rgb(40, 40, 40),
    overlay0: Color::Rgb(146, 131, 116),
    overlay1: Color::Rgb(168, 153, 132),
    text: Color::Rgb(235, 219, 178),
    subtext0: Color::Rgb(213, 196, 161),
    mauve: Color::Rgb(211, 134, 155),
    green: Color::Rgb(184, 187, 38),
    yellow: Color::Rgb(250, 189, 47),
    red: Color::Rgb(251, 73, 52),
    blue: Color::Rgb(131, 165, 152),
    teal: Color::Rgb(142, 192, 124),
    peach: Color::Rgb(254, 128, 25),
};

const GRUVBOX_LIGHT: Palette = Palette {
    accent: Color::Rgb(7, 102, 120),
    panel_bg: Color::Rgb(251, 241, 199),
    sidebar_bg: Color::Reset,
    active_row_bg: Color::Rgb(242, 229, 188),
    selection_bg: Color::Rgb(235, 219, 178),
    surface0: Color::Rgb(235, 219, 178),
    surface1: Color::Rgb(213, 196, 161),
    surface_dim: Color::Rgb(242, 229, 188),
    overlay0: Color::Rgb(146, 131, 116),
    overlay1: Color::Rgb(124, 111, 100),
    text: Color::Rgb(60, 56, 54),
    subtext0: Color::Rgb(80, 73, 69),
    mauve: Color::Rgb(143, 63, 113),
    green: Color::Rgb(121, 116, 14),
    yellow: Color::Rgb(181, 118, 20),
    red: Color::Rgb(157, 0, 6),
    blue: Color::Rgb(7, 102, 120),
    teal: Color::Rgb(66, 123, 88),
    peach: Color::Rgb(175, 58, 3),
};

const ONE_DARK: Palette = Palette {
    accent: Color::Rgb(97, 175, 239),
    panel_bg: Color::Rgb(40, 44, 52),
    sidebar_bg: Color::Reset,
    active_row_bg: Color::Rgb(49, 54, 64),
    selection_bg: Color::Rgb(51, 70, 89),
    surface0: Color::Rgb(44, 49, 58),
    surface1: Color::Rgb(62, 68, 81),
    surface_dim: Color::Rgb(40, 44, 52),
    overlay0: Color::Rgb(92, 99, 112),
    overlay1: Color::Rgb(115, 122, 135),
    text: Color::Rgb(171, 178, 191),
    subtext0: Color::Rgb(150, 156, 168),
    mauve: Color::Rgb(198, 120, 221),
    green: Color::Rgb(152, 195, 121),
    yellow: Color::Rgb(229, 192, 123),
    red: Color::Rgb(224, 108, 117),
    blue: Color::Rgb(97, 175, 239),
    teal: Color::Rgb(86, 182, 194),
    peach: Color::Rgb(209, 154, 102),
};

const ONE_LIGHT: Palette = Palette {
    accent: Color::Rgb(64, 120, 242),
    panel_bg: Color::Rgb(250, 250, 250),
    sidebar_bg: Color::Reset,
    active_row_bg: Color::Rgb(216, 219, 226),
    selection_bg: Color::Rgb(205, 219, 248),
    surface0: Color::Rgb(240, 240, 241),
    surface1: Color::Rgb(229, 229, 230),
    surface_dim: Color::Rgb(245, 245, 246),
    overlay0: Color::Rgb(160, 161, 167),
    overlay1: Color::Rgb(104, 107, 119),
    text: Color::Rgb(56, 58, 66),
    subtext0: Color::Rgb(104, 107, 119),
    mauve: Color::Rgb(166, 38, 164),
    green: Color::Rgb(80, 161, 79),
    yellow: Color::Rgb(193, 132, 1),
    red: Color::Rgb(228, 86, 73),
    blue: Color::Rgb(64, 120, 242),
    teal: Color::Rgb(1, 132, 188),
    peach: Color::Rgb(152, 104, 1),
};

const SOLARIZED: Palette = Palette {
    accent: Color::Rgb(38, 139, 210),
    panel_bg: Color::Rgb(0, 43, 54),
    sidebar_bg: Color::Reset,
    active_row_bg: Color::Rgb(22, 75, 87),
    selection_bg: Color::Rgb(8, 62, 85),
    surface0: Color::Rgb(7, 54, 66),
    surface1: Color::Rgb(88, 110, 117),
    surface_dim: Color::Rgb(0, 43, 54),
    overlay0: Color::Rgb(88, 110, 117),
    overlay1: Color::Rgb(101, 123, 131),
    text: Color::Rgb(147, 161, 161),
    subtext0: Color::Rgb(131, 148, 150),
    mauve: Color::Rgb(211, 54, 130),
    green: Color::Rgb(133, 153, 0),
    yellow: Color::Rgb(181, 137, 0),
    red: Color::Rgb(220, 50, 47),
    blue: Color::Rgb(38, 139, 210),
    teal: Color::Rgb(42, 161, 152),
    peach: Color::Rgb(203, 75, 22),
};

const SOLARIZED_LIGHT: Palette = Palette {
    accent: Color::Rgb(38, 139, 210),
    panel_bg: Color::Rgb(253, 246, 227),
    sidebar_bg: Color::Reset,
    active_row_bg: Color::Rgb(238, 232, 213),
    selection_bg: Color::Rgb(201, 220, 223),
    surface0: Color::Rgb(238, 232, 213),
    surface1: Color::Rgb(147, 161, 161),
    surface_dim: Color::Rgb(238, 232, 213),
    overlay0: Color::Rgb(147, 161, 161),
    overlay1: Color::Rgb(88, 110, 117),
    text: Color::Rgb(101, 123, 131),
    subtext0: Color::Rgb(131, 148, 150),
    mauve: Color::Rgb(211, 54, 130),
    green: Color::Rgb(133, 153, 0),
    yellow: Color::Rgb(181, 137, 0),
    red: Color::Rgb(220, 50, 47),
    blue: Color::Rgb(38, 139, 210),
    teal: Color::Rgb(42, 161, 152),
    peach: Color::Rgb(203, 75, 22),
};

const KANAGAWA: Palette = Palette {
    accent: Color::Rgb(126, 156, 216),
    panel_bg: Color::Rgb(31, 31, 40),
    sidebar_bg: Color::Reset,
    active_row_bg: Color::Rgb(54, 54, 70),
    selection_bg: Color::Rgb(50, 56, 75),
    surface0: Color::Rgb(42, 42, 55),
    surface1: Color::Rgb(54, 54, 70),
    surface_dim: Color::Rgb(31, 31, 40),
    overlay0: Color::Rgb(114, 113, 105),
    overlay1: Color::Rgb(135, 134, 125),
    text: Color::Rgb(220, 215, 186),
    subtext0: Color::Rgb(200, 195, 170),
    mauve: Color::Rgb(149, 127, 184),
    green: Color::Rgb(118, 148, 106),
    yellow: Color::Rgb(192, 163, 110),
    red: Color::Rgb(195, 64, 67),
    blue: Color::Rgb(126, 156, 216),
    teal: Color::Rgb(127, 180, 202),
    peach: Color::Rgb(255, 160, 102),
};

const KANAGAWA_LOTUS: Palette = Palette {
    accent: Color::Rgb(77, 105, 155),
    panel_bg: Color::Rgb(242, 236, 188),
    sidebar_bg: Color::Reset,
    active_row_bg: Color::Rgb(213, 206, 163),
    selection_bg: Color::Rgb(220, 213, 172),
    surface0: Color::Rgb(220, 213, 172),
    surface1: Color::Rgb(201, 203, 209),
    surface_dim: Color::Rgb(213, 206, 163),
    overlay0: Color::Rgb(160, 156, 172),
    overlay1: Color::Rgb(138, 137, 128),
    text: Color::Rgb(84, 84, 100),
    subtext0: Color::Rgb(67, 67, 108),
    mauve: Color::Rgb(98, 76, 131),
    green: Color::Rgb(111, 137, 78),
    yellow: Color::Rgb(119, 113, 63),
    red: Color::Rgb(200, 64, 83),
    blue: Color::Rgb(77, 105, 155),
    teal: Color::Rgb(78, 140, 162),
    peach: Color::Rgb(204, 109, 0),
};

const ROSE_PINE: Palette = Palette {
    accent: Color::Rgb(196, 167, 231),
    panel_bg: Color::Rgb(25, 23, 36),
    sidebar_bg: Color::Reset,
    active_row_bg: Color::Rgb(38, 35, 58),
    selection_bg: Color::Rgb(59, 52, 75),
    surface0: Color::Rgb(31, 29, 46),
    surface1: Color::Rgb(38, 35, 58),
    surface_dim: Color::Rgb(38, 35, 58),
    overlay0: Color::Rgb(110, 106, 134),
    overlay1: Color::Rgb(144, 140, 170),
    text: Color::Rgb(224, 222, 244),
    subtext0: Color::Rgb(200, 197, 220),
    mauve: Color::Rgb(196, 167, 231),
    green: Color::Rgb(49, 116, 143),
    yellow: Color::Rgb(246, 193, 119),
    red: Color::Rgb(235, 111, 146),
    blue: Color::Rgb(49, 116, 143),
    teal: Color::Rgb(156, 207, 216),
    peach: Color::Rgb(234, 154, 151),
};

const ROSE_PINE_DAWN: Palette = Palette {
    accent: Color::Rgb(144, 122, 169),
    panel_bg: Color::Rgb(250, 244, 237),
    sidebar_bg: Color::Reset,
    active_row_bg: Color::Rgb(227, 217, 207),
    selection_bg: Color::Rgb(242, 233, 225),
    surface0: Color::Rgb(242, 233, 225),
    surface1: Color::Rgb(255, 250, 243),
    surface_dim: Color::Rgb(242, 233, 225),
    overlay0: Color::Rgb(152, 147, 165),
    overlay1: Color::Rgb(121, 117, 147),
    text: Color::Rgb(70, 66, 97),
    subtext0: Color::Rgb(121, 117, 147),
    mauve: Color::Rgb(144, 122, 169),
    green: Color::Rgb(40, 105, 131),
    yellow: Color::Rgb(234, 157, 52),
    red: Color::Rgb(180, 99, 122),
    blue: Color::Rgb(40, 105, 131),
    teal: Color::Rgb(86, 148, 159),
    peach: Color::Rgb(215, 130, 126),
};

const VESPER: Palette = Palette {
    accent: Color::Rgb(255, 199, 153),
    panel_bg: Color::Rgb(26, 26, 26),
    sidebar_bg: Color::Reset,
    active_row_bg: Color::Rgb(16, 16, 16),
    selection_bg: Color::Rgb(35, 35, 35),
    surface0: Color::Rgb(35, 35, 35),
    surface1: Color::Rgb(40, 40, 40),
    surface_dim: Color::Rgb(16, 16, 16),
    overlay0: Color::Rgb(92, 92, 92),
    overlay1: Color::Rgb(126, 126, 126),
    text: Color::Rgb(255, 255, 255),
    subtext0: Color::Rgb(160, 160, 160),
    mauve: Color::Rgb(255, 209, 168),
    green: Color::Rgb(153, 255, 228),
    yellow: Color::Rgb(255, 199, 153),
    red: Color::Rgb(255, 128, 128),
    blue: Color::Rgb(176, 176, 176),
    teal: Color::Rgb(102, 221, 204),
    peach: Color::Rgb(255, 199, 153),
};

fn builtin(canonical: &str) -> Option<Palette> {
    Some(match canonical {
        "catppuccin" => CATPPUCCIN,
        "catppuccin-latte" => CATPPUCCIN_LATTE,
        "terminal" => TERMINAL,
        "tokyo-night" => TOKYO_NIGHT,
        "tokyo-night-day" => TOKYO_NIGHT_DAY,
        "dracula" => DRACULA,
        "nord" => NORD,
        "gruvbox" => GRUVBOX,
        "gruvbox-light" => GRUVBOX_LIGHT,
        "one-dark" => ONE_DARK,
        "one-light" => ONE_LIGHT,
        "solarized" => SOLARIZED,
        "solarized-light" => SOLARIZED_LIGHT,
        "kanagawa" => KANAGAWA,
        "kanagawa-lotus" => KANAGAWA_LOTUS,
        "rose-pine" => ROSE_PINE,
        "rose-pine-dawn" => ROSE_PINE_DAWN,
        "vesper" => VESPER,
        _ => return None,
    })
}

pub fn canonical_name(name: &str) -> Option<&'static str> {
    match name.to_lowercase().replace([' ', '_'], "-").as_str() {
        "catppuccin" | "catppuccin-mocha" => Some("catppuccin"),
        "catppuccin-latte" | "latte" | "light" => Some("catppuccin-latte"),
        "terminal" => Some("terminal"),
        "tokyo-night" | "tokyonight" => Some("tokyo-night"),
        "tokyo-night-day" | "tokyo-day" | "tokyonight-day" => Some("tokyo-night-day"),
        "dracula" => Some("dracula"),
        "nord" => Some("nord"),
        "gruvbox" | "gruvbox-dark" => Some("gruvbox"),
        "gruvbox-light" => Some("gruvbox-light"),
        "one-dark" | "onedark" => Some("one-dark"),
        "one-light" | "onelight" => Some("one-light"),
        "solarized" | "solarized-dark" => Some("solarized"),
        "solarized-light" => Some("solarized-light"),
        "kanagawa" => Some("kanagawa"),
        "kanagawa-lotus" | "lotus" => Some("kanagawa-lotus"),
        "rose-pine" | "rosepine" => Some("rose-pine"),
        "rose-pine-dawn" | "rosepine-dawn" | "dawn" => Some("rose-pine-dawn"),
        "vesper" => Some("vesper"),
        _ => None,
    }
}

/// herdr's `parse_color`, except that an unknown value is `None` (the base
/// token stays) where herdr logs a warning and paints cyan.
pub fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim().to_lowercase();
    if matches!(s.as_str(), "reset" | "default" | "none" | "transparent") {
        return Some(Color::Reset);
    }
    if let Some(hex) = s.strip_prefix('#') {
        let digits: Option<Vec<u8>> = hex
            .chars()
            .map(|c| c.to_digit(16).map(|d| d as u8))
            .collect();
        match digits.as_deref() {
            Some([r1, r2, g1, g2, b1, b2]) => {
                return Some(Color::Rgb(r1 * 16 + r2, g1 * 16 + g2, b1 * 16 + b2))
            }
            Some([r, g, b]) => return Some(Color::Rgb(r * 17, g * 17, b * 17)),
            _ => {}
        }
    }
    if let Some(inner) = s.strip_prefix("rgb(").and_then(|s| s.strip_suffix(')')) {
        let parts: Vec<Option<u8>> = inner.split(',').map(|p| p.trim().parse().ok()).collect();
        if let [Some(r), Some(g), Some(b)] = parts[..] {
            return Some(Color::Rgb(r, g, b));
        }
    }
    Some(match s.as_str() {
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" | "purple" => Color::Magenta,
        "cyan" => Color::Cyan,
        "white" => Color::White,
        "gray" | "grey" => Color::Gray,
        "darkgray" | "darkgrey" => Color::DarkGray,
        "lightred" => Color::LightRed,
        "lightgreen" => Color::LightGreen,
        "lightyellow" => Color::LightYellow,
        "lightblue" => Color::LightBlue,
        "lightmagenta" => Color::LightMagenta,
        "lightcyan" => Color::LightCyan,
        _ => return None,
    })
}

/// herdr's `[theme]` table. `auto_switch`, `dark_name`, `light_name` and the
/// per-mode `[theme.custom.light|dark]` tables are not read.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ThemeSection {
    pub name: Option<String>,
    pub custom: Option<CustomColors>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct CustomColors {
    pub accent: Option<String>,
    pub panel_bg: Option<String>,
    pub sidebar_bg: Option<String>,
    pub active_row_bg: Option<String>,
    pub selection_bg: Option<String>,
    pub surface0: Option<String>,
    pub surface1: Option<String>,
    pub surface_dim: Option<String>,
    pub overlay0: Option<String>,
    pub overlay1: Option<String>,
    pub text: Option<String>,
    pub subtext0: Option<String>,
    pub mauve: Option<String>,
    pub green: Option<String>,
    pub yellow: Option<String>,
    pub red: Option<String>,
    pub blue: Option<String>,
    pub teal: Option<String>,
    pub peach: Option<String>,
}

impl CustomColors {
    fn apply(&self, p: &mut Palette) {
        let pairs = [
            (&self.accent, &mut p.accent),
            (&self.panel_bg, &mut p.panel_bg),
            (&self.sidebar_bg, &mut p.sidebar_bg),
            (&self.active_row_bg, &mut p.active_row_bg),
            (&self.selection_bg, &mut p.selection_bg),
            (&self.surface0, &mut p.surface0),
            (&self.surface1, &mut p.surface1),
            (&self.surface_dim, &mut p.surface_dim),
            (&self.overlay0, &mut p.overlay0),
            (&self.overlay1, &mut p.overlay1),
            (&self.text, &mut p.text),
            (&self.subtext0, &mut p.subtext0),
            (&self.mauve, &mut p.mauve),
            (&self.green, &mut p.green),
            (&self.yellow, &mut p.yellow),
            (&self.red, &mut p.red),
            (&self.blue, &mut p.blue),
            (&self.teal, &mut p.teal),
            (&self.peach, &mut p.peach),
        ];
        for (value, token) in pairs {
            if let Some(color) = value.as_deref().and_then(parse_color) {
                *token = color;
            }
        }
    }
}

/// The palette plus the tones herdr has no token for. `app_bg` sits one step
/// below the card surface, `muted_card` just under the card, `muted_text` is
/// `overlay0`, and `muted_detail` one step dimmer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Theme {
    pub name: &'static str,
    pub palette: Palette,
    pub app_bg: Color,
    pub muted_card: Color,
    pub muted_text: Color,
    pub muted_detail: Color,
}

impl Default for Theme {
    fn default() -> Theme {
        Theme::gruvbox()
    }
}

// gruvbox: panel #282828 → app #1e1e1e and muted card #262626.
const APP_STEP: u8 = 10;
const MUTED_CARD_STEP: u8 = 2;
// Shawn's gruvbox muted detail; no blend reproduces it in every channel.
const GRUVBOX_MUTED_DETAIL: Color = Color::Rgb(0x7c, 0x6f, 0x64);
const MUTED_DETAIL_BLEND: f32 = 0.2;
// "about 60%" per the design (KTD6).
const FADE: f32 = 0.6;

impl Theme {
    pub fn gruvbox() -> Theme {
        Theme::build("gruvbox", GRUVBOX)
    }

    pub fn named(name: &str) -> Option<Theme> {
        let canonical = canonical_name(name)?;
        Some(Theme::build(canonical, builtin(canonical)?))
    }

    // herdr itself falls back to catppuccin; the board's fallback is gruvbox
    // by design (the theme the board was designed on).
    pub fn from_section(section: &ThemeSection) -> Theme {
        let (name, mut palette) = section
            .name
            .as_deref()
            .and_then(canonical_name)
            .and_then(|name| Some((name, builtin(name)?)))
            .unwrap_or(("gruvbox", GRUVBOX));
        if let Some(custom) = &section.custom {
            custom.apply(&mut palette);
        }
        Theme::build(name, palette)
    }

    fn build(name: &'static str, palette: Palette) -> Theme {
        let (app_bg, muted_card) = match palette.panel_bg {
            Color::Rgb(r, g, b) => {
                let below = below_direction(r, g, b);
                (
                    shift(palette.panel_bg, below, APP_STEP),
                    shift(palette.panel_bg, below, MUTED_CARD_STEP),
                )
            }
            _ => (Color::Reset, Color::Reset),
        };
        let gruvbox_tones =
            palette.panel_bg == GRUVBOX.panel_bg && palette.overlay0 == GRUVBOX.overlay0;
        let muted_detail = if gruvbox_tones {
            GRUVBOX_MUTED_DETAIL
        } else {
            match (palette.overlay0, muted_card) {
                (Color::Rgb(..), Color::Rgb(..)) => {
                    blend(palette.overlay0, muted_card, MUTED_DETAIL_BLEND)
                }
                _ => Color::DarkGray,
            }
        };
        Theme {
            name,
            palette,
            app_bg,
            muted_card,
            muted_text: palette.overlay0,
            muted_detail,
        }
    }

    pub fn app(&self) -> Style {
        Style::default().bg(self.app_bg).fg(self.palette.text)
    }

    pub fn text(&self) -> Style {
        Style::default().fg(self.palette.text)
    }

    pub fn secondary(&self) -> Style {
        Style::default().fg(self.palette.subtext0)
    }

    pub fn dim(&self) -> Style {
        Style::default().fg(self.palette.overlay0)
    }

    pub fn accent(&self) -> Style {
        Style::default().fg(self.palette.accent)
    }

    pub fn card(&self) -> Style {
        Style::default()
            .bg(self.palette.panel_bg)
            .fg(self.palette.text)
    }

    pub fn card_active(&self) -> Style {
        Style::default()
            .bg(self.palette.surface0)
            .fg(self.palette.text)
    }

    pub fn card_muted(&self) -> Style {
        Style::default().bg(self.muted_card).fg(self.muted_text)
    }

    pub fn card_title(&self) -> Style {
        Style::default()
            .fg(self.palette.text)
            .add_modifier(Modifier::BOLD)
    }

    pub fn card_title_active(&self) -> Style {
        Style::default()
            .fg(self.palette.yellow)
            .add_modifier(Modifier::BOLD)
    }

    pub fn card_title_muted(&self) -> Style {
        Style::default()
            .fg(self.muted_text)
            .add_modifier(Modifier::BOLD)
    }

    pub fn card_detail(&self) -> Style {
        Style::default().fg(self.palette.subtext0)
    }

    pub fn card_detail_muted(&self) -> Style {
        Style::default().fg(self.muted_detail)
    }

    pub fn rule(&self) -> Style {
        Style::default().fg(self.palette.surface1)
    }

    pub fn chip(&self) -> Style {
        Style::default()
            .bg(self.palette.surface0)
            .fg(self.palette.text)
    }

    pub fn panel(&self) -> Style {
        Style::default()
            .bg(self.palette.panel_bg)
            .fg(self.palette.text)
    }

    pub fn column_title_active(&self) -> Style {
        Style::default()
            .fg(self.palette.panel_bg)
            .bg(self.palette.accent)
            .add_modifier(Modifier::BOLD)
    }

    pub fn column_title_inactive(&self) -> Style {
        Style::default()
            .fg(self.palette.overlay0)
            .bg(self.palette.panel_bg)
    }

    /// A powerline cut between two bar segments: the glyph takes the left
    /// segment's colour over the right segment's background.
    pub fn cut(&self, left: Color, right: Color) -> Style {
        Style::default().fg(left).bg(right)
    }

    pub fn lane(&self) -> Style {
        Style::default().fg(self.palette.overlay0)
    }

    pub fn lane_holding(&self) -> Style {
        Style::default().fg(self.palette.accent)
    }

    pub fn lane_selected(&self) -> Style {
        self.lane_holding().bg(self.palette.surface0)
    }

    pub fn mark(&self, kind: MarkKind) -> Style {
        let fg = match kind {
            MarkKind::Attention => self.palette.red,
            MarkKind::Question => self.palette.peach,
            MarkKind::Done => self.palette.green,
            MarkKind::Suggestion => self.palette.mauve,
        };
        Style::default().fg(fg)
    }

    pub fn show_mark(&self) -> Style {
        Style::default().fg(self.palette.blue)
    }

    pub fn error(&self) -> Style {
        Style::default().fg(self.palette.red)
    }

    pub fn warning(&self) -> Style {
        Style::default().fg(self.palette.peach)
    }

    pub fn success(&self) -> Style {
        Style::default().fg(self.palette.green)
    }

    pub fn toast(&self, is_error: bool) -> Style {
        let bg = if is_error {
            self.palette.red
        } else {
            self.palette.yellow
        };
        Style::default().fg(self.palette.panel_bg).bg(bg)
    }

    /// `c` faded toward the app background, for what sits behind a modal.
    pub fn fade(&self, c: Color) -> Color {
        blend(c, self.app_bg, FADE)
    }

    /// The surface one tone above `under`: app → card → raised → rule tone.
    pub fn surface_above(&self, under: Color) -> Color {
        let p = &self.palette;
        if under == self.app_bg || under == self.muted_card {
            p.panel_bg
        } else if under == p.panel_bg {
            p.surface0
        } else {
            p.surface1
        }
    }
}

#[derive(Clone, Copy)]
enum Direction {
    Darker,
    Lighter,
}

/// Below the card is darker on dark themes and lighter on light ones, as
/// herdr's own raised tokens go the other way. Flipped when the full step
/// would clip, so the tones never collapse onto the card.
fn below_direction(r: u8, g: u8, b: u8) -> Direction {
    let luma = 299 * u32::from(r) + 587 * u32::from(g) + 114 * u32::from(b);
    let preferred = if luma < 128 * 1000 {
        Direction::Darker
    } else {
        Direction::Lighter
    };
    let fits = |d: Direction| match d {
        Direction::Darker => r.min(g).min(b) >= APP_STEP,
        Direction::Lighter => r.max(g).max(b) <= u8::MAX - APP_STEP,
    };
    match preferred {
        d if fits(d) => d,
        Direction::Darker => Direction::Lighter,
        Direction::Lighter => Direction::Darker,
    }
}

fn shift(c: Color, d: Direction, step: u8) -> Color {
    let Color::Rgb(r, g, b) = c else {
        return c;
    };
    let f = |v: u8| match d {
        Direction::Darker => v.saturating_sub(step),
        Direction::Lighter => v.saturating_add(step),
    };
    Color::Rgb(f(r), f(g), f(b))
}

fn blend(from: Color, to: Color, t: f32) -> Color {
    let (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) = (from, to) else {
        return from;
    };
    let mix = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
    Color::Rgb(mix(r1, r2), mix(g1, g2), mix(b1, b2))
}
