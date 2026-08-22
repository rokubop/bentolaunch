//! Which app a tile stands for, as a lowercased executable stem.
//!
//! A pin and a running window are the same app but not the same string:
//! `Visual Studio Code.lnk` against `Code.exe`. Comparing the two file names
//! works for Discord and fails for VS Code, so the shortcut is read for the
//! path it actually points at.
//!
//! `SLGP_RAWPATH` and never `IShellLink::Resolve`: resolve hunts for a target
//! that moved, which can walk the network. This only reads the small local file
//! the shortcut already is, which is what keeps it off the wrong side of
//! safety rule 6.

use std::path::Path;

use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IPersistFile, STGM_READ};
use windows::Win32::UI::Shell::{IShellLinkW, SLGP_RAWPATH, ShellLink};
use windows::core::{HSTRING, Interface};

use crate::log_warn;

/// The app behind a shell parsing name. `None` for anything that is not a file
/// path: a URI names a page, not a program.
pub fn app_stem(parsing_name: &str) -> Option<String> {
    let path = Path::new(parsing_name);
    if !path.is_absolute() {
        return None;
    }
    let target = match path.extension().is_some_and(|e| e.eq_ignore_ascii_case("lnk")) {
        true => shortcut_target(parsing_name).unwrap_or_else(|| parsing_name.to_owned()),
        false => parsing_name.to_owned(),
    };
    Path::new(&target)
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_lowercase())
}

/// What a `.lnk` points at, without going looking for it.
fn shortcut_target(lnk: &str) -> Option<String> {
    // SAFETY: COM is initialized on every thread that reaches here — the panel
    // thread as apartment-threaded, the icon workers as multi-threaded. Every
    // interface is released by Drop, and the buffer outlives GetPath.
    unsafe {
        let link: IShellLinkW = match CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER) {
            Ok(link) => link,
            Err(e) => {
                log_warn!("could not read shortcuts: {e}");
                return None;
            }
        };
        let file: IPersistFile = link.cast().ok()?;
        file.Load(&HSTRING::from(lnk), STGM_READ).ok()?;

        let mut buffer = [0u16; 260];
        // Null find-data: the target's timestamps and size are of no interest
        // here, and asking for them is what would touch it.
        link.GetPath(&mut buffer, std::ptr::null_mut(), SLGP_RAWPATH.0 as u32)
            .ok()?;

        let end = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
        (end > 0).then(|| String::from_utf16_lossy(&buffer[..end]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_uri_names_no_app() {
        assert_eq!(app_stem("ms-settings:display"), None);
        assert_eq!(app_stem("https://example.com/chrome.exe"), None);
    }

    #[test]
    fn a_plain_path_is_its_own_stem() {
        assert_eq!(
            app_stem(r"C:\Program Files\Google\Chrome\Application\chrome.exe"),
            Some("chrome".into())
        );
    }

    #[test]
    fn case_never_decides_a_match() {
        assert_eq!(app_stem(r"C:\Windows\NOTEPAD.EXE"), Some("notepad".into()));
    }
}
