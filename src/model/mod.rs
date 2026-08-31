//! What bentolaunch knows about, and how it stays current.

pub mod store;
pub mod taskbar;
pub mod windows;

use ::windows::Win32::Foundation::HWND;

/// A window handle stored as its raw value.
///
/// Window handles are process-wide and valid from any thread; the pointer inside
/// `HWND` is the only reason windows-rs marks it `!Send`. Storing the raw value
/// keeps the item store `Send`, which matters because icon work runs on the
/// worker threads that safety rule 5 requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Handle(isize);

impl Handle {
    pub fn new(hwnd: HWND) -> Self {
        Self(hwnd.0 as isize)
    }

    pub fn hwnd(self) -> HWND {
        HWND(self.0 as *mut core::ffi::c_void)
    }

    pub fn raw(self) -> isize {
        self.0
    }
}

/// What the panel is doing with a click on a tile.
///
/// Modes rather than modifiers. Nothing that points with gaze can hold a key
/// down, so anything that changes what a click means has to be a square you aim
/// at once and a square you aim at to leave. Every mode holds the panel open,
/// because all of them are things you do several of in a row.
///
/// Two ways in, both of them squares: a row of mode tiles in the grid itself
/// (`Source::Modes`), and the app's own button in the corner. That button ends
/// every one of them, so there is never a mode with no visible way out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Mode {
    /// Clicking a tile takes it. The whole app, most of the time.
    #[default]
    Grid,
    /// Clicking picks a box and the options rearrange the bento.
    Layout,
    /// Clicking adds a tile to the centre block, or takes one out of it.
    Center,
    /// Clicking closes the window behind a tile.
    Close,
    /// Arranging windows. Holds the panel open, points it at the window you
    /// click, and brings out the six moves - which is the only reason they
    /// ever needed a row of their own.
    ///
    /// The one mode that does not take clicks off the grid. Clicking is how you
    /// pick the window to move, and clicking is how you move it.
    Move,
    /// Every installed app, in place of the grid.
    ///
    /// The `Apps` box mirrors the taskbar, which is a shortlist somebody
    /// curated - so an app that is not on it had no way onto the panel at all.
    /// This is that way in: the last square of the box opens the rest of them.
    ///
    /// Clicks mean what they always mean here. It is a mode only because it
    /// holds the panel open and is left by a square, which is what everything
    /// that changes the panel has to be.
    AllApps,
    /// Every bookmark, in place of the grid. The same answer as `AllApps` to
    /// the same question: the box shows the row somebody curated, and this is
    /// the rest of them.
    ///
    /// Flat, with the folder each one is filed under on its second line. The
    /// tree is an archive of thousands and walking it a folder at a time is
    /// several clicks to a place typing three letters already reaches.
    AllBookmarks,
}

impl Mode {
    /// What the corner button says while this mode is on. `None` for the mode
    /// that is not one: there the button is the app's own name.
    pub fn done(self) -> Option<&'static str> {
        match self {
            Mode::Grid => None,
            Mode::Layout => Some("Stop editing"),
            Mode::Center => Some("Done \u{00B7} center"),
            Mode::Close => Some("Done \u{00B7} closing"),
            Mode::Move => Some("Done \u{00B7} moving"),
            Mode::AllApps => Some("Done \u{00B7} all apps"),
            Mode::AllBookmarks => Some("Done \u{00B7} all bookmarks"),
        }
    }

    /// Whether this mode takes the clicks the grid would otherwise treat as
    /// launches.
    pub fn takes_clicks(self) -> bool {
        matches!(self, Mode::Layout | Mode::Center | Mode::Close)
    }

    /// The name on the tile that turns this mode on, and the tile's id.
    pub fn label(self) -> &'static str {
        match self {
            Mode::Grid => "Done",
            Mode::Layout => "Edit layout",
            Mode::Center => "Edit center",
            Mode::Close => "Close apps",
            Mode::Move => "Move window",
            Mode::AllApps => "All apps",
            Mode::AllBookmarks => "All bookmarks",
        }
    }
}

/// What activating a tile does.
///
/// Everything that is not a live window collapses to a **shell parsing name** —
/// the string form the shell already understands. A file path, a folder, a
/// `.lnk`, `shell:AppsFolder\<AppUserModelID>` for a Store app, and a URI like
/// `ms-settings:display` are all parsing names, and all of them both launch
/// through `ShellExecuteW` and produce an icon through `IShellItemImageFactory`.
/// One string covers every non-window thing bentolaunch can show.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Target {
    /// Focus this window.
    Window(Handle),
    /// Hand this to the shell.
    Shell(String),
    /// The one thing bentolaunch cannot reach itself. Goes back over the socket.
    Tab { connection: u64, tab_id: i64, window_id: i64 },
    /// Move the targeted window. The only target that acts on another tile
    /// rather than on itself, and the only one that leaves the panel up.
    Arrange(crate::shell::arrange::Move),
    /// Hold the panel open, so the next window clicked is picked as the thing
    /// to move rather than switched to.
    Stay,
    /// Ask a browser for a new tab. Goes back over the socket for the same
    /// reason focusing one does: only the browser can do it.
    NewTab { connection: u64 },
    /// An empty slot in the centre block.
    ///
    /// Drawn rather than left out, because the block's whole worth is that it
    /// is the same shape in the same place every summon. Taking it opens
    /// center mode, so the empty square says what it is for by doing it.
    Slot,
    /// Turn a mode on, or off if it is the one already on.
    ///
    /// A tile in the grid, so a mode is reachable by aiming at a square in a
    /// place that does not move - which the corner menu is not, since it has to
    /// be opened first.
    Mode(Mode),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// A live top-level window. Gets a capture preview in Milestone 3.
    Window,
    App,
    Folder,
    /// A URI target: settings pages, web links.
    Link,
    /// An open browser tab.
    Tab,
    /// A tile that does something to the panel or to another window, rather
    /// than being a thing to switch to.
    Action,
}

