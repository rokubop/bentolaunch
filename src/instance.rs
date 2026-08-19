//! One instance per session.
//!
//! A second copy has nowhere to put itself: the hotkey is already registered, so
//! it starts deaf, and its tray icon sits next to the first one. Launching
//! dashpick again is not a request for a second copy, it is a request to see the
//! panel, so that is what the second copy hands over before exiting.

use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, FindWindowW, GetWindowThreadProcessId, PostMessageW, WM_APP,
};
use windows::core::w;

use crate::ui::panel::CLASS_NAME;
use crate::{log_info, log_warn};

/// Posted across processes by a second launch. Carries nothing: the running
/// instance already knows how to show itself.
pub const WM_SUMMON: u32 = WM_APP + 8;

/// `Local\` scopes it to this session, so a second user gets their own dashpick.
const MUTEX_NAME: windows::core::PCWSTR = w!("Local\\dashpick_single_instance");

/// Held for the life of the process. Windows releases it on exit either way;
/// the handle is closed here so a leak shows up as a compile error rather than
/// as a second instance being refused after a crash.
pub struct Claim(HANDLE);

impl Drop for Claim {
    fn drop(&mut self) {
        // SAFETY: our own handle, closed once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// `None` means another instance is already running and has been asked to show
/// itself; this process should exit.
pub fn claim() -> Option<Claim> {
    // SAFETY: a named mutex with default security. `true` asks for initial
    // ownership, which we never rely on — the handle existing is the claim.
    let handle = unsafe { CreateMutexW(None, true, MUTEX_NAME) };
    let handle = match handle {
        Ok(handle) => handle,
        Err(e) => {
            // Better a possible second instance than no instance at all.
            log_warn!("could not claim the single-instance mutex ({e}); starting anyway");
            return Some(Claim(HANDLE(std::ptr::null_mut())));
        }
    };

    // The mutex is created either way; whether it already existed is the whole
    // signal, and it only survives until the next call that sets it.
    // SAFETY: reads this thread's last error, set by the call above.
    if unsafe { GetLastError() } != ERROR_ALREADY_EXISTS {
        return Some(Claim(handle));
    }

    summon();
    // SAFETY: our own handle, and this process is about to end.
    unsafe {
        let _ = CloseHandle(handle);
    }
    None
}

/// Ask the instance that is already running to show its panel.
fn summon() {
    // SAFETY: a class lookup and a post; both tolerate a window that has gone.
    unsafe {
        let Ok(hwnd) = FindWindowW(CLASS_NAME, None) else {
            // The other instance is still starting and has no window yet. It
            // will come up on its own, so there is nothing to hand over.
            log_info!("already running; exiting without summoning");
            return;
        };
        // Foreground rights belong to this process: it is the one that was just
        // launched. Lend them over or the panel comes up behind whatever has
        // focus.
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid != 0 {
            let _ = AllowSetForegroundWindow(pid);
        }
        let _ = PostMessageW(Some(hwnd), WM_SUMMON, Default::default(), Default::default());
        log_info!("already running; handed the summon to it and exiting");
    }
}
