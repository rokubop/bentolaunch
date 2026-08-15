//! What flick knows about, and how it stays current.

pub mod store;
pub mod windows;

use std::path::PathBuf;

use ::windows::Win32::Foundation::HWND;

/// A window handle stored as its raw value.
///
/// Window handles are process-wide and valid from any thread; the pointer inside
/// `HWND` is the only reason windows-rs marks it `!Send`. Storing the raw value
/// keeps the item store `Send`, which matters once icon and capture work moves
/// to the worker threads that safety rule 5 requires.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// A live top-level window. Gets a capture preview in Milestone 3.
    Window,
    /// A pinned executable or shortcut. Icon only.
    App,
    /// A pinned Explorer folder. Icon only.
    Folder,
}

/// Stable across refreshes, so hover and selection survive the window list
/// changing underneath them.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ItemId {
    Window(isize),
    Path(PathBuf),
}

#[derive(Debug, Clone)]
pub struct Item {
    pub id: ItemId,
    pub kind: Kind,
    /// Window title, or the app/folder name.
    pub title: String,
    /// Process name or path. Shown small, and used to pick an icon.
    pub detail: String,
    pub handle: Option<Handle>,
    /// File to source the icon from: the exe for a window, the target for a pin.
    pub icon_source: Option<PathBuf>,
}

impl Item {
    /// One line describing what activating this item would do. Milestone 1
    /// writes this to the log instead of acting on it.
    pub fn activation_summary(&self) -> String {
        match self.kind {
            Kind::Window => format!(
                "focus window {:#x} \"{}\" ({})",
                self.handle.map(Handle::raw).unwrap_or(0),
                self.title,
                self.detail
            ),
            Kind::App => format!("launch app \"{}\" ({})", self.title, self.detail),
            Kind::Folder => format!("open folder \"{}\" ({})", self.title, self.detail),
        }
    }
}
