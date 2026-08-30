//! Right-click menus on the panel.
//!
//! Where managing a tile lives. This is the convention every comparable surface
//! follows — the taskbar's jump list, the bookmarks bar, Quick Access — so it is
//! the first place a user looks, and it costs the grid no pixels at rest.
//!
//! Section layout is the one thing behind a mode instead, and this is where it
//! is entered: it changes the shape of the panel rather than one tile's place
//! in it, so it needs the panel to stay up while it is being changed.

use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, MF_CHECKED, MF_SEPARATOR, MF_STRING,
    SetForegroundWindow, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu,
};
use windows::core::HSTRING;

use crate::log_warn;

pub const CMD_PIN_APP: usize = 200;
pub const CMD_UNPIN: usize = 201;
pub const CMD_OPEN_LOCATION: usize = 202;
pub const CMD_ADD_APP: usize = 203;
pub const CMD_ADD_FOLDER: usize = 204;
pub const CMD_ADD_FILE: usize = 205;
pub const CMD_SETTINGS: usize = 207;
pub const CMD_EDIT_LAYOUT: usize = 208;
/// Into and out of the centre block. The mode is the way this is meant to be
/// reached; these are the second path, the same way "Unpin" is.
pub const CMD_CENTER: usize = 209;
pub const CMD_UNCENTER: usize = 210;

pub struct Entry {
    pub id: usize,
    pub label: String,
    pub checked: bool,
}

impl Entry {
    pub fn new(id: usize, label: impl Into<String>) -> Entry {
        Entry { id, label: label.into(), checked: false }
    }
}

/// Show a popup at the cursor. `None` entries are separators. Returns the
/// chosen command.
pub fn show(owner: HWND, entries: &[Option<Entry>]) -> Option<usize> {
    if entries.is_empty() {
        return None;
    }

    // SAFETY: the menu is created and destroyed inside this call, and
    // TPM_RETURNCMD makes TrackPopupMenu modal and returns the selection rather
    // than posting a WM_COMMAND.
    unsafe {
        let mut point = POINT::default();
        let _ = GetCursorPos(&mut point);

        let Ok(menu) = CreatePopupMenu() else {
            log_warn!("could not create the context menu");
            return None;
        };

        for entry in entries {
            let appended = match entry {
                None => AppendMenuW(menu, MF_SEPARATOR, 0, None),
                Some(entry) => {
                    let flags = if entry.checked { MF_STRING | MF_CHECKED } else { MF_STRING };
                    let label = HSTRING::from(entry.label.as_str());
                    AppendMenuW(menu, flags, entry.id, &label)
                }
            };
            if let Err(e) = appended {
                log_warn!("could not build a menu item: {e}");
            }
        }

        // Without taking foreground first, the menu never dismisses when the
        // user clicks elsewhere. Long-standing Win32 requirement.
        let _ = SetForegroundWindow(owner);
        let chosen = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            point.x,
            point.y,
            Some(0),
            owner,
            None,
        );
        let _ = DestroyMenu(menu);

        (chosen.0 != 0).then_some(chosen.0 as usize)
    }
}
