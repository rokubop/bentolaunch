//! Config lives next to the binary (safety rule 2: portable single exe, no
//! scattered state). Missing file => defaults, written out on first run so it is
//! discoverable and editable.

use std::path::{Path, PathBuf};

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
    pub center: Center,
}

/// The block held in the middle of the panel.
///
/// The centre of the screen is where a gaze pointer is most accurate, so it is
/// the one piece of the panel worth reserving rather than letting the bento
/// spend it on whatever happened to be listed first. What goes there is chosen
/// by hand and stays put: it is the only part of the grid whose contents do not
/// change with what is running.
///
/// Two halves, because the two things worth putting there answer different
/// questions - an app to start and a page to open - and mixing them would cost
/// the block the one thing it has over the rest of the grid, which is that you
/// already know what is in it without reading.
///
/// The centre does not sit in the bento tree. Every cut in that tree runs edge
/// to edge, so a box in the middle would drag its lines across the whole panel.
/// Instead it claims its rectangle first and the tree is planned as if it were
/// not there; the boxes wrap around it. See `ui::grid::Layout::compute`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Center {
    /// Rows of tiles in the block. **0 turns it off**, the same way
    /// `max_columns = 0` means no cap: how much centre you want and whether you
    /// want any are one question, and one settings square answers it.
    pub rows: usize,
    /// Tiles across in each half. `rows * columns` is how many slots a half
    /// has, and empty ones are drawn: the shape has to be the same every
    /// summon or there is nothing to learn the position of.
    pub columns: usize,
    /// Which of the two lists the block holds, and whether they are kept
    /// apart. Four answers to one question, so the square that steps through
    /// them cannot leave the block in a state no square can name.
    pub contents: Contents,
    /// Tint behind the block, "#AARRGGBB" or "#RRGGBB". This is the one box
    /// that is always in the same place, so it is the one worth marking out.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Shell parsing names, the same strings a manual section's `items` takes.
    /// Written by center mode; hand-editable like everything else here.
    pub apps: Vec<ManualItem>,
    /// URLs, and anything else the shell opens. Same form as `apps`.
    pub sites: Vec<ManualItem>,
}

impl Default for Center {
    fn default() -> Self {
        Self {
            // Off. A block nobody has put anything in is eighteen empty
            // squares in the middle of the screen, which is the most expensive
            // place on the panel spent on nothing. The first favorite turns it
            // on at a shape that fits - see `Center::shape_for`.
            //
            // A width is still named, so the block that comes back when it is
            // switched on by hand is a block rather than a column of one.
            rows: 0,
            columns: 3,
            contents: Contents::Split,
            // Warm, low alpha. It sits over a translucent panel, so anything
            // opaque would punch a hole in it.
            color: Some("#38FFC24B".into()),
            apps: Vec::new(),
            sites: Vec::new(),
        }
    }
}

/// What the centre block is holding.
///
/// The block is two lists that answer different questions - an app to start and
/// a page to open - and not everyone wants both. Kept as one setting rather
/// than a switch per list, because "apps only" and "sites only" and "both, but
/// mixed" are the same question asked once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Contents {
    /// Apps left, sites right. Two halves, a seam between them.
    #[default]
    Split,
    /// Both lists, apps first, in one block.
    One,
    /// Apps alone.
    Apps,
    /// Sites alone.
    Sites,
}

impl Contents {
    /// What this is called in the file. The same spelling serde reads, in one
    /// place, so the settings square and the parser cannot drift apart.
    pub fn key(self) -> &'static str {
        match self {
            Contents::Split => "split",
            Contents::One => "one",
            Contents::Apps => "apps",
            Contents::Sites => "sites",
        }
    }

    /// Whether this half is on the panel at all. Favoriting something the block
    /// would not show is a click that writes to a list nobody can see, so this
    /// is also what greys those tiles out in center mode.
    pub fn shows(self, half: usize) -> bool {
        match self {
            Contents::Split | Contents::One => true,
            Contents::Apps => half == 0,
            Contents::Sites => half == 1,
        }
    }

    /// Whether the block's `drawn`th half on screen is holding list `half`.
    ///
    /// The two are not the same number. Sites alone draws the sites list as the
    /// block's first and only half, and one block draws both lists in it - so
    /// "which square does a page land in" cannot be answered from the list's
    /// own index. Everything that points at a landing square asks this.
    pub fn holds(self, drawn: usize, half: usize) -> bool {
        match self {
            Contents::Split => drawn == half,
            Contents::One => drawn == 0,
            Contents::Apps => drawn == 0 && half == 0,
            Contents::Sites => drawn == 0 && half == 1,
        }
    }
}

