//! Tray icon. flick has no taskbar presence and no window of its own most of the
//! time, so this is the only affordance proving it is running — and the only way
//! to quit it without the Task Manager.

use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, IDI_APPLICATION, LoadIconW,
    MF_SEPARATOR, MF_STRING, SetForegroundWindow, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu,
    WM_APP, WM_LBUTTONUP, WM_RBUTTONUP,
};
use windows::core::w;

use crate::{log_info, log_warn};

/// Tray callbacks arrive as this message, with the mouse event in lparam.
pub const WM_TRAY: u32 = WM_APP + 2;

pub const CMD_TOGGLE: usize = 100;
pub const CMD_EXIT: usize = 101;

const ICON_ID: u32 = 1;

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
    // SAFETY: a stock system icon; no resource of ours to keep alive.
    data.hIcon = unsafe { LoadIconW(None, IDI_APPLICATION).unwrap_or_default() };

    let tip = "flick — dry run";
    for (slot, unit) in data.szTip.iter_mut().zip(tip.encode_utf16()) {
        *slot = unit;
    }

    // SAFETY: data is fully initialized and lives across the call.
    if unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool() {
        log_info!("tray icon installed");
    } else {
        log_warn!("could not install the tray icon; flick is running with no visible affordance");
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
        let _ = AppendMenuW(menu, MF_STRING, CMD_TOGGLE, w!("Show flick"));
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
