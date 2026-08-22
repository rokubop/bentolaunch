//! Apps pinned to the taskbar.
//!
//! Windows keeps these as `.lnk` files in a fixed per-user folder. Reading that
//! folder is enough: `ShellExecuteW` on a `.lnk` launches its target, and
//! `IShellItemImageFactory` on a `.lnk` returns the target's icon, so bentopick
//! never has to resolve the shortcut itself. Read-only, per safety rule 3.
//!
//! What this does **not** recover is the taskbar's left-to-right order. That
//! lives in `HKCU\...\Explorer\Taskband\Favorites` as an undocumented binary
//! blob of serialised PIDLs. Parsing it would be guesswork against a format
//! Microsoft can change silently, so entries are sorted by name instead. For an
//! exact order, list the apps in a manual section.

use std::path::PathBuf;

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

/// Pins in the order `order` names them, by pin title, with anything unlisted
/// following in name order.
///
/// The taskbar's own left-to-right order is not readable (see above), so an
/// explicit list is the only way to get an exact one. Dragging a taskbar tile is
/// what writes that list.
pub fn pins_in_order(order: &[String]) -> Vec<Item> {
    let mut items = pins();
    if order.is_empty() {
        return items;
    }
    let rank: Vec<String> = order.iter().map(|name| name.to_lowercase()).collect();
    let position = |item: &Item| {
        rank.iter()
            .position(|name| *name == item.title.to_lowercase())
            .unwrap_or(usize::MAX)
    };
    // Stable, so the alphabetical order `pins` produced survives as the
    // tie-break for everything the list does not mention.
    items.sort_by_key(position);
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
    })
}
