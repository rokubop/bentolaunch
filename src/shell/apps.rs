//! Every installed app, from `shell:AppsFolder`.
//!
//! The same virtual folder the "Add an app" picker opens, enumerated instead of
//! browsed. It holds Store apps as well as desktop ones, and each entry's
//! parsing name is exactly what `Target::Shell` already launches and what
//! `icons` already fetches an icon for - so a tile out of here is an ordinary
//! tile in every other respect.
//!
//! Safety rule 6: the enumeration is a shell call and shell calls block. It runs
//! on a worker, the UI thread asks and gets `None` until the list lands, and the
//! panel is told to rebuild when it does.

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize};
use windows::Win32::UI::Shell::{
    BHID_EnumItems, IEnumShellItems, IShellItem, SHCreateItemFromParsingName,
    SIGDN_NORMALDISPLAY, SIGDN_PARENTRELATIVEPARSING,
};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
use windows::core::HSTRING;

use crate::model::store::WM_MODEL_CHANGED;
use crate::{log_info, log_warn};

/// The virtual folder holding every installed app, Store apps included.
const APPS_FOLDER: &str = "shell:AppsFolder";

/// One installed app: what to call it, and what to hand the shell.
#[derive(Clone)]
pub struct App {
    pub title: String,
    /// A shell parsing name, launchable and iconable as it stands.
    pub target: String,
}

/// How long a read stays good for. Apps are installed and uninstalled while the
/// panel is up, and a list read once per process run would say otherwise for
/// days. The refresh is a background read that replaces the list in place, so
/// nothing blinks and nothing waits.
const STALE_AFTER: Duration = Duration::from_secs(300);

/// The list as last read, and when. `None` until the first read lands.
type Read = Option<(Arc<Vec<App>>, Instant)>;

static LIST: OnceLock<Mutex<Read>> = OnceLock::new();
static LOADING: AtomicBool = AtomicBool::new(false);
static NOTIFY: AtomicIsize = AtomicIsize::new(0);

fn list() -> &'static Mutex<Read> {
    LIST.get_or_init(|| Mutex::new(None))
}

/// `notify` receives `WM_MODEL_CHANGED` when the list lands.
pub fn start(notify: HWND) {
    NOTIFY.store(notify.0 as isize, Ordering::SeqCst);
}

/// Non-blocking. `None` means the worker is still reading, and the caller draws
/// what it can and comes back on `WM_MODEL_CHANGED`.
pub fn request() -> Option<Arc<Vec<App>>> {
    let held = match list().lock() {
        Ok(held) => held.clone(),
        Err(_) => None,
    };
    let fresh = held
        .as_ref()
        .is_some_and(|(_, read)| read.elapsed() < STALE_AFTER);
    // One reader at a time. A second summon while the first is still going
    // would otherwise walk the folder again for the same answer.
    if !fresh && !LOADING.swap(true, Ordering::SeqCst) {
        let spawned = std::thread::Builder::new()
            .name("bentolaunch-apps".to_owned())
            .spawn(load);
        if let Err(e) = spawned {
            log_warn!("could not start the installed-apps reader: {e}");
            LOADING.store(false, Ordering::SeqCst);
        }
    }
    // The list it has, stale or not. A refresh replaces it a moment later and
    // the panel is told to rebuild; an empty box in the meantime would be the
    // list flickering out every five minutes.
    held.map(|(apps, _)| apps)
}

fn load() {
    // MTA: this thread never pumps messages, so it must not be an STA host.
    // SAFETY: paired with CoUninitialize below.
    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    if com.is_err() {
        log_warn!("the installed-apps reader could not initialize COM");
        LOADING.store(false, Ordering::SeqCst);
        return;
    }

    let apps = enumerate();
    log_info!("{} installed app(s) read", apps.len());
    if let Ok(mut held) = list().lock() {
        *held = Some((Arc::new(apps), Instant::now()));
    }
    LOADING.store(false, Ordering::SeqCst);

    // SAFETY: posting is asynchronous and safe even if the panel is mid-
    // teardown; a failed post is not worth reacting to.
    let notify = NOTIFY.load(Ordering::SeqCst);
    if notify != 0 {
        unsafe {
            let _ = PostMessageW(
                Some(HWND(notify as *mut core::ffi::c_void)),
                WM_MODEL_CHANGED,
                Default::default(),
                Default::default(),
            );
        }
    }

    // SAFETY: pairs with the CoInitializeEx above, on the same thread.
    unsafe { CoUninitialize() };
}

fn enumerate() -> Vec<App> {
    // SAFETY: every interface below is released by its own Drop, and each name
    // is freed with CoTaskMemFree exactly once.
    let mut apps: Vec<App> = unsafe {
        let folder: IShellItem =
            match SHCreateItemFromParsingName(&HSTRING::from(APPS_FOLDER), None) {
                Ok(item) => item,
                Err(e) => {
                    log_warn!("could not open {APPS_FOLDER}: {e}");
                    return Vec::new();
                }
            };
        let items: IEnumShellItems = match folder.BindToHandler(None, &BHID_EnumItems) {
            Ok(items) => items,
            Err(e) => {
                log_warn!("could not enumerate {APPS_FOLDER}: {e}");
                return Vec::new();
            }
        };

        let mut out = Vec::new();
        loop {
            let mut fetched = [const { None }; 1];
            let mut count = 0u32;
            if items.Next(&mut fetched, Some(&mut count)).is_err() || count == 0 {
                break;
            }
            let Some(item) = fetched[0].take() else { break };
            // `shell:AppsFolder\<child>`, always. A child of this folder names
            // itself by AppUserModelID, and an AUMID on its own is a string
            // neither `ShellExecuteW` nor the icon factory can resolve - which
            // showed as a list of apps with no icons that launched nothing.
            let (Some(title), Some(child)) = (
                name(&item, SIGDN_NORMALDISPLAY),
                name(&item, SIGDN_PARENTRELATIVEPARSING),
            ) else {
                continue;
            };
            out.push(App { title, target: format!("{APPS_FOLDER}\\{child}") });
        }
        out
    };

    // Alphabetical, which is the order every other all-apps list on Windows
    // uses. The folder's own order is not one anybody has learned.
    apps.sort_by_key(|app| app.title.to_lowercase());
    apps
}

/// One display name off a shell item, copied out before the shell's buffer is
/// freed.
unsafe fn name(item: &IShellItem, form: windows::Win32::UI::Shell::SIGDN) -> Option<String> {
    // SAFETY: the caller holds the item; the returned buffer is ours to free.
    unsafe {
        let raw = item.GetDisplayName(form).ok()?;
        let text = raw.to_string().ok();
        CoTaskMemFree(Some(raw.0 as *const core::ffi::c_void));
        text.filter(|s| !s.is_empty())
    }
}
