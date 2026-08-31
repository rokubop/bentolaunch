//! Live top-level windows.
//!
//! DESIGN.md is explicit that enumeration must not happen on the hotkey - that
//! is the usual reason these launchers feel sluggish. So this module enumerates
//! once at startup and then keeps the list current from `SetWinEventHook`.

use std::path::PathBuf;

use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, MAX_PATH};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GWL_EXSTYLE, GA_ROOTOWNER, GetAncestor, GetClassNameW, GetLastActivePopup,
    GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
    IsWindowVisible, WS_EX_TOOLWINDOW,
};
use windows::core::{BOOL, PWSTR};

use crate::model::{Handle, Item, ItemId, Kind, Target};

/// Shell surfaces that pass the alt-tab test but are never worth switching to.
const CLASS_DENYLIST: &[&str] = &[
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "Windows.UI.Core.CoreWindow",
    "ApplicationManager_DesktopShellWindow",
    "Xaml_WindowedPopupClass",
    "MultitaskingViewFrame",
    "ForegroundStaging",
];

#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub handle: Handle,
    pub title: String,
    pub class: String,
    pub exe: Option<PathBuf>,
    /// Owning process. Kept so foreground rights can be handed to a browser
    /// that is about to raise one of its own windows.
    pub pid: u32,
}

impl WindowInfo {
    /// Lowercased executable stem. Same shape `shell::link::app_stem` gives a
    /// pin, which is what makes the two comparable.
    pub fn app(&self) -> Option<String> {
        self.exe
            .as_ref()
            .and_then(|p| p.file_stem())
            .map(|stem| stem.to_string_lossy().to_lowercase())
    }

    pub fn to_item(&self) -> Item {
        let detail = self
            .exe
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.class.clone());
        Item {
            id: ItemId::Window(self.handle.raw()),
            kind: Kind::Window,
            title: self.title.clone(),
            detail,
            target: Target::Window(self.handle),
            // A filesystem path is already a valid shell parsing name.
            icon_source: self.exe.as_ref().map(|p| p.to_string_lossy().into_owned()),
            app: self.app(),
            origin: crate::config::Source::Windows,
            link: None,
            running: None,
            group: 0,
        }
    }
}

/// Snapshot of every window worth showing. Ordered by `EnumWindows`, which is
/// roughly Z-order, so the most recently used windows land first.
pub fn enumerate(exclude: HWND) -> Vec<WindowInfo> {
    let mut found: Vec<HWND> = Vec::with_capacity(64);
    // SAFETY: the callback only pushes to the Vec behind `lparam`, which
    // outlives the call, and EnumWindows is synchronous.
    unsafe {
        let _ = EnumWindows(
            Some(collect),
            LPARAM(&mut found as *mut Vec<HWND> as isize),
        );
    }

    found
        .into_iter()
        .filter(|&hwnd| hwnd != exclude)
        .filter_map(describe)
        .collect()
}

unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: lparam is the &mut Vec<HWND> handed to EnumWindows above.
    let out = unsafe { &mut *(lparam.0 as *mut Vec<HWND>) };
    if unsafe { is_switchable(hwnd) } {
        out.push(hwnd);
    }
    true.into()
}

/// The standard alt-tab eligibility test, plus the cloaking check that Windows
/// 11 needs to drop suspended UWP apps and hidden shell surfaces.
unsafe fn is_switchable(hwnd: HWND) -> bool {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            return false;
        }

        // Walk the owner chain: only the last visible active popup of a root
        // owner is the "real" window. Keeps dialogs from doubling up their parent.
        let mut walk = GetAncestor(hwnd, GA_ROOTOWNER);
        loop {
            let popup = GetLastActivePopup(walk);
            if popup == walk || IsWindowVisible(popup).as_bool() {
                walk = popup;
                break;
            }
            walk = popup;
        }
        if walk != hwnd {
            return false;
        }

        let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        if ex_style & WS_EX_TOOLWINDOW.0 != 0 {
            return false;
        }

        if is_cloaked(hwnd) {
            return false;
        }

        if GetWindowTextLengthW(hwnd) == 0 {
            return false;
        }

        !CLASS_DENYLIST.contains(&class_name(hwnd).as_str())
    }
}

unsafe fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked: u32 = 0;
    // SAFETY: the out-param matches DWMWA_CLOAKED's documented u32 type.
    let ok = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut core::ffi::c_void,
            size_of::<u32>() as u32,
        )
    };
    ok.is_ok() && cloaked != 0
}

unsafe fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    // SAFETY: GetClassNameW writes at most buf.len() units and returns the count.
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..len.max(0) as usize])
}

unsafe fn window_title(hwnd: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; len as usize + 1];
        let written = GetWindowTextW(hwnd, &mut buf);
        String::from_utf16_lossy(&buf[..written.max(0) as usize])
    }
}

/// The full path of the owning process. `PROCESS_QUERY_LIMITED_INFORMATION` is
/// the least privilege that answers this, and it works without elevation for
/// same-user processes (safety rule 1).
unsafe fn process_path(pid: u32) -> Option<PathBuf> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;

        let mut buf = [0u16; MAX_PATH as usize];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        );
        let _ = CloseHandle(handle);

        ok.ok()?;
        Some(PathBuf::from(String::from_utf16_lossy(
            &buf[..len as usize],
        )))
    }
}

fn describe(hwnd: HWND) -> Option<WindowInfo> {
    // SAFETY: hwnd came from EnumWindows and passed is_switchable this pass. It
    // can still die between then and now, in which case these calls fail
    // harmlessly and we drop the entry.
    unsafe {
        let title = window_title(hwnd);
        if title.trim().is_empty() {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        Some(WindowInfo {
            handle: Handle::new(hwnd),
            title,
            class: class_name(hwnd),
            exe: process_path(pid),
            pid,
        })
    }
}

/// Re-read one window's title. Used by the name-change hook, which fires
/// constantly for browsers and terminals.
pub fn refresh_title(hwnd: HWND) -> Option<String> {
    // SAFETY: a dead hwnd yields an empty title rather than misbehaving.
    let title = unsafe { window_title(hwnd) };
    (!title.trim().is_empty()).then_some(title)
}

/// Whether a window that already exists still belongs in the list.
pub fn still_switchable(hwnd: HWND) -> bool {
    // SAFETY: same contract as is_switchable.
    unsafe { is_switchable(hwnd) }
}
