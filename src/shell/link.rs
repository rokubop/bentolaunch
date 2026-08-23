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
        true => shortcut_app(parsing_name).unwrap_or_else(|| parsing_name.to_owned()),
        false => parsing_name.to_owned(),
    };
    Path::new(&target)
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_lowercase())
}

/// The program a `.lnk` stands for, without going looking for it.
///
/// Three shapes, all of them on a taskbar:
///
/// * a path straight to the program;
/// * a stub launcher naming the program in its arguments. Squirrel pins as
///   `Update.exe --processStart Discord.exe`, so on path alone every app it
///   packages is "update";
/// * no path at all, meaning a shortcut to a shell folder. `File Explorer.lnk`
///   is the one every taskbar has. What opens a shell folder is what the
///   shortcut draws its icon from.
fn shortcut_app(lnk: &str) -> Option<String> {
    // SAFETY: COM is initialized on every thread that reaches here: the panel
    // thread apartment-threaded, the icon workers multi-threaded. Interfaces
    // are released by Drop, and each buffer outlives the call filling it.
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
        let path = link
            .GetPath(&mut buffer, std::ptr::null_mut(), SLGP_RAWPATH.0 as u32)
            .ok()
            .and_then(|()| read(&buffer));

        let mut buffer = [0u16; 1024];
        if let Some(launched) = link
            .GetArguments(&mut buffer)
            .ok()
            .and_then(|()| read(&buffer))
            .as_deref()
            .and_then(process_start)
        {
            return Some(launched);
        }

        if path.is_some() {
            return path;
        }

        // Only an executable answers this. A `.ico` or `.png` icon says nothing
        // about what runs it.
        let mut buffer = [0u16; 260];
        let mut index = 0i32;
        link.GetIconLocation(&mut buffer, &mut index)
            .ok()
            .and_then(|()| read(&buffer))
            .filter(|icon| {
                Path::new(icon)
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("exe") || e.eq_ignore_ascii_case("dll"))
            })
    }
}

/// A wide buffer up to its first NUL, or `None` when it is empty.
fn read(buffer: &[u16]) -> Option<String> {
    let end = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
    (end > 0).then(|| String::from_utf16_lossy(&buffer[..end]))
}

/// The program a stub launcher is asked to start.
///
/// Squirrel's flag, and the only argument worth reading: it names what to run
/// rather than describing how.
fn process_start(args: &str) -> Option<String> {
    let mut parts = args.split_whitespace();
    while let Some(part) = parts.next() {
        if part.eq_ignore_ascii_case("--processStart")
            || part.eq_ignore_ascii_case("--processStartAndWait")
        {
            let name = parts.next()?.trim_matches('"');
            return (!name.is_empty()).then(|| name.to_owned());
        }
    }
    None
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

    /// Without this every Squirrel-packaged app is "update" and they all match
    /// each other's windows.
    #[test]
    fn a_stub_launcher_is_read_for_what_it_starts() {
        assert_eq!(
            process_start("--processStart Discord.exe"),
            Some("Discord.exe".into())
        );
        assert_eq!(
            process_start(r#"--processStartAndWait "Teams.exe" --other"#),
            Some("Teams.exe".into())
        );
    }

    #[test]
    fn ordinary_arguments_name_nothing() {
        assert_eq!(process_start(""), None);
        assert_eq!(process_start("--new-window https://example.com"), None);
        // The flag with nothing after it is a truncated command line, not a name.
        assert_eq!(process_start("--processStart"), None);
    }

    #[test]
    fn a_buffer_is_read_up_to_its_nul() {
        assert_eq!(read(&[0x41, 0x42, 0, 0x43]), Some("AB".into()));
        assert_eq!(read(&[0, 0x41]), None);
        assert_eq!(read(&[]), None);
    }
}
