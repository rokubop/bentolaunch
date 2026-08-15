//! Config lives next to the binary (safety rule 2: portable single exe, no
//! scattered state). Missing file => defaults, written out on first run so it is
//! discoverable and editable.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN,
};

use crate::{log_info, log_warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Log what a click would do instead of doing it. Off since Milestone 2.
    pub dry_run: bool,
    /// e.g. "alt+`". Modifiers: ctrl, alt, shift, win.
    pub hotkey: String,
    /// Grid contents, top to bottom. Order here is order on screen.
    pub sections: Vec<SectionConfig>,
    pub grid: Grid,
    pub theme: Theme,
}

/// Where a section's tiles come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Apps pinned to the Windows taskbar, read from disk.
    Taskbar,
    /// Every open window.
    Windows,
    /// Whatever is listed in `items`.
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SectionConfig {
    /// Shown as the section header. Empty string hides the header.
    pub title: String,
    pub source: Source,
    /// Only read when `source = "manual"`. Each entry is a shell parsing name:
    /// a file, a folder, a .lnk, `shell:AppsFolder\<AppUserModelID>`, or a URI
    /// such as `ms-settings:display` or `https://example.com`.
    #[serde(default)]
    pub items: Vec<ManualItem>,
}

/// A manual entry, either bare or with a chosen label:
///
/// ```toml
/// items = [
///   "R:\dev",
///   { title = "Display", target = "ms-settings:display" },
/// ]
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ManualItem {
    Plain(String),
    Named { title: String, target: String },
}

impl ManualItem {
    pub fn target(&self) -> &str {
        match self {
            ManualItem::Plain(target) => target,
            ManualItem::Named { target, .. } => target,
        }
    }

    pub fn title(&self) -> Option<&str> {
        match self {
            ManualItem::Plain(_) => None,
            ManualItem::Named { title, .. } => Some(title),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Grid {
    /// Tile size is fixed and never changes with item count — that stability is
    /// what makes the grid learnable. See DESIGN.md "Resolved".
    pub tile_width: f32,
    pub tile_height: f32,
    /// Space between tiles.
    pub gap: f32,
    /// Space between the outermost tiles and the panel edge.
    pub padding: f32,
    /// The grid grows outward from center until it reaches this fraction of the
    /// monitor work area, then stops widening and starts scrolling.
    pub max_screen_fraction: f32,
    /// Height reserved inside each tile for its label.
    pub label_height: f32,
    /// Height of a section header.
    pub header_height: f32,
    /// Extra space above each section after the first.
    pub section_gap: f32,
    pub corner_radius: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Theme {
    /// "#AARRGGBB" or "#RRGGBB".
    pub panel: String,
    pub tile: String,
    pub tile_hover: String,
    pub text: String,
    pub header: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            dry_run: false,
            hotkey: "alt+`".into(),
            sections: vec![
                SectionConfig {
                    title: "Pinned".into(),
                    source: Source::Taskbar,
                    items: Vec::new(),
                },
                SectionConfig {
                    title: "Windows".into(),
                    source: Source::Windows,
                    items: Vec::new(),
                },
                // Empty by default, and empty sections do not render. Present so
                // the shape is discoverable without reading the docs.
                SectionConfig {
                    title: "Places".into(),
                    source: Source::Manual,
                    items: Vec::new(),
                },
            ],
            grid: Grid::default(),
            theme: Theme::default(),
        }
    }
}

impl Default for Grid {
    fn default() -> Self {
        Self {
            tile_width: 220.0,
            tile_height: 150.0,
            gap: 14.0,
            padding: 24.0,
            max_screen_fraction: 0.8,
            label_height: 30.0,
            header_height: 28.0,
            section_gap: 14.0,
            corner_radius: 10.0,
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            panel: "#F01A1A1E".into(),
            tile: "#FF2A2A32".into(),
            tile_hover: "#FF3C3C48".into(),
            text: "#FFE8E8EC".into(),
            header: "#FF9A9AA8".into(),
        }
    }
}

impl Config {
    pub fn path() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        Some(exe.parent()?.join("flick.toml"))
    }

    /// Never fails: a broken or absent config falls back to defaults rather than
    /// refusing to start. A launcher that won't launch is worse than one with
    /// stock settings.
    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            log_warn!("could not resolve config path; using defaults");
            return Self::default();
        };

        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(cfg) => {
                    log_info!("config loaded from {}", path.display());
                    cfg.validated()
                }
                Err(e) => {
                    log_warn!("config at {} is invalid ({e}); using defaults", path.display());
                    Self::default()
                }
            },
            Err(_) => {
                let cfg = Self::default();
                cfg.write_to(&path);
                cfg
            }
        }
    }

    fn write_to(&self, path: &std::path::Path) {
        match toml::to_string_pretty(self) {
            Ok(text) => match std::fs::write(path, text) {
                Ok(()) => log_info!("wrote default config to {}", path.display()),
                Err(e) => log_warn!("could not write config to {}: {e}", path.display()),
            },
            Err(e) => log_warn!("could not serialize default config: {e}"),
        }
    }

    /// Clamp anything that would produce a degenerate or offscreen layout.
    fn validated(mut self) -> Self {
        let d = Grid::default();
        let g = &mut self.grid;
        if !(16.0..=1024.0).contains(&g.tile_width) {
            log_warn!("tile_width {} out of range; using {}", g.tile_width, d.tile_width);
            g.tile_width = d.tile_width;
        }
        if !(16.0..=1024.0).contains(&g.tile_height) {
            log_warn!("tile_height {} out of range; using {}", g.tile_height, d.tile_height);
            g.tile_height = d.tile_height;
        }
        if !(0.0..=256.0).contains(&g.gap) {
            g.gap = d.gap;
        }
        if !(0.0..=256.0).contains(&g.padding) {
            g.padding = d.padding;
        }
        if !(0.1..=1.0).contains(&g.max_screen_fraction) {
            log_warn!(
                "max_screen_fraction {} out of range; using {}",
                g.max_screen_fraction, d.max_screen_fraction
            );
            g.max_screen_fraction = d.max_screen_fraction;
        }
        if !(0.0..=200.0).contains(&g.label_height) {
            g.label_height = d.label_height;
        }
        if !(0.0..=200.0).contains(&g.header_height) {
            g.header_height = d.header_height;
        }
        if !(0.0..=256.0).contains(&g.section_gap) {
            g.section_gap = d.section_gap;
        }
        if !(0.0..=128.0).contains(&g.corner_radius) {
            g.corner_radius = d.corner_radius;
        }

        if self.sections.is_empty() {
            log_warn!("config has no sections; falling back to the default set");
            self.sections = Config::default().sections;
        }
        self
    }
}