impl Center {
    /// The most tiles the block may take either way. Past this the grid around
    /// it has nowhere to wrap to, and `validated` clamps to the same number.
    pub const MOST: usize = 4;

    /// The smallest shape with room for `slots` in a half.
    ///
    /// Every rectangle up to the cap, not a ladder of chosen ones: adding a
    /// favorite should cost the block the room that favorite needs and no more,
    /// and a ladder stepping 2 x 2 to 3 x 2 hands back two empty squares for
    /// one tile. A rectangle cannot fit every count exactly - five is 3 x 2 or
    /// nothing - but this is always the least that holds them.
    ///
    /// Wider before taller when the area ties, because a block is read along
    /// its rows: four are 4 x 1, not 2 x 2.
    ///
    /// Only ever bigger. A block that shrank as it emptied would be moving
    /// targets, so coming back down stays an edit-mode click.
    pub fn shape_for(slots: usize) -> (usize, usize) {
        (1..=Self::MOST)
            .flat_map(|rows| (1..=Self::MOST).map(move |columns| (columns, rows)))
            .filter(|(columns, rows)| columns * rows >= slots)
            .min_by_key(|&(columns, rows)| (columns * rows, Self::MOST - columns))
            .unwrap_or((Self::MOST, Self::MOST))
    }

    /// Whether the block is drawn at all.
    pub fn on(&self) -> bool {
        self.rows > 0 && self.columns > 0
    }

    /// Slots in one half.
    pub fn slots(&self) -> usize {
        self.rows * self.columns
    }
}

/// Loopback WebSocket the extension dials into.
///
/// Off by default, and switching it on grants nothing on its own: a caller
/// still has to arrive from a paired origin and prove it knows that peer's
/// token. A socket that hands out your open tabs is not something to switch on
/// by accident. The gates are in `browser::gate`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Browser {
    pub enabled: bool,
    /// Loopback only. Never bound on any other interface.
    pub port: u16,
}

impl Default for Browser {
    fn default() -> Self {
        Self {
            enabled: false,
            port: 8777,
        }
    }
}

