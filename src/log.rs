//! Minimal logger: appends to `%LOCALAPPDATA%\flick\flick.log` and mirrors to stderr.
//!
//! Milestone 1 is a dry run, so the log *is* the product — every action flick
//! would have taken gets recorded here instead of executed.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use windows::Win32::System::SystemInformation::GetLocalTime;

static SINK: OnceLock<Mutex<Option<std::fs::File>>> = OnceLock::new();

/// `%LOCALAPPDATA%\flick` — the only directory flick writes to besides its own
/// config file (safety rule 2).
pub fn cache_dir() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    let dir = PathBuf::from(base).join("flick");
    fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

pub fn init() {
    let file = cache_dir().and_then(|dir| {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("flick.log"))
            .ok()
    });
    let _ = SINK.set(Mutex::new(file));
}

fn stamp() -> String {
    // SAFETY: no arguments; returns by value.
    let t = unsafe { GetLocalTime() };
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    )
}

pub fn write(level: &str, msg: &str) {
    let line = format!("{} {:<5} {}", stamp(), level, msg);
    eprintln!("{line}");
    if let Some(sink) = SINK.get()
        && let Ok(mut guard) = sink.lock()
        && let Some(file) = guard.as_mut()
    {
        let _ = writeln!(file, "{line}");
        let _ = file.flush();
    }
}

#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => { $crate::log::write("INFO", &format!($($arg)*)) };
}

#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => { $crate::log::write("WARN", &format!($($arg)*)) };
}

#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => { $crate::log::write("ERROR", &format!($($arg)*)) };
}

/// Dry-run marker: "this is what flick *would* have done."
#[macro_export]
macro_rules! log_dry {
    ($($arg:tt)*) => { $crate::log::write("DRY", &format!($($arg)*)) };
}
