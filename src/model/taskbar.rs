//! Apps pinned to the taskbar.
//!
//! Pins are `.lnk` files in a per-user folder. `ShellExecuteW` launches one,
//! `IShellItemImageFactory` draws one. Read-only, safety rule 3.
//!
//! Two things that folder lacks: the left-to-right order, and apps pinned from
//! a running window (those get an *implicit* shortcut in a hashed sibling
//! folder). `HKCU\...\Explorer\Taskband\Favorites` has both, as serialised
//! PIDLs in taskbar order.
//!
//! Only its framing is guesswork: marker byte, u32 length, `ITEMIDLIST`.
//! `SHGetPathFromIDListW` decodes the rest. Self-checking, because a desynced
//! walk overruns the blob or hands the shell lists it refuses, never plausible
//! wrong paths. So a format change costs the order, not the grid.

use std::path::PathBuf;

use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_BINARY, RegGetValueW};
use windows::Win32::UI::Shell::SHGetPathFromIDListW;
use windows::core::w;

use crate::model::{Item, ItemId, Kind, Target};
use crate::{log_info, log_warn};

/// `%APPDATA%\Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar`
fn pin_dir() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(
        PathBuf::from(appdata)
            .join("Microsoft")
            .join("Internet Explorer")
            .join("Quick Launch")
            .join("User Pinned")
            .join("TaskBar"),
    )
}

/// Pins in the order the taskbar shows them.
///
/// Precedence: `order` (written by dragging) beats the registry beats
/// alphabetical. A hand-arranged order is a decision; the taskbar's is a
/// default.
pub fn pins_in_order(order: &[String]) -> Vec<Item> {
    let mut items = pins();
    let favorites = favorites();

    // Only the registry names implicit shortcuts, so anything here the folder
    // lacked is a pin that was being missed.
    for path in &favorites {
        let known = items
            .iter()
            .any(|item| item.shell_target().is_some_and(|t| t.eq_ignore_ascii_case(path)));
        if !known
            && let Some(item) = item_for(std::path::Path::new(path))
        {
            log_info!("taskbar pin only the registry names: {}", item.title);
            items.push(item);
        }
    }

    if !order.is_empty() {
        let rank: Vec<String> = order.iter().map(|name| name.to_lowercase()).collect();
        let position = |item: &Item| {
            rank.iter()
                .position(|name| *name == item.title.to_lowercase())
                .unwrap_or(usize::MAX)
        };
        // Stable, so the alphabetical order `pins` produced survives as the
        // tie-break for everything the list does not mention.
        items.sort_by_key(position);
        return items;
    }

    if !favorites.is_empty() {
        let position = |item: &Item| {
            item.shell_target()
                .and_then(|target| favorites.iter().position(|p| p.eq_ignore_ascii_case(target)))
                .unwrap_or(usize::MAX)
        };
        items.sort_by_key(position);
        log_info!(
            "taskbar order: {} pin(s) placed from the registry",
            favorites.len()
        );
    }

    items
}

pub fn pins() -> Vec<Item> {
    let Some(dir) = pin_dir() else {
        log_warn!("APPDATA is not set; cannot read taskbar pins");
        return Vec::new();
    };

    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) => {
            log_warn!("cannot read taskbar pins at {}: {e}", dir.display());
            return Vec::new();
        }
    };

    let mut items: Vec<Item> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("lnk"))
        })
        .filter_map(|path| item_for(&path))
        .collect();

    items.sort_by_key(|item| item.title.to_lowercase());
    log_info!("taskbar pins: {}", items.len());
    items
}

fn item_for(path: &std::path::Path) -> Option<Item> {
    let title = path.file_stem()?.to_string_lossy().into_owned();
    let name = path.to_string_lossy().into_owned();
    Some(Item {
        id: ItemId::Shell(name.clone()),
        kind: Kind::App,
        title,
        detail: "taskbar".into(),
        target: Target::Shell(name.clone()),
        app: crate::shell::link::app_stem(&name),
        icon_source: Some(name),
        origin: crate::config::Source::Taskbar,
        group: 0,
        link: None,
        running: None,
    })
}

/// The `.lnk` paths the taskbar holds, left to right. Empty means fall back.
fn favorites() -> Vec<String> {
    let Some(blob) = favorites_blob() else {
        return Vec::new();
    };

    let entries = entries(&blob);
    let paths: Vec<String> = entries.iter().filter_map(|pidl| path_of(pidl)).collect();

    // Not necessarily a broken parse: a pinned Store app has no file behind it.
    // Costs that pin its place, leaves the rest ordered.
    if paths.len() < entries.len() {
        log_warn!(
            "taskbar order: {} of {} registry entries did not resolve",
            entries.len() - paths.len(),
            entries.len()
        );
    }
    paths
}