/// Parsed `hotkey` string, ready for `RegisterHotKey`.
pub struct Hotkey {
    pub modifiers: HOT_KEY_MODIFIERS,
    pub vk: u32,
}

/// Parse "ctrl+alt+space". Returns `None` if there is no modifier or no key —
/// `RegisterHotKey` with no modifier would hijack a bare key system-wide.
pub fn parse_hotkey(spec: &str) -> Option<Hotkey> {
    let mut modifiers = HOT_KEY_MODIFIERS(0);
    let mut vk = None;

    for part in spec.split('+') {
        let part = part.trim().to_ascii_lowercase();
        match part.as_str() {
            "" => continue,
            "ctrl" | "control" => modifiers |= MOD_CONTROL,
            "alt" => modifiers |= MOD_ALT,
            "shift" => modifiers |= MOD_SHIFT,
            "win" | "super" | "meta" => modifiers |= MOD_WIN,
            key => {
                if vk.is_some() {
                    log_warn!("hotkey '{spec}' names more than one key");
                    return None;
                }
                vk = Some(vk_from_name(key)?);
            }
        }
    }

    let vk = vk?;
    if modifiers.0 == 0 {
        log_warn!("hotkey '{spec}' has no modifier; refusing to bind a bare key");
        return None;
    }
    Some(Hotkey { modifiers, vk })
}

fn vk_from_name(name: &str) -> Option<u32> {
    // Single character keys map to their uppercase ASCII value, which is the VK
    // for letters and digits.
    if name.len() == 1 {
        let c = name.chars().next()?.to_ascii_uppercase();
        if c.is_ascii_alphanumeric() {
            return Some(c as u32);
        }
    }
    Some(match name {
        "space" => 0x20,
        "tab" => 0x09,
        "enter" | "return" => 0x0D,
        "esc" | "escape" => 0x1B,
        "backspace" => 0x08,
        "insert" => 0x2D,
        "delete" => 0x2E,
        "home" => 0x24,
        "end" => 0x23,
        "pageup" => 0x21,
        "pagedown" => 0x22,
        "left" => 0x25,
        "up" => 0x26,
        "right" => 0x27,
        "down" => 0x28,
        "`" | "grave" | "backtick" | "tilde" => 0xC0,
        "-" | "minus" => 0xBD,
        "=" | "equals" => 0xBB,
        "[" => 0xDB,
        "]" => 0xDD,
        "\\" => 0xDC,
        ";" => 0xBA,
        "'" => 0xDE,
        "," => 0xBC,
        "." => 0xBE,
        "/" => 0xBF,
        f if f.starts_with('f') => {
            let n: u32 = f[1..].parse().ok()?;
            if !(1..=24).contains(&n) {
                return None;
            }
            0x70 + (n - 1)
        }
        other => {
            log_warn!("unknown key name in hotkey: '{other}'");
            return None;
        }
    })
}

