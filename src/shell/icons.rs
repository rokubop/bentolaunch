//! Icons from `IShellItemImageFactory`, fetched on worker threads.
//!
//! Safety rule 5 is the whole design here. `IShellItemImageFactory::GetImage`
//! can block for *seconds* on a network path or a misbehaving shell extension,
//! and a blocking COM call cannot be cancelled from outside. So the UI thread
//! never calls it and never waits on it: it asks for an icon, gets `None`, draws
//! the tile without one, and repaints when the worker delivers. The "timeout" is
//! structural — there is no code path on which the UI can block at all.
//!
//! Requests use `SIIGBF_ICONONLY`. Real thumbnail extraction is the slow, risky
//! half of the shell imaging API, and for apps and folders the icon is what a
//! switcher wants anyway. Window previews come from capture in Milestone 3.
//!
//! Keys are shell parsing names, not paths, so the same cache serves an exe, a
//! folder, a `.lnk`, a Store app by AppUserModelID, and a `ms-settings:` URI.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use windows::Win32::Foundation::{HWND, MAX_PATH, SIZE};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits,
    GetObjectW, HBITMAP, ReleaseDC,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::UI::Shell::{
    ASSOCF_IS_PROTOCOL, ASSOCSTR_EXECUTABLE, AssocQueryStringW, IShellItemImageFactory,
    SHCreateItemFromParsingName, SIIGBF_BIGGERSIZEOK, SIIGBF_ICONONLY,
};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
use windows::core::{HSTRING, PWSTR};

use crate::{log_info, log_warn};

/// Posted to the panel when at least one icon finished loading.
pub const WM_ICON_READY: u32 = WM_APP + 3;

/// How many workers. More than one so a single wedged shell extension does not
/// stall every remaining icon.
const WORKERS: usize = 2;

/// A request slower than this says something on the machine is misbehaving.
/// Logged, not enforced — a blocking COM call cannot be cancelled.
const SLOW_REQUEST_MS: u128 = 2_000;

/// Premultiplied BGRA, top-down, ready to hand to Direct2D.
pub struct IconPixels {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct Key {
    /// A shell parsing name.
    name: String,
    size: u32,
}

struct Cache {
    /// `None` means "asked, and there is no icon" — cached so a failing path is
    /// not retried on every show.
    entries: HashMap<Key, Option<Arc<IconPixels>>>,
    in_flight: HashSet<Key>,
}

static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
static SENDER: OnceLock<Mutex<Sender<Key>>> = OnceLock::new();
static NOTIFY: AtomicIsize = AtomicIsize::new(0);

fn cache() -> &'static Mutex<Cache> {
    CACHE.get_or_init(|| {
        Mutex::new(Cache {
            entries: HashMap::new(),
            in_flight: HashSet::new(),
        })
    })
}

/// Start the workers. `notify` receives `WM_ICON_READY` as icons land.
pub fn start(notify: HWND) {
    NOTIFY.store(notify.0 as isize, Ordering::SeqCst);

    let (tx, rx) = channel::<Key>();
    let rx = Arc::new(Mutex::new(rx));
    let _ = SENDER.set(Mutex::new(tx));

    for n in 0..WORKERS {
        let rx: Arc<Mutex<Receiver<Key>>> = Arc::clone(&rx);
        let spawned = std::thread::Builder::new()
            .name(format!("flick-icons-{n}"))
            .spawn(move || worker(rx));
        if let Err(e) = spawned {
            log_warn!("could not start icon worker {n}: {e}");
        }
    }
    log_info!("icon workers started");
}

/// Non-blocking. Returns the icon if it is already cached, otherwise queues it
/// and returns `None` — the caller draws without an icon and repaints on
/// `WM_ICON_READY`.
pub fn request(parsing_name: &str, size: u32) -> Option<Arc<IconPixels>> {
    let key = Key { name: parsing_name.to_owned(), size };

    {
        let Ok(mut c) = cache().lock() else { return None };
        if let Some(hit) = c.entries.get(&key) {
            return hit.clone();
        }
        if !c.in_flight.insert(key.clone()) {
            return None;
        }
    }

    let queued = SENDER
        .get()
        .and_then(|tx| tx.lock().ok().map(|tx| tx.send(key.clone()).is_ok()))
        .unwrap_or(false);

    if !queued {
        // No worker to service it; drop the reservation so a later attempt can
        // retry rather than waiting forever on a request that will never run.
        if let Ok(mut c) = cache().lock() {
            c.in_flight.remove(&key);
        }
    }
    None
}

fn worker(rx: Arc<Mutex<Receiver<Key>>>) {
    // MTA: this thread never pumps messages, so it must not be an STA host.
    // SAFETY: paired with CoUninitialize when the channel closes.
    unsafe {
        if CoInitializeEx(None, COINIT_MULTITHREADED).is_err() {
            log_warn!("icon worker could not initialize COM; icons are disabled on this thread");
            return;
        }
    }

    loop {
        // The lock is released before the (potentially very slow) fetch, so the
        // other worker can pick up the next request immediately.
        let key = {
            let Ok(rx) = rx.lock() else { break };
            match rx.recv() {
                Ok(key) => key,
                Err(_) => break,
            }
        };

        let started = Instant::now();
        let pixels = fetch(&key).map(Arc::new);
        let elapsed = started.elapsed().as_millis();
        if elapsed > SLOW_REQUEST_MS {
            log_warn!(
                "icon for {} took {elapsed}ms — a shell extension is slow on this target",
                key.name
            );
        }

        if let Ok(mut c) = cache().lock() {
            c.in_flight.remove(&key);
            c.entries.insert(key, pixels);
        }

        let notify = NOTIFY.load(Ordering::SeqCst);
        if notify != 0 {
            // SAFETY: an asynchronous post; harmless if the window is gone.
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(notify as *mut core::ffi::c_void)),
                    WM_ICON_READY,
                    Default::default(),
                    Default::default(),
                );
            }
        }
    }

    // SAFETY: pairs with the CoInitializeEx above.
    unsafe { CoUninitialize() };
}