/// Split the blob into serialised `ITEMIDLIST`s. Pure, so the guessed framing
/// is testable without a registry.
fn entries(blob: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut i = 0usize;
    // Marker byte, u32 length, then the list.
    while i + 5 < blob.len() {
        let len = u32::from_le_bytes([blob[i + 1], blob[i + 2], blob[i + 3], blob[i + 4]]) as usize;
        // A list is at minimum its own terminator. Anything overrunning the
        // blob is a desync, and the trailing byte ends the value the same way.
        if len < 2 || i + 5 + len > blob.len() {
            break;
        }
        out.push(&blob[i + 5..i + 5 + len]);
        i += 5 + len;
    }
    out
}

/// The filesystem path an `ITEMIDLIST` names. `None` if it names no file.
fn path_of(pidl: &[u8]) -> Option<String> {
    // Entries sit at odd offsets and `ITEMIDLIST` is a run of `u16`s. Copy
    // rather than point: shell32 is entitled to an aligned pointer.
    let mut aligned = vec![0u16; pidl.len().div_ceil(2) + 1];
    for (n, pair) in pidl.chunks(2).enumerate() {
        aligned[n] = u16::from_le_bytes([pair[0], pair.get(1).copied().unwrap_or(0)]);
    }

    let mut buffer = [0u16; 260];
    // SAFETY: `aligned` outlives the call and holds a copy of the list;
    // `buffer` is the 260 wide characters this API documents. A malformed list
    // is refused, which is what the return value is for.
    let ok = unsafe { SHGetPathFromIDListW(aligned.as_ptr().cast(), &mut buffer) };
    if !ok.as_bool() {
        return None;
    }
    let end = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
    (end > 0).then(|| String::from_utf16_lossy(&buffer[..end]))
}

/// `Taskband\Favorites`, raw. Two calls: size, then data.
fn favorites_blob() -> Option<Vec<u8>> {
    const KEY: windows::core::PCWSTR =
        w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Taskband");
    const VALUE: windows::core::PCWSTR = w!("Favorites");

    let mut size = 0u32;
    // SAFETY: sizing call. The null data pointer is what asks for one, and
    // `size` is all that is written.
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            KEY,
            VALUE,
            RRF_RT_REG_BINARY,
            None,
            None,
            Some(&mut size),
        )
    };
    if rc.is_err() || size == 0 {
        log_warn!("no taskbar order in the registry ({rc:?}); falling back to name order");
        return None;
    }

    let mut blob = vec![0u8; size as usize];
    // SAFETY: `blob` is the `size` bytes the call above asked for; `size` comes
    // back as the count actually written.
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            KEY,
            VALUE,
            RRF_RT_REG_BINARY,
            None,
            Some(blob.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if rc.is_err() {
        log_warn!("could not read the taskbar order ({rc:?}); falling back to name order");
        return None;
    }
    blob.truncate(size as usize);
    Some(blob)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One entry, framed the way the registry frames it.
    fn framed(list: &[u8]) -> Vec<u8> {
        let mut out = vec![0u8];
        out.extend_from_slice(&(list.len() as u32).to_le_bytes());
        out.extend_from_slice(list);
        out
    }

    #[test]
    fn entries_are_split_on_their_own_lengths() {
        let mut blob = framed(&[1, 2, 3, 4]);
        blob.extend(framed(&[9, 9]));
        blob.push(0); // the trailing byte that closes the value

        assert_eq!(entries(&blob), vec![&[1u8, 2, 3, 4][..], &[9u8, 9][..]]);
    }

    /// A blob stopping mid-entry gives back what was whole. Never hands the
    /// shell a truncated list.
    #[test]
    fn an_entry_running_past_the_end_ends_the_walk() {
        let mut blob = framed(&[1, 2, 3, 4]);
        blob.extend_from_slice(&[0, 0xff, 0xff, 0, 0, 7, 7]);

        assert_eq!(entries(&blob), vec![&[1u8, 2, 3, 4][..]]);
    }

    /// A desynced walk reads its length from the middle of a PIDL. Stop, do
    /// not resynchronise onto plausible garbage.
    #[test]
    fn a_nonsense_length_ends_the_walk_rather_than_guessing() {
        assert!(entries(&[0, 1, 0, 0, 0, 0xab]).is_empty());
    }

    #[test]
    fn an_empty_value_yields_nothing() {
        assert!(entries(&[]).is_empty());
    }
}
