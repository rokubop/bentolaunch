//! The live item list, kept current by `SetWinEventHook` so that showing the
//! panel is a read, never a scan.
//!
//! Windows are held in MRU order: the foreground hook moves the newly focused
//! window to the front, which is the order a switcher wants. Taskbar pins and
//! manual entries are resolved once at startup, because neither changes without
//! a restart and both touch the disk.

use std::path::Path;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Mutex, OnceLock};

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::WindowsAndMessaging::{
    CHILDID_SELF, EVENT_OBJECT_CLOAKED, EVENT_OBJECT_CREATE, EVENT_OBJECT_DESTROY,
    EVENT_OBJECT_HIDE, EVENT_OBJECT_NAMECHANGE, EVENT_OBJECT_UNCLOAKED, EVENT_SYSTEM_FOREGROUND,
    OBJID_WINDOW, PostMessageW, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_APP,
};

use crate::config::{ManualItem, SectionConfig, Source};
// Imported by name rather than as a module: `windows` would otherwise shadow the
// `windows` crate throughout this file.
use crate::model::taskbar;
use crate::model::windows::{WindowInfo, enumerate, refresh_title, still_switchable};
use crate::model::{Handle, Item, ItemId, Kind, Section, Target};
use crate::{log_info, log_warn};

/// Posted to the panel when the item list changed. Only acted on while visible.
pub const WM_MODEL_CHANGED: u32 = WM_APP + 1;

/// One configured section, with whatever could be resolved up front.
struct Group {
    title: String,
    source: Source,
    /// Lowercased process names this section claims. Empty means catch-all.
    matches: Vec<String>,
    /// Pre-resolved for taskbar and manual sources; empty for windows.
    fixed: Vec<Item>,
}

impl Group {
    fn claims(&self, window: &WindowInfo) -> bool {
        if self.matches.is_empty() {
            return true;
        }
        let Some(exe) = window.exe.as_ref().and_then(|p| p.file_name()) else {
            return false;
        };
        let exe = exe.to_string_lossy().to_lowercase();
        self.matches.contains(&exe)
    }
}

struct Store {
    windows: Vec<WindowInfo>,
    groups: Vec<Group>,
    /// flick's own panel, which must never appear in its own grid.
    exclude: Handle,
}

static STORE: OnceLock<Mutex<Store>> = OnceLock::new();
static HOOKS: Mutex<Vec<isize>> = Mutex::new(Vec::new());
static NOTIFY: AtomicIsize = AtomicIsize::new(0);

fn store() -> &'static Mutex<Store> {
    STORE.get_or_init(|| {
        Mutex::new(Store {
            windows: Vec::new(),
            groups: Vec::new(),
            exclude: Handle::new(HWND(std::ptr::null_mut())),
        })
    })
}

/// Resolve the parts of a section that do not change without a config edit:
/// taskbar pins and manual entries both touch the disk, so they are read once.
fn build_groups(sections: &[SectionConfig]) -> Vec<Group> {
    sections
        .iter()
        .map(|section| Group {
            title: section.title.clone(),
            source: section.source,
            matches: section.matches.iter().map(|m| m.to_lowercase()).collect(),
            fixed: match section.source {
                Source::Taskbar => taskbar::pins_in_order(&section.order),
                Source::Manual => section.items.iter().filter_map(manual_item).collect(),
                Source::Windows | Source::Tabs => Vec::new(),
            },
        })
        .collect()
}

/// Rebuild sections after a config edit. Windows are left alone: the hooks have
/// been keeping that list current and it does not depend on config.
pub fn reconfigure(sections: &[SectionConfig]) {
    let groups = build_groups(sections);
    if let Ok(mut s) = store().lock() {
        s.groups = groups;
    }
}

