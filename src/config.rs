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
    pub browser: Browser,
}

/// Loopback WebSocket the extension dials into.
///
/// Off by default and until paired: `enabled` alone is not enough. A socket
/// that hands out your open tabs is not something to switch on by accident.
/// The two gates are in `browser::gate`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Browser {
    pub enabled: bool,
    /// Loopback only. Never bound on any other interface.
    pub port: u16,
    /// `chrome-extension://<id>` origins allowed to connect. Empty rejects
    /// everything. Refused origins are logged, so pairing is copy from the log.
    pub allow: Vec<String>,
    /// Legacy. The token now lives in `%LOCALAPPDATA%\bentopick\bridge-token`,
    /// which Windows restricts to this account. Anything left here is moved
    /// there on startup and this is blanked.
    pub token: String,
}

impl Default for Browser {
    fn default() -> Self {
        Self {
            enabled: false,
            port: 8777,
            allow: Vec::new(),
            token: String::new(),
        }
    }
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
    /// Open browser tabs, from the extension. Empty until one connects.
    Tabs,
}

/// One group of tiles inside a section: where they come from, and for windows
/// which processes it claims.
///
/// Written bare when it claims everything left, or as a table to carry its own
/// `match`:
///
/// ```toml
/// source = [
///   { source = "windows", match = ["chrome.exe"] },
///   "tabs",
///   "windows",
/// ]
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SourceSpec {
    Plain(Source),
    Matched {
        source: Source,
        #[serde(default, rename = "match")]
        matches: Vec<String>,
    },
}

impl SourceSpec {
    pub fn source(&self) -> Source {
        match self {
            Self::Plain(source) => *source,
            Self::Matched { source, .. } => *source,
        }
    }

    /// `None` when this group carries no rule of its own, in which case the
    /// section's own `match` applies.
    pub fn matches(&self) -> Option<&[String]> {
        match self {
            Self::Plain(_) => None,
            Self::Matched { matches, .. } => Some(matches),
        }
    }
}

/// A section's groups, in the order their tiles appear under the one header.
///
/// `source = "windows"` and `source = ["windows", "tabs"]` are both valid. A
/// section costs a header plus a whole row even for one tile, so merging is how
/// a panel of mostly-empty sections gets its vertical space back. Grouping
/// survives the merge: the groups stay ordered and stay visually apart, they
/// just no longer each cost a header and a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sources(Vec<SourceSpec>);

impl Sources {
    pub fn iter(&self) -> impl Iterator<Item = &SourceSpec> {
        self.0.iter()
    }

    /// Only the pin-writing tests ask; the panel decides per item.
    #[cfg(test)]
    pub fn contains(&self, source: Source) -> bool {
        self.0.iter().any(|spec| spec.source() == source)
    }
}

impl From<Source> for Sources {
    fn from(source: Source) -> Self {
        Self(vec![SourceSpec::Plain(source)])
    }
}

