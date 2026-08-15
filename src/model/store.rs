//! The live item list, kept current by `SetWinEventHook` so that showing the
//! panel is a read, never a scan.
//!
//! Windows are held in MRU order: the foreground hook moves the newly focused
//! window to the front, which is the order a switcher wants.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Mutex, OnceLock};

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::WindowsAndMessaging::{
    CHILDID_SELF, EVENT_OBJECT_CLOAKED, EVENT_OBJECT_CREATE, EVENT_OBJECT_DESTROY,
    EVENT_OBJECT_HIDE, EVENT_OBJECT_NAMECHANGE, EVENT_OBJECT_UNCLOAKED, EVENT_SYSTEM_FOREGROUND,
    OBJID_WINDOW, PostMessageW, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_APP,
};

// Imported by name rather than as a module: `windows` would otherwise shadow the
// `windows` crate throughout this file.
use crate::model::windows::{WindowInfo, enumerate, refresh_title, still_switchable};
use crate::model::{Handle, Item, ItemId, Kind};
use crate::{log_info, log_warn};

/// Posted to the panel when the item list changed. Only acted on while visible.
pub const WM_MODEL_CHANGED: u32 = WM_APP + 1;

struct Store {
    windows: Vec<WindowInfo>,
    pinned: Vec<Item>,
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
            pinned: Vec::new(),
            exclude: Handle::new(HWND(std::ptr::null_mut())),
        })
    })
}

/// First and only full enumeration. Everything after this is incremental.
pub fn init(exclude: HWND, pinned_paths: &[String]) {
    let pinned = pinned_paths.iter().filter_map(|p| pin_item(p)).collect::<Vec<_>>();
    let found = enumerate(exclude);
    log_info!(
        "initial scan: {} windows, {} pinned",
        found.len(),
        pinned.len()
    );
    // Milestone 1 is about validating what flick sees before it acts on any of
    // it, so the whole list goes to the log.
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

    if let Ok(mut s) = store().lock() {
        s.exclude = Handle::new(exclude);
        s.windows = found;
        s.pinned = pinned;
    }
}

fn pin_item(spec: &str) -> Option<Item> {
    let path = PathBuf::from(spec);
    if !path.exists() {
        log_warn!("pinned entry does not exist, skipping: {spec}");
        return None;
    }
    let kind = if path.is_dir() { Kind::Folder } else { Kind::App };
    let title = pin_title(&path);
    Some(Item {
        id: ItemId::Path(path.clone()),
        kind,
        title,
        detail: path.to_string_lossy().into_owned(),
        handle: None,
        icon_source: Some(path),
    })
}

/// Character-aware truncation; window titles are full of non-ASCII.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
}

fn pin_title(path: &Path) -> String {
    path.file_stem()
        .or_else(|| path.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// Pins first so their positions never move, then windows in MRU order.
pub fn items() -> Vec<Item> {
    let Ok(s) = store().lock() else {
        log_warn!("item store is poisoned; showing an empty grid");
        return Vec::new();
    };
    let mut out = s.pinned.clone();
    out.extend(s.windows.iter().map(WindowInfo::to_item));
    out
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
