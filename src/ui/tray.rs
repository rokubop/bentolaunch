//! Tray icon. bentopick has no taskbar presence and no window of its own most of the
//! time, so this is the only affordance proving it is running — and the only way
//! to quit it without the Task Manager.

use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, HICON, IDI_APPLICATION, LoadIconW,
    MF_SEPARATOR, MF_STRING, SetForegroundWindow, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu,
    WM_APP, WM_LBUTTONUP, WM_RBUTTONUP,
};
use windows::core::{PCWSTR, w};

use crate::{log_info, log_warn};

/// Tray callbacks arrive as this message, with the mouse event in lparam.
pub const WM_TRAY: u32 = WM_APP + 2;

pub const CMD_TOGGLE: usize = 100;
pub const CMD_EXIT: usize = 101;
pub const CMD_ADD_APP: usize = 102;
pub const CMD_ADD_FOLDER: usize = 103;
pub const CMD_ADD_FILE: usize = 104;
pub const CMD_EDIT_CONFIG: usize = 105;

const ICON_ID: u32 = 1;

/// Resource id the build script gives the embedded `.ico`. `winresource`
/// numbers the first icon 1.
///
/// This is `MAKEINTRESOURCE`: a resource id travels in the pointer itself, so
/// the address is an integer and is never dereferenced.
fn app_icon_id() -> PCWSTR {
    PCWSTR(std::ptr::without_provenance(1))
}

/// The embedded icon, or the stock one if it is missing. Missing happens on a
/// machine that built without rc.exe, and a stock icon beats no tray at all.
fn app_icon() -> HICON {
    // SAFETY: the module handle is this exe; a missing resource returns an
    // error rather than misbehaving.
    unsafe {
        let instance = GetModuleHandleW(None).ok();
        instance
            .and_then(|module| LoadIconW(Some(module.into()), app_icon_id()).ok())
            .or_else(|| LoadIconW(None, IDI_APPLICATION).ok())
            .unwrap_or_default()
    }
}

fn icon_data(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: ICON_ID,
        ..Default::default()
    }
}

pub fn install(hwnd: HWND) {
    let mut data = icon_data(hwnd);
    data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    data.uCallbackMessage = WM_TRAY;
    data.hIcon = app_icon();

    let tip = "BentoPick";
    for (slot, unit) in data.szTip.iter_mut().zip(tip.encode_utf16()) {
        *slot = unit;
    }

    // SAFETY: data is fully initialized and lives across the call.
    if unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool() {
        log_info!("tray icon installed");
    } else {
        log_warn!("could not install the tray icon; bentopick is running with no visible affordance");
    }
}

pub fn remove(hwnd: HWND) {
    let data = icon_data(hwnd);
    // SAFETY: removing an icon that was never added fails harmlessly.
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &data);
    }
}

/// Returns the chosen command, if any.
pub fn show_menu(hwnd: HWND) -> Option<usize> {
    // SAFETY: menu lifetime is bounded by this function; TrackPopupMenu is
    // modal and returns the selection because of TPM_RETURNCMD.
    unsafe {
        let mut point = POINT::default();
        let _ = GetCursorPos(&mut point);

        let menu = CreatePopupMenu().ok()?;
        let _ = AppendMenuW(menu, MF_STRING, CMD_TOGGLE, w!("Show BentoPick"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, CMD_ADD_APP, w!("Add app..."));
        let _ = AppendMenuW(menu, MF_STRING, CMD_ADD_FOLDER, w!("Add folder..."));
        let _ = AppendMenuW(menu, MF_STRING, CMD_ADD_FILE, w!("Add file or shortcut..."));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, CMD_EDIT_CONFIG, w!("Edit settings..."));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, CMD_EXIT, w!("Exit"));

        // Long-standing requirement: without taking foreground first, the menu
        // never dismisses when the user clicks elsewhere.
        let _ = SetForegroundWindow(hwnd);
        let chosen = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            point.x,
            point.y,
            Some(0),
            hwnd,
            None,
        );
        let _ = DestroyMenu(menu);

        (chosen.0 != 0).then_some(chosen.0 as usize)
    }
}

/// Which mouse event a `WM_TRAY` message carried.
pub enum Click {
    Left,
    Right,
    Other,
}

pub fn classify(_wparam: WPARAM, lparam: LPARAM) -> Click {
    match lparam.0 as u32 {
        WM_LBUTTONUP => Click::Left,
        WM_RBUTTONUP => Click::Right,
        _ => Click::Other,
    }
}
