//! What flick knows about, and how it stays current.

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
/// One string covers every non-window thing flick can show.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Target {
    /// Focus this window.
    Window(Handle),
    /// Hand this to the shell.
    Shell(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// A live top-level window. Gets a capture preview in Milestone 3.
    Window,
    App,
    Folder,
    /// A URI target: settings pages, web links.
    Link,
}

impl Kind {
    fn verb(self) -> &'static str {
        match self {
            Kind::Window => "focus window",
            Kind::App => "launch",
            Kind::Folder => "open folder",
            Kind::Link => "open",
        }
    }
}

/// Stable across refreshes, so hover and selection survive the list changing.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ItemId {
    Window(isize),
    Shell(String),
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
}

impl Item {
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
        }
    }
}

/// A titled group of tiles. Sections are laid out stacked, each under its own
/// header, and their order comes from config.
#[derive(Debug, Clone)]
pub struct Section {
    pub title: String,
    pub items: Vec<Item>,
}