/// "#AARRGGBB" / "#RRGGBB" -> (a, r, g, b). Falls back to opaque magenta so a
/// typo is visible rather than invisible.
pub fn parse_color(spec: &str) -> (u8, u8, u8, u8) {
    let hex = spec.trim().trim_start_matches('#');
    let parsed = match hex.len() {
        6 => u32::from_str_radix(hex, 16).ok().map(|v| 0xFF00_0000 | v),
        8 => u32::from_str_radix(hex, 16).ok(),
        _ => None,
    };
    match parsed {
        Some(v) => ((v >> 24) as u8, (v >> 16) as u8, (v >> 8) as u8, v as u8),
        None => {
            log_warn!("could not parse color '{spec}'");
            (0xFF, 0xFF, 0x00, 0xFF)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_default_hotkey() {
        let spec = Config::default().hotkey;
        let hk = parse_hotkey(&spec).expect("the default hotkey must parse");
        assert_eq!(hk.modifiers, MOD_ALT);
        assert_eq!(hk.vk, 0xC0);
    }

    #[test]
    fn parses_the_ctrl_alt_form_too() {
        let hk = parse_hotkey("ctrl+alt+space").unwrap();
        assert_eq!(hk.modifiers, MOD_CONTROL | MOD_ALT);
        assert_eq!(hk.vk, 0x20);
    }

    #[test]
    fn rejects_bare_keys_and_junk() {
        assert!(parse_hotkey("space").is_none());
        assert!(parse_hotkey("ctrl+nonsense").is_none());
        assert!(parse_hotkey("ctrl+a+b").is_none());
        assert!(parse_hotkey("ctrl").is_none());
    }

    #[test]
    fn parses_letters_and_function_keys() {
        assert_eq!(parse_hotkey("win+k").unwrap().vk, 'K' as u32);
        assert_eq!(parse_hotkey("alt+f4").unwrap().vk, 0x73);
        assert!(parse_hotkey("alt+f25").is_none());
    }

    #[test]
    fn parses_colors_with_and_without_alpha() {
        assert_eq!(parse_color("#204080"), (0xFF, 0x20, 0x40, 0x80));
        assert_eq!(parse_color("#80204080"), (0x80, 0x20, 0x40, 0x80));
    }

    #[test]
    fn default_config_round_trips() {
        let text = toml::to_string_pretty(&Config::default()).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.hotkey, Config::default().hotkey);
        assert_eq!(back.sections.len(), 3);
        assert_eq!(back.sections[0].source, Source::Taskbar);
        assert_eq!(back.sections[1].source, Source::Windows);
    }

    #[test]
    fn a_hand_written_manual_section_parses() {
        let text = r#"
hotkey = "alt+`"

[[sections]]
title = "Places"
source = "manual"
items = ["R:\\dev", "ms-settings:display"]
"#;
        let cfg: Config = toml::from_str(text).unwrap();
        assert_eq!(cfg.sections.len(), 1);
        assert_eq!(cfg.sections[0].source, Source::Manual);
        assert_eq!(cfg.sections[0].items[1].target(), "ms-settings:display");
        assert_eq!(cfg.sections[0].items[1].title(), None);
    }

    #[test]
    fn a_manual_item_can_carry_its_own_title() {
        let text = r#"
[[sections]]
title = "Places"
source = "manual"
items = [{ title = "Display", target = "ms-settings:display" }]
"#;
        let cfg: Config = toml::from_str(text).unwrap();
        let item = &cfg.sections[0].items[0];
        assert_eq!(item.title(), Some("Display"));
        assert_eq!(item.target(), "ms-settings:display");
    }

    #[test]
    fn empty_section_list_falls_back_rather_than_showing_nothing() {
        let cfg = Config { sections: Vec::new(), ..Config::default() }.validated();
        assert!(!cfg.sections.is_empty());
    }
}