/// Where a section's tiles come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Apps pinned to the Windows taskbar, read from disk.
    Taskbar,
    /// Every installed app, from `shell:AppsFolder`.
    ///
    /// Not a box to list in the file: it is what all-apps mode fills the panel
    /// with, and a box of three hundred tiles is not a box. It is named here
    /// because every tile carries where it came from.
    #[serde(rename = "allapps")]
    AllApps,
    /// Every bookmark a paired browser has, not just the bar. Asked for rather
    /// than sent, so it is empty until the all-bookmarks square has been
    /// clicked once. Not a box to list in the file, for the same reason
    /// `allapps` is not.
    #[serde(rename = "allbookmarks")]
    AllBookmarks,
    /// Every open window.
    Windows,
    /// `windows`, minus the redundant half: only apps with more than one window
    /// open.
    ///
    /// One window is already on the panel as an app tile, so repeating it by
    /// title says nothing. Four windows is the opposite, since the app tile
    /// reaches only the most recent and the titles are what pick the rest.
    ///
    /// Assumes apps come from `taskbar` and `running`. Alone it would leave
    /// single-window apps unreachable.
    Extra,
    /// One tile per open-but-unpinned app, after the pins and never among them.
    /// The taskbar's own rule: an app that comes and goes cannot hold a fixed
    /// slot, so it takes the one position costing nothing to learn.
    ///
    /// Deduplicated against what the section already listed, so a pin is never
    /// doubled by its own running app.
    Running,
    /// Whatever is listed in `items`.
    Manual,
    /// Open browser tabs, from the extension. Empty until one connects.
    Tabs,
    /// The browser's bookmarks bar, from the extension. Read-only: bentolaunch
    /// never writes to a browser profile (safety rule 4).
    Bookmarks,
    /// The six window moves. A fixed set, so nothing enumerates and the box is
    /// the same shape every summon.
    ///
    /// Empty unless the panel is in move mode, when `modes` is also on the
    /// panel. Six squares that only ever apply to one window at a time are a
    /// row this app cannot afford to spend all the time; a `modes` box brings
    /// them out on the click that needs them and puts them away after.
    ///
    /// Listed on its own, with no `modes` box anywhere, it is the old always-on
    /// bar - `Stay open` and the six - because that is a bar some people will
    /// want and nothing about it stopped working.
    #[serde(rename = "move")]
    Moves,
    /// One tile per mode: move a window, center, close apps, edit layout.
    ///
    /// A fixed set in a fixed order, like `move`. This is the bar that replaces
    /// the move bar: four squares that are always the same four in the same
    /// places, each one turning on a mode and each one turning it off again.
    Modes,
    /// What `[center]` holds, apps then sites.
    ///
    /// The centre block is where these normally appear, and it is not a
    /// section. This is here so the one list has one name wherever it turns up:
    /// it is what tags a tile as belonging to the centre, which is how a
    /// favorite is left out of the list it came from and how removing one knows
    /// where to write.
    Center,
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
    /// Tint behind this box, "#AARRGGBB" or "#RRGGBB". Absent leaves it on the
    /// panel colour.
    ///
    /// The alpha is the point: these sit over a translucent panel, and an
    /// opaque plate would punch a hole in it. Something in the low twenties
    /// reads as a tint rather than as a second surface.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Ring around the box, and the colour of its title. `"#AARRGGBB"` or
    /// `"#RRGGBB"`. Unset takes the next colour off `theme.section_edges`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge: Option<String>,
    /// Which band across the panel this box sits in: `"left"`, `"right"` or
    /// `"full"`. Order in this file is order down the lane.
    ///
    /// A property of this box, not a relationship with another one. `at` said
    /// "left" by cutting the panel in two and taking the near half, so it
    /// stopped meaning left the moment nothing was on the right.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side: Option<String>,
    /// Tile columns inside this box. 0 fits as many as its rectangle takes.
    #[serde(default)]
    pub columns: usize,
    /// Most tiles this box will show. 0 means all of them. What a bookmarks or
    /// tabs box needs: both lists are as long as the browser makes them, and a
    /// box that grows without limit is one that pushes the rest off screen.
    #[serde(default)]
    pub max_items: usize,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Height of the title plate that rides the ring round a box.
    ///
    /// Not a row above the tiles any more: it costs no layout at all, so this
    /// is how tall the mark on the line is and nothing else. `0` hides titles.
    pub header_height: f32,
    /// How far along the ring's top edge the title sits, from the box's left
    /// corner. Enough to clear the corner arc.
    pub header_gap: f32,
    /// Clear rows between a box and the box stacked under it. In pixels, and
    /// rounded to whole rows - anything under half a row is none.
    ///
    /// Never a fraction of a row. The panel is one lattice and every tile in
    /// every box sits on it, so a box cannot be nudged off it to be told apart
    /// from its neighbour. The coloured ring is what does that.
    pub section_gap: f32,
    pub corner_radius: f32,
    /// Share of the columns the left lane takes. One seam for the whole panel,
    /// because there is only one line down the middle to argue about.
    ///
    /// Hand-edited: no square writes it. Which lane a box is in is a question
    /// about that box, and a width square beside the three that answer it read
    /// as a fourth answer.
    pub split: f32,
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
    /// holding the panel open. The warmest of the three states: it is the one
    /// that means something is switched on.
    pub tile_drag: String,
    /// The tile Enter would take. Distinct from `tile_hover`: cursor and
    /// keyboard can point at different tiles.
    pub tile_selected: String,
    /// Ring around the window the move bar acts on. An outline rather than a
    /// fill: it has to survive hover and selection colouring the same tile,
    /// and it is a different question from either of them.
    ///
    /// The logo's own warm colour. Selection, drag and this are one family told
    /// apart by weight, so nothing on the panel is accented in a second hue.
    pub tile_target: String,
    /// What a box's ring is drawn in when `section_edges` is empty. One colour
    /// for every box, which is the panel as it was before boxes wore colours of
    /// their own. `"#00000000"` turns rings off altogether.
    pub box_edge: String,
    /// Line around the centre block, and the seam down the middle of it.
    ///
    /// Distinctly stronger than `box_edge`, because the block is the one thing
    /// on the panel that is *in front of* the layout rather than part of it.
    /// The accent again, so nothing is picked out in a second hue.
    pub center_edge: String,
    /// Ring colours dealt out to sections in order, when a section does not
    /// name its own `edge`.
    ///
    /// The ring is what says which box this is, which is what lets the title
    /// shrink to a mark on it instead of taking a row above it. A panel nobody
    /// has configured still comes out with its boxes told apart.
    ///
    /// No amber in here. That hue belongs to the centre block and the tile it
    /// is pointed at, and a box ring wearing it would read as one of those.
    /// Empty falls back to `box_edge` for every box, which is the old
    /// one-colour panel.
    pub section_edges: Vec<String>,
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
        color: None,
        edge: None,
        side: None,
        columns: 0,
        max_items: 0,
        items: Vec::new(),
        order: Vec::new(),
    }
}

