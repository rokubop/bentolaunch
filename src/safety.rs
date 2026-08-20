//! The failure mode that feels like a broken PC: an invisible topmost window
//! sitting over the desktop swallowing every click. This module makes that
//! recoverable without the user knowing bentopick exists.
//!
//! The escape hatch is `SetWindowLongPtrW(GWL_EXSTYLE, ... | WS_EX_TRANSPARENT)`.
//! That writes the window struct directly and does **not** require the owning
//! thread to pump messages, so it still works when the UI thread is wedged —
//! unlike `ShowWindow`/`SetWindowPos`, which marshal to that thread and would
//! hang right along with it. Clicks fall through to whatever is underneath and
//! the machine is usable again.

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};

use windows::Win32::Foundation::HWND;
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetWindowLongPtrW, SW_HIDE, SetWindowLongPtrW, ShowWindowAsync,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TRANSPARENT,
};

use crate::{log_error, log_warn};

static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);
static HEARTBEAT: AtomicU64 = AtomicU64::new(0);
static SHOWN: AtomicBool = AtomicBool::new(false);
static NEUTRALIZED: AtomicBool = AtomicBool::new(false);

/// How long the UI thread may go without pumping, while the panel is visible,
/// before we assume it is wedged. The panel runs a timer while shown, so a
/// healthy pump beats several times a second.
const STALL_LIMIT_MS: u64 = 4_000;

pub fn register_window(hwnd: HWND) {
    MAIN_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
    beat();
}

fn window() -> Option<HWND> {
    match MAIN_HWND.load(Ordering::SeqCst) {
        0 => None,
        h => Some(HWND(h as *mut core::ffi::c_void)),
    }
}

/// Called from the message loop. Proof of life.
pub fn beat() {
    // SAFETY: no arguments, no out-params.
    HEARTBEAT.store(unsafe { GetTickCount64() }, Ordering::SeqCst);
}

/// The panel tracks its own visibility here rather than asking the OS, because
/// `IsWindowVisible` on a wedged window is exactly the call we cannot trust.
pub fn mark_shown(shown: bool) {
    SHOWN.store(shown, Ordering::SeqCst);
    if shown {
        beat();
    }
}

pub fn is_neutralized() -> bool {
    NEUTRALIZED.load(Ordering::SeqCst)
}

/// Make the window harmless: click-through, non-activating, and hidden if the
/// owning thread is still alive enough to process the request.
pub fn neutralize(reason: &str) {
    let Some(hwnd) = window() else { return };
    if NEUTRALIZED.swap(true, Ordering::SeqCst) {
        return;
    }
    log_error!("neutralizing bentopick window: {reason}");

    // SAFETY: hwnd came from CreateWindowExW and is only cleared on shutdown.
    // Both calls are safe on another thread's window; neither blocks on it.
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let harmless = ex | WS_EX_TRANSPARENT.0 | WS_EX_NOACTIVATE.0 | WS_EX_LAYERED.0;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, harmless as isize);
        let _ = ShowWindowAsync(hwnd, SW_HIDE);
    }
}

/// Any panic, on any thread, neutralizes the window before unwinding further.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Log first: neutralize touches the window, which is the riskier half.
        log_error!("panic: {info}");
        neutralize("panic");
        previous(info);
    }));
}

/// Watches for a wedged UI thread. Cheap: wakes once a second, and only looks at
/// an atomic unless the panel is actually up.
pub fn start_watchdog() {
    std::thread::Builder::new()
        .name("bentopick-watchdog".into())
        .spawn(|| {
            loop {
                std::thread::sleep(std::time::Duration::from_millis(1000));
                if !SHOWN.load(Ordering::SeqCst) || NEUTRALIZED.load(Ordering::SeqCst) {
                    continue;
                }
                // SAFETY: no arguments, no out-params.
                let now = unsafe { GetTickCount64() };
                let last = HEARTBEAT.load(Ordering::SeqCst);
                let stalled = now.saturating_sub(last);
                if stalled > STALL_LIMIT_MS {
                    log_warn!("UI thread has not pumped for {stalled}ms");
                    neutralize("UI thread stalled");
                }
            }
        })
        .map(|_| ())
        .unwrap_or_else(|e| log_error!("could not start watchdog thread: {e}"));
}
