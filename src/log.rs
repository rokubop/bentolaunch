//! Minimal logger: appends to `%LOCALAPPDATA%\bentolaunch\bentolaunch.log` and mirrors to stderr.
//!
//! Milestone 1 is a dry run, so the log *is* the product — every action bentolaunch
//! would have taken gets recorded here instead of executed.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use windows::Win32::System::SystemInformation::GetLocalTime;

static SINK: OnceLock<Mutex<Option<std::fs::File>>> = OnceLock::new();

/// `%LOCALAPPDATA%\bentolaunch` — the only directory bentolaunch writes to besides its own
/// config file (safety rule 2).
pub fn cache_dir() -> Option<PathBuf> {
    cache_dir_in(&PathBuf::from(std::env::var_os("LOCALAPPDATA")?))
}

/// Renamed from BentoPick. The move carries `peers.json` across, so a browser
/// paired before the rename stays paired. A failed move keeps the old
/// directory: it is the one with the pairing in it.
fn cache_dir_in(base: &Path) -> Option<PathBuf> {
    let dir = base.join("bentolaunch");
    if !dir.exists() {
        let legacy = base.join("bentopick");
        if legacy.is_dir() {
            if fs::rename(&legacy, &dir).is_err() {
                return Some(legacy);
            }
            let _ = fs::rename(dir.join("bentopick.log"), dir.join("bentolaunch.log"));
        }
    }
    fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

pub fn init() {
    let file = cache_dir().and_then(|dir| {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("bentolaunch.log"))
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

/// Dry-run marker: "this is what bentolaunch *would* have done."
#[macro_export]
macro_rules! log_dry {
    ($($arg:tt)*) => { $crate::log::write("DRY", &format!($($arg)*)) };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("bentolaunch-test-cache-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_bentopick_cache_is_carried_over_so_paired_browsers_survive() {
        let base = scratch("carried");
        let legacy = base.join("bentopick");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("peers.json"), r#"{"peers":[]}"#).unwrap();
        fs::write(legacy.join("bentopick.log"), "old line\n").unwrap();

        let dir = cache_dir_in(&base).unwrap();

        assert_eq!(dir, base.join("bentolaunch"));
        assert_eq!(
            fs::read_to_string(dir.join("peers.json")).unwrap(),
            r#"{"peers":[]}"#
        );
        assert!(dir.join("bentolaunch.log").is_file(), "log renamed too");
        assert!(!legacy.exists(), "the old directory is moved, not copied");
    }

    #[test]
    fn an_existing_bentolaunch_cache_is_left_alone() {
        let base = scratch("existing");
        let current = base.join("bentolaunch");
        fs::create_dir_all(&current).unwrap();
        fs::write(current.join("peers.json"), "current").unwrap();
        let legacy = base.join("bentopick");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("peers.json"), "stale").unwrap();

        let dir = cache_dir_in(&base).unwrap();

        assert_eq!(fs::read_to_string(dir.join("peers.json")).unwrap(), "current");
        assert!(legacy.exists(), "nothing is deleted behind the user's back");
    }

    #[test]
    fn a_fresh_install_just_gets_the_directory() {
        let base = scratch("fresh");
        let dir = cache_dir_in(&base).unwrap();
        assert_eq!(dir, base.join("bentolaunch"));
        assert!(dir.is_dir());
    }
}
