//! What bentopick knows about, and how it stays current.

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

/// What activating a tile does.
///
/// Everything that is not a live window collapses to a **shell parsing name** —
/// the string form the shell already understands. A file path, a folder, a
/// `.lnk`, `shell:AppsFolder\<AppUserModelID>` for a Store app, and a URI like
/// `ms-settings:display` are all parsing names, and all of them both launch
/// through `ShellExecuteW` and produce an icon through `IShellItemImageFactory`.
/// One string covers every non-window thing bentopick can show.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Target {
    /// Focus this window.
    Window(Handle),
    /// Hand this to the shell.
    Shell(String),
    /// The one thing bentopick cannot reach itself. Goes back over the socket.
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
    /// Which of a section's sources produced this tile. A merged section holds
    /// more than one, and what may be dragged, removed and written back to
    /// config is a property of the tile, not of the header above it.
    pub origin: crate::config::Source,
    /// Which group inside the section produced it: the index into that
    /// section's source list. Two groups can share a source — browser windows
    /// and everything else — so this, not `origin`, is what the tint and the
    /// drag runs key on. Set as the section is assembled.
    pub group: usize,
}

impl Item {
    /// The parsing name behind this tile. `None` for a live window, which is
    /// the one thing bentopick shows that config cannot name.
    pub fn shell_target(&self) -> Option<&str> {
        match &self.target {
            Target::Shell(name) => Some(name),
            Target::Window(_)
            | Target::Tab { .. }
            | Target::Arrange(_)
            | Target::Stay
            | Target::NewTab { .. } => None,
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
    /// How many items the section had before `max_items` cut it down. Edit
    /// mode needs it: "more tiles" has to stop at the number that exist.
    pub total: usize,
}