fn fetch(key: &Key) -> Option<IconPixels> {
    // SAFETY: every COM object is scoped to this call, and the HBITMAP is
    // deleted on both the success and failure paths below.
    unsafe {
        // For a URI, ask the handler first. `SHCreateItemFromParsingName` does
        // not reject `ms-settings:display` outright — it hands back a generic
        // item whose icon is a blank page — so trying it first would always win
        // and always look wrong. The app registered to open the scheme is the
        // one the tile will actually launch, so its icon is the honest one.
        let factory: IShellItemImageFactory = match protocol_handler(&key.name) {
            Some(exe) => shell_item(&exe).or_else(|| shell_item(&key.name))?,
            None => shell_item(&key.name)?,
        };

        let size = SIZE { cx: key.size as i32, cy: key.size as i32 };
        let bitmap = factory
            .GetImage(size, SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK)
            .ok()?;

        let pixels = read_bitmap(bitmap);
        let _ = DeleteObject(bitmap.into());
        pixels
    }
}

unsafe fn shell_item(parsing_name: &str) -> Option<IShellItemImageFactory> {
    // SAFETY: the HSTRING outlives the call.
    unsafe { SHCreateItemFromParsingName(&HSTRING::from(parsing_name), None).ok() }
}

/// Schemes owned by a packaged app rather than a classic executable.
/// `AssocQueryStringW` reports ERROR_NO_ASSOCIATION for these, because there is
/// no .exe to name. The AppUserModelIDs are stable across Windows 10 and 11.
const PACKAGED_HANDLERS: &[(&str, &str)] = &[(
    "ms-settings",
    r"shell:AppsFolder\windows.immersivecontrolpanel_cw5n1h2txyewy!microsoft.windows.immersivecontrolpanel",
)];

/// What will open a URI: the executable registered for its scheme, or a known
/// packaged app. `None` for anything that is not a URI.
unsafe fn protocol_handler(parsing_name: &str) -> Option<String> {
    let (scheme, _) = parsing_name.split_once(':')?;
    if scheme.len() <= 1 || !scheme.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return None;
    }

    if let Some((_, aumid)) = PACKAGED_HANDLERS
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(scheme))
    {
        return Some((*aumid).to_owned());
    }

    let mut buf = [0u16; MAX_PATH as usize];
    let mut len = buf.len() as u32;
    // SAFETY: len starts as the buffer capacity and is updated to the length
    // written, per the API contract.
    let hr = unsafe {
        AssocQueryStringW(
            ASSOCF_IS_PROTOCOL,
            ASSOCSTR_EXECUTABLE,
            &HSTRING::from(scheme),
            None,
            Some(PWSTR(buf.as_mut_ptr())),
            &mut len,
        )
    };
    if hr.is_err() || len == 0 {
        return None;
    }

    // The returned length includes the terminating NUL.
    let exe = String::from_utf16_lossy(&buf[..(len as usize).saturating_sub(1)]);
    (!exe.is_empty()).then_some(exe)
}

/// Copy an HBITMAP into a premultiplied BGRA buffer.
unsafe fn read_bitmap(bitmap: HBITMAP) -> Option<IconPixels> {
    unsafe {
        let mut info = BITMAP::default();
        let written = GetObjectW(
            bitmap.into(),
            size_of::<BITMAP>() as i32,
            Some(&mut info as *mut BITMAP as *mut core::ffi::c_void),
        );
        if written == 0 || info.bmWidth <= 0 || info.bmHeight <= 0 {
            return None;
        }

        let width = info.bmWidth as u32;
        let height = info.bmHeight as u32;

        let mut header = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: info.bmWidth,
                // Negative height requests a top-down buffer, matching D2D.
                biHeight: -info.bmHeight,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };

        let mut bgra = vec![0u8; (width * height * 4) as usize];
        let dc = GetDC(None);
        let rows = GetDIBits(
            dc,
            bitmap,
            0,
            height,
            Some(bgra.as_mut_ptr() as *mut core::ffi::c_void),
            &mut header,
            DIB_RGB_COLORS,
        );
        ReleaseDC(None, dc);

        if rows == 0 {
            return None;
        }

        // The shell returns straight alpha; Direct2D was asked for premultiplied.
        for px in bgra.chunks_exact_mut(4) {
            let a = px[3] as u32;
            px[0] = ((px[0] as u32 * a) / 255) as u8;
            px[1] = ((px[1] as u32 * a) / 255) as u8;
            px[2] = ((px[2] as u32 * a) / 255) as u8;
        }

        Some(IconPixels { width, height, bgra })
    }
}