fn placed(title: &str, sources: &[SourceSpec], side: &str) -> SectionConfig {
    SectionConfig { side: Some(side.into()), ..section(title, sources) }
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
            // Browsing down the whole right side, everything else down the
            // left. Two halves and one question each: what is open on the web,
            // and what is on this machine. A panel split that way is answered
            // by looking at one half of it, which no stack of full-width rows
            // ever manages - and it gives the centre block a half to sit in on
            // either side of it.
            //
            // Fixed boxes lead each lane. Launch and Bookmarks hold their
            // tiles still. Active and Browsing are as long as whatever is open,
            // and a box that changes height walks the rest of its lane down.
            sections: vec![
                placed(
                    "Launch",
                    &[SourceSpec::Plain(Source::Taskbar), SourceSpec::Plain(Source::Manual)],
                    "left",
                ),
                // Bookmarks are a box of their own, above the tabs rather than
                // merged in with them. Three groups under one header told
                // apart only by an alternating tile fill said "these belong
                // together" about two lists that answer different questions:
                // somewhere you go, and somewhere you already are. The row a
                // second box used to cost is what kept them merged, and a
                // title costs no row now.
                //
                // Capped: 32 bookmarks ran the right lane off the bottom of
                // the screen on the first summon.
                SectionConfig {
                    max_items: 10,
                    ..placed("Bookmarks", &[SourceSpec::Plain(Source::Bookmarks)], "right")
                },
                // Before Active, though it draws in the other lane: `claimed`
                // in `model/store.rs` runs the list in order, so Active's
                // catch-all would take the browser windows first.
                placed(
                    "Browsing",
                    &[group(Source::Windows, BROWSERS), SourceSpec::Plain(Source::Tabs)],
                    "right",
                ),
                placed(
                    "Active",
                    &[
                        group(Source::Windows, &["explorer.exe"]),
                        SourceSpec::Plain(Source::Windows),
                    ],
                    "left",
                ),
                // A `move` box, empty until move mode brings the six out. An
                // empty section draws nothing, so this costs a row only while
                // it is being used - which is the whole reason the moves
                // stopped being a bar of their own.
                //
                // Listed first of the two so it stacks above the modes bar:
                // that bar is the one row whose position never changes, and a
                // box appearing under it would push it off the place it is
                // aimed at.
                placed("", &[SourceSpec::Plain(Source::Moves)], "full"),
                // Untitled: four squares that each say what they are, under a
                // header that would only say it again.
                placed("", &[SourceSpec::Plain(Source::Modes)], "full"),
            ],
            grid: Grid::default(),
            center: Center::default(),
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
            header_height: 16.0,
            header_gap: 14.0,
            section_gap: 0.0,
            corner_radius: 8.0,
            split: 0.5,
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
            tile_drag: "#FF7A5326".into(),
            tile_selected: "#FF4E4230".into(),
            tile_target: "#FFFFC24B".into(),
            box_edge: "#14FFFFFF".into(),
            center_edge: "#66FFC24B".into(),
            // Quieter than `center_edge` on purpose: the block is in front of
            // the layout and has to win. Far enough apart in hue to be told
            // apart at a glance from the middle of the screen, which is the
            // only way this gets read.
            section_edges: vec![
                "#5A4FD1C5".into(),
                "#5AA78BFA".into(),
                "#5A60A5FA".into(),
                "#5AF472B6".into(),
                "#5A6EE7A8".into(),
            ],
        }
    }
}