/// First and only full enumeration. Everything after this is incremental.
pub fn init(exclude: HWND, sections: &[SectionConfig]) {
    let groups = build_groups(sections);
    let found = enumerate(exclude);
    log_info!("initial scan: {} windows", found.len());
    for (n, w) in found.iter().enumerate() {
        log_info!(
            "  [{n:>2}] {:<48} {} [{}]",
            truncate(&w.title, 48),
            w.exe
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "?".into()),
            w.class
        );
    }
    for group in &groups {
        log_info!(
            "section \"{}\" ({:?}): {} fixed item(s)",
            group.title,
            group.source,
            group.fixed.len()
        );
    }

    if let Ok(mut s) = store().lock() {
        s.exclude = Handle::new(exclude);
        s.windows = found;
        s.groups = groups;
    }
}

/// Build a tile from a manual config entry.
///
/// Everything here is a shell parsing name, so nothing needs to exist on disk —
/// `ms-settings:display` is as valid a target as `R:\dev`. Existence only
/// affects which title and icon flick can infer.
fn manual_item(entry: &ManualItem) -> Option<Item> {
    let target = entry.target().trim();
    if target.is_empty() {
        return None;
    }
    let kind = derive_kind(target);
    let title = entry
        .title()
        .map(str::to_owned)
        .unwrap_or_else(|| derive_title(target));

    Some(Item {
        id: ItemId::Shell(target.to_owned()),
        kind,
        title,
        detail: shorten_detail(target),
        target: Target::Shell(target.to_owned()),
        icon_source: Some(target.to_owned()),
    })
}

/// A URI scheme is letters followed by `:`. A drive letter is a single
/// character, so `C:\x` is a path and `ms-settings:display` is a link.
fn is_uri(spec: &str) -> bool {
    match spec.split_once(':') {
        Some((scheme, _)) => {
            scheme.len() > 1 && scheme.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        }
        None => false,
    }
}

fn derive_kind(spec: &str) -> Kind {
    let path = Path::new(spec);
    if path.is_dir() {
        Kind::Folder
    } else if path.is_file() {
        Kind::App
    } else if is_uri(spec) {
        Kind::Link
    } else {
        log_warn!("manual entry does not exist and is not a URI: {spec}");
        Kind::App
    }
}

fn derive_title(spec: &str) -> String {
    let path = Path::new(spec);
    if path.is_dir() {
        return path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| spec.to_owned());
    }
    if path.is_file() {
        return path
            .file_stem()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| spec.to_owned());
    }
    if let Some((scheme, rest)) = spec.split_once(':')
        && is_uri(spec)
    {
        let rest = rest.trim_start_matches('/').trim_end_matches('/');
        return if rest.is_empty() { scheme.to_owned() } else { rest.to_owned() };
    }
    spec.to_owned()
}

/// Long paths are unreadable at tile width; keep the tail, which is the part
/// that identifies the target.
fn shorten_detail(spec: &str) -> String {
    const MAX: usize = 40;
    if spec.chars().count() <= MAX {
        return spec.to_owned();
    }
    let tail: String = spec
        .chars()
        .skip(spec.chars().count().saturating_sub(MAX - 1))
        .collect();
    format!("…{tail}")
}

/// The grid, in config order. Empty sections are dropped so a header never
/// appears over nothing.
///
/// Each window is claimed by the first section whose `match` accepts it, so a
/// filtered section listed above the catch-all is what pulls the browsers, or
/// Explorer, out into their own group. No window appears twice.
/// Who gets foreground rights before a browser raises itself. Not the socket's
/// peer: Chrome opens sockets from its network process, which owns no windows.
pub fn browser_pids() -> Vec<u32> {
    let Ok(s) = store().lock() else {
        return Vec::new();
    };
    let mut pids: Vec<u32> = s
        .windows
        .iter()
        .filter(|w| {
            w.exe
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().to_lowercase())
                .is_some_and(|exe| crate::config::BROWSERS.contains(&exe.as_str()))
        })
        .map(|w| w.pid)
        .collect();
    pids.sort_unstable();
    pids.dedup();
    pids
}

