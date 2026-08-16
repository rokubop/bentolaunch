//! Milestone 2: actually switching to things.
//!
//! Two cases only, because `Target` has two variants. Windows get focused;
//! everything else goes to `ShellExecuteW`, which already knows how to open a
//! file, a folder, a `.lnk`, a Store app by AppUserModelID, and a URI.
//!
//! The `open` verb is used exclusively. `runas` would prompt for elevation and
//! is never appropriate here (safety rule 1).

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, IsIconic, IsWindow, SW_RESTORE, SW_SHOWNORMAL, SetForegroundWindow,
    ShowWindow,
    SW_SHOW,
};
use windows::core::HSTRING;

use crate::model::{Item, Target};
use crate::{log_info, log_warn};

/// `ShellExecuteW` returns a fake HINSTANCE; anything at or below 32 is an error
/// code rather than a handle.
const SHELL_EXECUTE_ERROR_MAX: isize = 32;

pub fn activate(item: &Item) {
    match &item.target {
        Target::Window(handle) => focus(handle.hwnd(), &item.title),
        Target::Shell(name) => launch(name, &item.title),
        Target::Tab { connection, tab_id, window_id } => {
            switch_to_tab(*connection, *tab_id, *window_id, &item.title)
        }
    }
}

/// Hand the foreground right over and let the browser raise itself.
///
/// flick cannot map a browser `windowId` onto an HWND. The browser can.
fn switch_to_tab(connection: u64, tab_id: i64, window_id: i64, title: &str) {
    // Named browser pids, not ASFW_ANY: that would let anything steal
    // foreground for the same window.
    for pid in crate::model::store::browser_pids() {
        // SAFETY: a stale pid fails harmlessly. This grants a right, it never
        // takes one away.
        unsafe {
            let _ = AllowSetForegroundWindow(pid);
        }
    }

    if crate::browser::server::focus(connection, tab_id, window_id) {
        log_info!("asked the browser to switch to \"{title}\"");
    } else {
        log_warn!("could not reach the browser to switch to \"{title}\"");
    }
}

/// Bring a window forward.
///
/// `SetForegroundWindow` is normally restricted, but the process that received
/// the last input event is allowed to call it — and `RegisterHotKey` delivers
/// `WM_HOTKEY` as exactly that. So summoning flick by its hotkey grants the
/// right that activation needs. See DESIGN.md "Focus model".
fn focus(hwnd: HWND, title: &str) {
    // SAFETY: the window may have closed between enumeration and this click, in
    // which case IsWindow says so and the rest is skipped.
    unsafe {
        if !IsWindow(Some(hwnd)).as_bool() {
            log_warn!("window \"{title}\" closed before it could be focused");
            return;
        }

        // A minimized window will not come forward until it is restored.
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        } else {
            let _ = ShowWindow(hwnd, SW_SHOW);
        }

        if SetForegroundWindow(hwnd).as_bool() {
            log_info!("focused \"{title}\"");
        } else {
            // Windows declined the foreground change. Nothing is broken; the
            // window is restored and flashing in the taskbar.
            log_warn!("the OS declined to foreground \"{title}\"");
        }
    }
}

fn launch(parsing_name: &str, title: &str) {
    let target = HSTRING::from(parsing_name);
    // SAFETY: both strings outlive the call. `open` never elevates.
    let result = unsafe {
        ShellExecuteW(
            None,
            windows::core::w!("open"),
            &target,
            None,
            None,
            SW_SHOWNORMAL,
        )
    };

    if result.0 as isize > SHELL_EXECUTE_ERROR_MAX {
        log_info!("launched \"{title}\" ({parsing_name})");
    } else {
        log_warn!(
            "could not launch \"{title}\" ({parsing_name}): ShellExecute code {}",
            result.0 as isize
        );
    }
}