impl Serialize for Sources {
    /// A single bare source round-trips as a string, so an unmerged section is
    /// left exactly as it was written.
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self.0.as_slice() {
            [one @ SourceSpec::Plain(_)] => one.serialize(s),
            many => many.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for Sources {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            One(SourceSpec),
            Many(Vec<SourceSpec>),
        }

        let list = match Raw::deserialize(d)? {
            Raw::One(spec) => vec![spec],
            Raw::Many(list) => list,
        };
        if list.is_empty() {
            return Err(serde::de::Error::custom("a section needs at least one source"));
        }
        Ok(Self(list))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SectionConfig {
    /// Shown as the section header. Empty string hides the header.
    pub title: String,
    pub source: Sources,
    /// Process names this section's windows groups claim, e.g.
    /// `["chrome.exe", "firefox.exe"]`. Case-insensitive. Empty means "whatever
    /// is left", so an unfiltered windows group acts as the catch-all.
    ///
    /// Groups are matched in order and a window is claimed once, so listing a
    /// filtered group above the catch-all is what groups apps together. A group
    /// that carries its own `match` ignores this; it is the fallback, and what
    /// a section with a single bare source still writes.
    #[serde(default, rename = "match")]
    pub matches: Vec<String>,
    /// Only read when `source = "manual"`. Each entry is a shell parsing name:
    /// a file, a folder, a .lnk, `shell:AppsFolder\<AppUserModelID>`, or a URI
    /// such as `ms-settings:display` or `https://example.com`.
    #[serde(default)]
    pub items: Vec<ManualItem>,
    /// Only for `source = "taskbar"`. Pin names, in the order they should
    /// appear. Windows does not expose the taskbar's own order (see
    /// `model/taskbar.rs`), so this is where dragging a taskbar tile in edit
    /// mode records what it did. Anything not listed keeps following, sorted by
    /// name.
    #[serde(default)]
    pub order: Vec<String>,
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
    /// Hard cap on columns, applied on top of `max_screen_fraction`. A very wide
    /// monitor would otherwise produce a row too long to scan in one look. 0
    /// means no cap beyond what fits the screen.
    pub max_columns: usize,
    /// Height reserved inside each tile for its label.
    pub label_height: f32,
    /// Show the second line (process name or path) under the title. Off by
    /// default: at compact tile sizes the title alone is what identifies a tile,
    /// and the second line costs a row of tiles across the whole panel.
    pub show_detail: bool,
    /// Height of a section header.
    pub header_height: f32,
    /// Breathing room between a section title and its first row of tiles.
    pub header_gap: f32,
    /// Extra space above each section after the first.
    pub section_gap: f32,
    pub corner_radius: f32,
    /// Filter strip. Only appears while there is a query. 0 filters silently.
    /// Its text is sized from this, so raising it makes the query bigger.
    pub search_height: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Theme {
    /// "#AARRGGBB" or "#RRGGBB".
    pub panel: String,
    pub tile: String,
    /// Fill for every other group inside a section. Merging cost the groups
    /// their headers, so alternating the tile fill is what still reads as
    /// "these belong together and those do not". Set it equal to `tile` to
    /// turn the banding off.
    pub tile_alt: String,
    pub tile_hover: String,
    pub text: String,
    pub header: String,
    /// Fill for a tile being dragged, and for the keep-open button while it is
    /// holding the panel open.
    pub tile_drag: String,
    /// The tile Enter would take. Distinct from `tile_hover`: cursor and
    /// keyboard can point at different tiles.
    pub tile_selected: String,
}

/// Browsers, grouped together because that is how they are thought about. Any
/// browser not listed simply lands in the catch-all section instead.
pub const BROWSERS: &[&str] = &[
    "chrome.exe",
    "msedge.exe",
    "firefox.exe",
    "brave.exe",
    "vivaldi.exe",
    "opera.exe",
    "arc.exe",
    "zen.exe",
];

fn section(title: &str, sources: &[SourceSpec]) -> SectionConfig {
    SectionConfig {
        title: title.into(),
        source: Sources(sources.to_vec()),
        matches: Vec::new(),
        items: Vec::new(),
        order: Vec::new(),
    }
}

fn group(source: Source, matches: &[&str]) -> SourceSpec {
    SourceSpec::Matched {
        source,
        matches: matches.iter().map(|s| (*s).to_string()).collect(),
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            dry_run: false,
            hotkey: "alt+`".into(),
            // Three sections, not six. A section costs a header plus a full row
            // even for one tile, so the ones that stayed had to earn the row.
            // The groups survive inside a section: still ordered, still tinted
            // apart, no longer a header and a row each.
            //
            // Browsing earns its own header. Browser windows and tabs answer
            // one question — get me back to a page — and there are enough of
            // them on any real machine to fill the row a header costs, which is
            // what the other splits could not do.
            //
            // Running things first: switching to what exists beats launching
            // something new, so it gets the top of the panel.
            sections: vec![
                section(
                    "Browsing",
                    &[group(Source::Windows, BROWSERS), SourceSpec::Plain(Source::Tabs)],
                ),
                section(
                    "Active",
                    &[
                        group(Source::Windows, &["explorer.exe"]),
                        SourceSpec::Plain(Source::Windows),
                    ],
                ),
                section(
                    "Launch",
                    &[SourceSpec::Plain(Source::Taskbar), SourceSpec::Plain(Source::Manual)],
                ),
            ],
            grid: Grid::default(),
            theme: Theme::default(),
            browser: Browser::default(),
        }
    }
}

impl Default for Grid {
    fn default() -> Self {
        Self {
            tile_width: 140.0,
            tile_height: 100.0,
            gap: 10.0,
            padding: 18.0,
            max_screen_fraction: 0.8,
            max_columns: 9,
            label_height: 24.0,
            show_detail: false,
            header_height: 22.0,
            header_gap: 6.0,
            section_gap: 10.0,
            corner_radius: 8.0,
            search_height: 72.0,
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            panel: "#F01A1A1E".into(),
            tile: "#FF2A2A32".into(),
            tile_alt: "#FF22222A".into(),
            tile_hover: "#FF3C3C48".into(),
            text: "#FFE8E8EC".into(),
            header: "#FF9A9AA8".into(),
            tile_drag: "#FF4A4460".into(),
            tile_selected: "#FF4C5A78".into(),
        }
    }
}

impl Config {
    pub fn path() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        Some(exe.parent()?.join("bentopick.toml"))
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
        if g.max_columns > 64 {
            log_warn!("max_columns {} is unreasonable; using {}", g.max_columns, d.max_columns);
            g.max_columns = d.max_columns;
        }
        if !(0.0..=200.0).contains(&g.label_height) {
            g.label_height = d.label_height;
        }
        if !(0.0..=200.0).contains(&g.header_gap) {
            g.header_gap = d.header_gap;
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
        if !(0.0..=200.0).contains(&g.search_height) {
            log_warn!(
                "search_height {} out of range; using {}",
                g.search_height, d.search_height
            );
            g.search_height = d.search_height;
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
        assert!(back.sections[0].source.contains(Source::Tabs));
    }

    #[test]
    fn running_things_are_listed_before_launchable_ones() {
        let running = |s: &SectionConfig| {
            s.source
                .iter()
                .all(|spec| matches!(spec.source(), Source::Windows | Source::Tabs))
        };
        let sections = Config::default().sections;
        let last_running = sections.iter().rposition(running).unwrap();
        let first_launch = sections.iter().position(|s| !running(s)).unwrap();
        assert!(
            last_running < first_launch,
            "everything already open must come before the launchers"
        );
    }

    #[test]
    fn tabs_share_a_section_with_the_browser_windows() {
        let sections = Config::default().sections;
        let tabs = sections.iter().find(|s| s.source.contains(Source::Tabs)).unwrap();
        let browser_windows = tabs
            .source
            .iter()
            .find(|spec| spec.source() == Source::Windows)
            .and_then(SourceSpec::matches)
            .unwrap_or_default();
        assert!(
            browser_windows.iter().any(|m| m == "chrome.exe"),
            "tabs sit with the browser windows: both answer get me back to a page"
        );
    }

    /// One group, across every section: a window is claimed once, so a second
    /// unfiltered group would never see one and the first would swallow the
    /// filtered groups listed after it.
    #[test]
    fn exactly_one_windows_group_is_an_unfiltered_catch_all() {
        let catch_alls = Config::default()
            .sections
            .iter()
            .flat_map(|s| s.source.iter().map(move |spec| (s, spec)))
            .filter(|(section, spec)| {
                spec.source() == Source::Windows
                    && spec.matches().unwrap_or(&section.matches).is_empty()
            })
            .count();
        assert_eq!(catch_alls, 1, "windows with no matching group must land somewhere");
    }

    #[test]
    fn a_source_reads_as_a_string_or_a_list() {
        let text = r#"
[[sections]]
title = "Active"
source = ["windows", "tabs"]

[[sections]]
title = "Launch"
source = "taskbar"
"#;
        let cfg: Config = toml::from_str(text).unwrap();
        let sources = |s: &SectionConfig| s.source.iter().map(SourceSpec::source).collect::<Vec<_>>();
        assert_eq!(sources(&cfg.sections[0]), [Source::Windows, Source::Tabs]);
        assert_eq!(sources(&cfg.sections[1]), [Source::Taskbar]);

        // A lone source goes back out bare, so an unmerged section is untouched.
        let out = toml::to_string(&cfg).unwrap();
        assert!(out.contains(r#"source = "taskbar""#), "{out}");
        assert!(out.contains(r#"source = ["windows", "tabs"]"#), "{out}");
    }

    /// The browsing section leads, and inside it the windows come before the
    /// tabs. The catch-all sits behind both, or it would claim the browser
    /// windows before the section above ever sees them.
    #[test]
    fn browsing_leads_and_the_catch_all_comes_after_it() {
        let sections = Config::default().sections;
        let browsing: Vec<_> = sections[0].source.iter().collect();

        assert_eq!(browsing[0].source(), Source::Windows);
        assert!(browsing[0].matches().unwrap().iter().any(|m| m == "chrome.exe"));
        assert_eq!(browsing[1].source(), Source::Tabs);

        let catch_all = sections[1].source.iter().last().unwrap();
        assert_eq!(catch_all.source(), Source::Windows);
        assert_eq!(catch_all.matches(), None);
    }

    #[test]
    fn a_group_carries_its_own_match_or_falls_back_to_the_sections() {
        let text = r#"
[[sections]]
title = "Active"
source = [{ source = "windows", match = ["chrome.exe"] }, "windows"]
match = ["explorer.exe"]
"#;
        let cfg: Config = toml::from_str(text).unwrap();
        let groups: Vec<_> = cfg.sections[0].source.iter().collect();
        assert_eq!(groups[0].matches(), Some(["chrome.exe".to_string()].as_slice()));
        assert_eq!(groups[1].matches(), None, "a bare source defers to the section");
        assert_eq!(cfg.sections[0].matches, ["explorer.exe"]);
    }

    #[test]
    fn a_section_with_no_source_is_rejected() {
        let text = "[[sections]]\ntitle = \"Nothing\"\nsource = []\n";
        assert!(toml::from_str::<Config>(text).is_err());
    }

    #[test]
    fn a_section_can_declare_process_matches() {
        let text = r#"
[[sections]]
title = "Browsing"
source = "windows"
match = ["chrome.exe", "firefox.exe"]
"#;
        let cfg: Config = toml::from_str(text).unwrap();
        assert_eq!(cfg.sections[0].matches, ["chrome.exe", "firefox.exe"]);
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
        assert!(cfg.sections[0].source.contains(Source::Manual));
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