fn tab_items() -> Vec<Item> {
    crate::browser::server::tabs()
        .into_iter()
        .map(|owned| Item {
            id: ItemId::Tab(owned.connection, owned.tab.id),
            kind: Kind::Tab,
            // Filtering searches this line, so a generic title is still
            // findable by host.
            detail: owned.tab.host().to_string(),
            title: if owned.tab.title.is_empty() {
                owned.tab.host().to_string()
            } else {
                owned.tab.title.clone()
            },
            target: Target::Tab {
                connection: owned.connection,
                tab_id: owned.tab.id,
                window_id: owned.tab.window_id,
            },
            icon_source: owned
                .tab
                .icon
                .as_ref()
                .map(|key| format!("{}{key}", crate::shell::icons::FAVICON)),
        })
        .collect()
}

pub fn sections() -> Vec<Section> {
    let Ok(s) = store().lock() else {
        log_warn!("item store is poisoned; showing an empty grid");
        return Vec::new();
    };

    let mut claimed = vec![false; s.windows.len()];
    let mut out = Vec::with_capacity(s.groups.len());

    for group in &s.groups {
        let items = match group.source {
            Source::Windows => {
                let mut items = Vec::new();
                for (index, window) in s.windows.iter().enumerate() {
                    if claimed[index] || !group.claims(window) {
                        continue;
                    }
                    claimed[index] = true;
                    items.push(window.to_item());
                }
                items
            }
            // Read at show time, not resolved up front: they change as fast
            // as the browser does.
            Source::Tabs => tab_items(),
            _ => group.fixed.clone(),
        };

        if !items.is_empty() {
            out.push(Section {
                title: group.title.clone(),
                source: group.source,
                items,
            });
        }
    }
    out
}

/// Character-aware truncation; window titles are full of non-ASCII.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
}

/// `notify` receives `WM_MODEL_CHANGED` whenever the list changes.
pub fn install_hooks(notify: HWND) {
    NOTIFY.store(notify.0 as isize, Ordering::SeqCst);

    // Grouped into contiguous ranges; one hook per range is cheaper than one per
    // event. WINEVENT_SKIPOWNPROCESS keeps flick from reacting to itself.
    let ranges = [
        (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
        (EVENT_OBJECT_CREATE, EVENT_OBJECT_HIDE),
        (EVENT_OBJECT_NAMECHANGE, EVENT_OBJECT_NAMECHANGE),
        (EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED),
    ];

    let mut handles = Vec::new();
    for (first, last) in ranges {
        // SAFETY: out-of-context hooks deliver on this thread's message loop, so
        // `on_event` never runs concurrently with the UI thread.
        let hook = unsafe {
            SetWinEventHook(
                first,
                last,
                None,
                Some(on_event),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            )
        };
        if hook.is_invalid() {
            log_warn!("SetWinEventHook failed for range {first:#x}..{last:#x}");
        } else {
            handles.push(hook.0 as isize);
        }
    }
    log_info!("installed {} window event hooks", handles.len());
    if let Ok(mut h) = HOOKS.lock() {
        *h = handles;
    }
}

pub fn uninstall_hooks() {
    let Ok(mut handles) = HOOKS.lock() else { return };
    for raw in handles.drain(..) {
        // SAFETY: each handle came from a successful SetWinEventHook and is
        // unhooked exactly once.
        unsafe {
            let _ = UnhookWinEvent(HWINEVENTHOOK(raw as *mut core::ffi::c_void));
        }
    }
}

unsafe extern "system" fn on_event(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    // Only whole top-level windows. Without this we would also see every button
    // and menu item in every app on the machine.
    if id_object != OBJID_WINDOW.0 || id_child != CHILDID_SELF as i32 || hwnd.is_invalid() {
        return;
    }

    let changed = match event {
        EVENT_SYSTEM_FOREGROUND => on_foreground(hwnd),
        EVENT_OBJECT_CREATE | EVENT_OBJECT_UNCLOAKED => on_appear(hwnd),
        EVENT_OBJECT_DESTROY | EVENT_OBJECT_HIDE | EVENT_OBJECT_CLOAKED => on_vanish(hwnd),
        EVENT_OBJECT_NAMECHANGE => on_rename(hwnd),
        _ => false,
    };

    if changed {
        let notify = NOTIFY.load(Ordering::SeqCst);
        if notify != 0 {
            // SAFETY: posting is asynchronous and safe even if the target is
            // mid-teardown; a failed post is not worth reacting to.
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(notify as *mut core::ffi::c_void)),
                    WM_MODEL_CHANGED,
                    Default::default(),
                    Default::default(),
                );
            }
        }
    }
}