impl Config {
    pub fn path() -> Option<PathBuf> {
        Some(Self::path_in(std::env::current_exe().ok()?.parent()?))
    }

    fn path_in(dir: &Path) -> PathBuf {
        dir.join("bentolaunch.toml")
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
    ///
    /// `pub(crate)` so the settings squares can prove their presets survive it.
    /// A preset that got clamped would be a knob that silently does nothing.
    pub(crate) fn validated(mut self) -> Self {
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

        // The centre is held in the middle of the panel and everything else
        // wraps around it, so a block bigger than the panel would leave the
        // grid nowhere to wrap to. Four each way is already half a screen.
        let f = &mut self.center;
        if f.rows > Center::MOST {
            log_warn!("center.rows {} is more than {}; clamped", f.rows, Center::MOST);
            f.rows = Center::MOST;
        }
        if f.columns > Center::MOST {
            log_warn!("center.columns {} is more than {}; clamped", f.columns, Center::MOST);
            f.columns = Center::MOST;
        }
        // Rows alone says whether the block is on. A width of zero with rows
        // asked for is a typo, not a way to turn it off.
        if f.rows > 0 && f.columns == 0 {
            f.columns = Center::default().columns;
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
    fn a_drawn_half_is_not_the_list_it_holds() {
        // Split is the only case where the two numbers agree.
        assert!(Contents::Split.holds(0, 0) && Contents::Split.holds(1, 1));
        assert!(!Contents::Split.holds(0, 1));
        // Sites alone draws the sites list as the block's first half, so the
        // square a page lands in is drawn half 0 holding list 1.
        assert!(Contents::Sites.holds(0, 1));
        assert!(!Contents::Sites.holds(0, 0));
        assert!(Contents::Apps.holds(0, 0) && !Contents::Apps.holds(0, 1));
        // One block: both lists land in the one half that is drawn.
        assert!(Contents::One.holds(0, 0) && Contents::One.holds(0, 1));
        assert!(!Contents::One.holds(1, 0));
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
        // Four of content, then the two bars along the bottom.
        assert_eq!(back.sections.len(), 6);
        assert!(back.sections[0].source.contains(Source::Taskbar));
        assert_eq!(back.center.rows, Config::default().center.rows);
        assert_eq!(back.center.columns, Config::default().center.columns);
        assert_eq!(back.center.contents, Config::default().center.contents);
    }



    #[test]
    fn the_block_starts_off_and_empty() {
        let f = Config::default().center;
        assert!(!f.on(), "a fresh panel should not be holding empty squares");
        assert_eq!(f.slots(), 0);
        assert_eq!(f.contents, Contents::Split);
        assert!(f.apps.is_empty() && f.sites.is_empty());
        // A width is still named. Switching the block on by hand has to give
        // back a block rather than a column of one.
        assert_eq!(f.columns, 3);
    }

    #[test]
    fn the_block_grows_by_what_it_needs_and_no_more() {
        // Off holds nothing, so the first favorite is what turns it on - and it
        // costs one square, not a shape with spares in it.
        assert_eq!(Center::shape_for(1), (1, 1));
        assert_eq!(Center::shape_for(2), (2, 1));
        assert_eq!(Center::shape_for(3), (3, 1));
        // Wider before taller on a tie: a block is read along its rows.
        assert_eq!(Center::shape_for(4), (4, 1));
        // A rectangle cannot hold five exactly. Six is the least that does.
        assert_eq!(Center::shape_for(5), (3, 2));
        assert_eq!(Center::shape_for(6), (3, 2));
        assert_eq!(Center::shape_for(9), (3, 3));
        assert_eq!(Center::shape_for(16), (4, 4));
        // Past the biggest it stops rather than wrapping or refusing.
        assert_eq!(Center::shape_for(999), (4, 4));
    }

    #[test]
    fn no_shape_it_picks_is_wasteful_or_out_of_range() {
        let mut area = 0;
        for count in 1..=16 {
            let (columns, rows) = Center::shape_for(count);
            assert!(columns * rows >= count, "{count} does not fit {columns}x{rows}");
            assert!(columns <= Center::MOST && rows <= Center::MOST, "{columns}x{rows} is past the cap");
            // Never more than one row of slack, and never smaller than the last
            // answer: the block only grows.
            assert!(columns * rows < count + columns, "{count} got {columns}x{rows}, a row of spares");
            assert!(columns * rows >= area, "the block shrank between {} and {count}", count - 1);
            area = columns * rows;
        }
    }

    #[test]
    fn a_half_the_block_does_not_hold_is_not_shown() {
        assert!(Contents::Split.shows(0) && Contents::Split.shows(1));
        assert!(Contents::One.shows(0) && Contents::One.shows(1));
        assert!(Contents::Apps.shows(0) && !Contents::Apps.shows(1));
        assert!(!Contents::Sites.shows(0) && Contents::Sites.shows(1));
    }

    #[test]
    fn the_modes_bar_is_the_bottom_row_and_the_moves_stack_above_it() {
        // The modes bar is the one row whose place never changes. A box that
        // came and went underneath it would push it off the spot it is aimed at.
        let sections = Config::default().sections;
        let bar = |source| {
            sections
                .iter()
                .position(|s| s.source.contains(source))
                .unwrap()
        };
        assert!(bar(Source::Moves) < bar(Source::Modes));
        for source in [Source::Moves, Source::Modes] {
            assert_eq!(sections[bar(source)].side.as_deref(), Some("full"));
        }
    }

    #[test]
    fn the_moves_are_not_a_bar_of_their_own_when_there_is_a_mode_to_open_them() {
        // Six squares that only ever apply to one window at a time cannot hold
        // a row all the time. The default has a `modes` box, so they wait.
        let sections = Config::default().sections;
        assert!(sections.iter().any(|s| s.source.contains(Source::Modes)));
    }

    /// A lane led by a box that grows walks its other boxes down the panel.
    #[test]
    fn each_lane_leads_with_the_box_that_holds_still() {
        let volatile = |s: &SectionConfig| {
            s.source
                .iter()
                .all(|spec| matches!(spec.source(), Source::Windows | Source::Tabs))
        };
        let sections = Config::default().sections;
        for side in ["left", "right"] {
            let lane: Vec<_> =
                sections.iter().filter(|s| s.side.as_deref() == Some(side)).collect();
            assert!(lane.len() > 1, "{side} lane should hold more than one box");
            let first_volatile = lane.iter().position(|s| volatile(s)).unwrap();
            let last_fixed = lane.iter().rposition(|s| !volatile(s)).unwrap();
            assert!(
                last_fixed < first_volatile,
                "{side} lane: {} grows with what is open and must not lead it",
                lane[first_volatile].title
            );
        }
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

    /// `claimed` walks the list in order, so a catch-all above Browsing takes
    /// the browser windows before Browsing sees them. Lane does not matter.
    #[test]
    fn the_catch_all_is_listed_after_the_filtered_browser_group() {
        let sections = Config::default().sections;
        let at = |title: &str| {
            sections
                .iter()
                .position(|s| s.title == title)
                .unwrap_or_else(|| panic!("no {title} box in the defaults"))
        };

        let browsing: Vec<_> = sections[at("Browsing")].source.iter().collect();
        assert_eq!(browsing[0].source(), Source::Windows);
        assert!(browsing[0].matches().unwrap().iter().any(|m| m == "chrome.exe"));
        assert_eq!(browsing[1].source(), Source::Tabs);

        let catch_all = sections[at("Active")].source.iter().last().unwrap();
        assert_eq!(catch_all.source(), Source::Windows);
        assert_eq!(catch_all.matches(), None);

        assert!(
            at("Browsing") < at("Active"),
            "the catch-all would claim the browser windows first"
        );
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

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("bentolaunch-test-config-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }



    #[test]
    fn a_fresh_install_names_the_new_config() {
        let dir = scratch("fresh");
        assert_eq!(Config::path_in(&dir), dir.join("bentolaunch.toml"));
    }
}