impl Kind {
    fn verb(self) -> &'static str {
        match self {
            Kind::Window => "focus window",
            Kind::App => "launch",
            Kind::Folder => "open folder",
            Kind::Link => "open",
            Kind::Tab => "switch to tab",
            Kind::Action => "run",
        }
    }
}

/// Stable across refreshes, so hover and selection survive the list changing.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ItemId {
    Window(isize),
    Shell(String),
    /// Scoped by connection: two browsers number their tabs independently.
    Tab(u64, i64),
    /// The action tiles. Their names are fixed, so the id is the name.
    Action(&'static str),
    /// An empty slot in the centre block, by its place in it. Numbered because
    /// several are on screen at once and hover has to tell them apart.
    Slot(usize),
}

#[derive(Debug, Clone)]
pub struct Item {
    pub id: ItemId,
    pub kind: Kind,
    pub title: String,
    /// Process name or path. Shown small under the title.
    pub detail: String,
    pub target: Target,
    /// Shell parsing name to source the icon from. `None` for windows whose
    /// process path could not be read.
    pub icon_source: Option<String>,
    /// The app this tile stands for, as a lowercased executable stem. What
    /// makes a pin and that app's running window answer to each other, which
    /// nothing else here can: they share no string otherwise. `None` for a tab,
    /// a bookmark, or an action.
    pub app: Option<String>,
    /// The window this tile's app already has open. Set while a section is
    /// assembled, by matching `app` against the live window list.
    ///
    /// A pin keeps its `target` and stays a pin: config, drag and unpin all
    /// still see the shell name. This only decides what taking the tile does
    /// (switch, not launch a second copy) and whether it draws a running mark.
    pub running: Option<Handle>,
    /// Which of a section's sources produced this tile. A merged section holds
    /// more than one, and what may be dragged, removed and written back to
    /// config is a property of the tile, not of the header above it.
    pub origin: crate::config::Source,
    /// The URL behind this tile, when there is one that outlives the tile.
    ///
    /// A tab is reached over the socket and its id means nothing once the
    /// browser has closed, so `target` cannot be written down. This can: it is
    /// what favoriting an open tab stores, and it is the same string a bookmark
    /// of the same page would store.
    pub link: Option<String>,
    /// Which group inside the section produced it: the index into that
    /// section's source list. Two groups can share a source — browser windows
    /// and everything else — so this, not `origin`, is what the tint and the
    /// drag runs key on. Set as the section is assembled.
    pub group: usize,
}

impl Item {
    /// The parsing name behind this tile. `None` for a live window, which is
    /// the one thing bentolaunch shows that config cannot name.
    pub fn shell_target(&self) -> Option<&str> {
        match &self.target {
            Target::Shell(name) => Some(name),
            Target::Window(_)
            | Target::Tab { .. }
            | Target::Arrange(_)
            | Target::Stay
            | Target::NewTab { .. }
            | Target::Slot
            | Target::Mode(_) => None,
        }
    }

    /// One line describing what activating this item does. Dry run logs it
    /// instead of acting on it.
    pub fn activation_summary(&self) -> String {
        match &self.target {
            Target::Window(h) => format!(
                "{} {:#x} \"{}\" ({})",
                self.kind.verb(),
                h.raw(),
                self.title,
                self.detail
            ),
            Target::Shell(name) => format!("{} \"{}\" -> {}", self.kind.verb(), self.title, name),
            Target::Tab { tab_id, .. } => {
                format!("{} {} \"{}\" ({})", self.kind.verb(), tab_id, self.title, self.detail)
            }
            Target::Arrange(mv) => format!("move the target window {}", mv.key()),
            Target::Stay => "hold the panel open".to_owned(),
            Target::NewTab { .. } => "open a new tab".to_owned(),
            Target::Slot => "fill an empty center slot".to_owned(),
            Target::Mode(mode) => format!("turn on {} mode", mode.label()),
        }
    }
}

/// A titled group of tiles. Sections are laid out stacked, each under its own
/// header, and their order comes from config.
#[derive(Debug, Clone)]
pub struct Section {
    pub title: String,
    pub items: Vec<Item>,
    /// Tint behind the box, straight off config.
    pub color: Option<String>,
    /// Ring round the box, and the colour of its title. `None` takes a colour
    /// off the theme's palette by `slot` instead.
    pub edge: Option<String>,
    /// Where this section sits in the config file, which is what the palette is
    /// dealt out by. Not its place in the grid: an empty section never reaches
    /// the panel, and a colour that shifted when a browser connected would be
    /// no use for finding a box by.
    pub slot: usize,
    /// How many items the section had before `max_items` cut it down. Edit
    /// mode needs it: "more tiles" has to stop at the number that exist.
    pub total: usize,
    /// Which half of the centre block this is, left to right. `None` for every
    /// ordinary section, which takes its place from the bento tree instead.
    pub center: Option<usize>,
    /// Tiles across. Only the centre sets it: an ordinary section's column
    /// count is in config, which the panel reads for itself.
    pub columns: usize,
}