fn on_foreground(hwnd: HWND) -> bool {
    let handle = Handle::new(hwnd);
    let Ok(mut s) = store().lock() else { return false };
    if let Some(pos) = s.windows.iter().position(|w| w.handle == handle) {
        if pos == 0 {
            return false;
        }
        let entry = s.windows.remove(pos);
        s.windows.insert(0, entry);
        return true;
    }
    drop(s);
    // Foreground for something we have not seen: it just became eligible.
    on_appear(hwnd)
}

fn on_appear(hwnd: HWND) -> bool {
    let handle = Handle::new(hwnd);
    let exclude = {
        let Ok(s) = store().lock() else { return false };
        if handle == s.exclude || s.windows.iter().any(|w| w.handle == handle) {
            return false;
        }
        s.exclude
    };
    if !still_switchable(hwnd) {
        return false;
    }
    // A full pass, but only on a create/uncloak event, not on the hotkey. The
    // owner-chain test in `is_switchable` needs the surrounding windows to
    // decide whether this one is the switchable member of its group.
    let Some(info) = enumerate(exclude.hwnd())
        .into_iter()
        .find(|w| w.handle == handle)
    else {
        return false;
    };
    let Ok(mut s) = store().lock() else { return false };
    if s.windows.iter().any(|w| w.handle == handle) {
        return false;
    }
    s.windows.insert(0, info);
    true
}

fn on_vanish(hwnd: HWND) -> bool {
    let handle = Handle::new(hwnd);
    let Ok(mut s) = store().lock() else { return false };
    let before = s.windows.len();
    s.windows.retain(|w| w.handle != handle);
    s.windows.len() != before
}

fn on_rename(hwnd: HWND) -> bool {
    let Some(title) = refresh_title(hwnd) else {
        return false;
    };
    let handle = Handle::new(hwnd);
    let Ok(mut s) = store().lock() else { return false };
    match s.windows.iter_mut().find(|w| w.handle == handle) {
        Some(w) if w.title != title => {
            w.title = title;
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_letters_are_paths_and_schemes_are_uris() {
        assert!(!is_uri(r"C:\Windows\notepad.exe"));
        assert!(!is_uri(r"R:\dev"));
        assert!(is_uri("ms-settings:display"));
        assert!(is_uri("https://example.com"));
        assert!(!is_uri("plain-text"));
    }

    #[test]
    fn uri_titles_drop_the_scheme() {
        assert_eq!(derive_title("ms-settings:display"), "display");
        assert_eq!(derive_title("https://example.com/"), "example.com");
    }

    #[test]
    fn a_named_entry_keeps_its_title() {
        let entry = ManualItem::Named {
            title: "Display".into(),
            target: "ms-settings:display".into(),
        };
        let item = manual_item(&entry).unwrap();
        assert_eq!(item.title, "Display");
        assert_eq!(item.kind, Kind::Link);
        assert_eq!(item.target, Target::Shell("ms-settings:display".into()));
    }

    #[test]
    fn blank_entries_are_dropped() {
        assert!(manual_item(&ManualItem::Plain("   ".into())).is_none());
    }

    #[test]
    fn long_targets_keep_their_tail() {
        let long = format!("C:\\{}\\thing.exe", "x".repeat(80));
        let detail = shorten_detail(&long);
        assert!(detail.chars().count() <= 40);
        assert!(detail.ends_with("thing.exe"));
    }
}
