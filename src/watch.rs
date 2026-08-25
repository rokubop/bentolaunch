//! Config file watcher.
//!
//! Polls the modification time rather than using `ReadDirectoryChangesW`. One
//! file, a one second granularity, and no directory handle held open for the
//! life of the process. Editors that save by rename (most of them) change the
//! mtime just the same.

use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::SystemTime;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use crate::config::Config;
use crate::log_info;

/// Posted to the panel when `bentolaunch.toml` changed on disk.
pub const WM_CONFIG_RELOAD: u32 = WM_APP + 4;

static NOTIFY: AtomicIsize = AtomicIsize::new(0);

const POLL_MS: u64 = 700;

pub fn start(notify: HWND) {
    let Some(path) = Config::path() else { return };
    NOTIFY.store(notify.0 as isize, Ordering::SeqCst);

    let watched = path.clone();
    let spawned = std::thread::Builder::new()
        .name("bentolaunch-config-watch".into())
        .spawn(move || {
            let path = watched;
            let mut seen = modified(&path);
            loop {
                std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
                let now = modified(&path);
                if now == seen {
                    continue;
                }
                seen = now;

                let notify = NOTIFY.load(Ordering::SeqCst);
                if notify == 0 {
                    continue;
                }
                // SAFETY: an asynchronous post; harmless if the window is gone.
                unsafe {
                    let _ = PostMessageW(
                        Some(HWND(notify as *mut core::ffi::c_void)),
                        WM_CONFIG_RELOAD,
                        Default::default(),
                        Default::default(),
                    );
                }
            }
        });

    match spawned {
        Ok(_) => log_info!("watching {} for changes", path.display()),
        Err(e) => crate::log::write("WARN", &format!("could not watch the config file: {e}")),
    }
}

fn modified(path: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}
