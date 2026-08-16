//! Dropping a file or folder from Explorer onto the panel.
//!
//! An `IDropTarget` COM object registered on the panel window, as DESIGN.md
//! calls for. It does no work of its own: OLE calls arrive on the UI thread, so
//! each one turns into a synchronous message to the panel and the panel stays
//! the single place that touches its own state.
//!
//! Synchronous matters twice over. The reply to `WM_DRAG_OVER` is what decides
//! the drop effect the user sees, and the path list handed to `WM_DRAG_DROP`
//! lives on this stack frame — `SendMessageW` on the same thread is a direct
//! call, so the borrow is valid for exactly as long as the panel is reading it.

use windows::Win32::Foundation::{HWND, LPARAM, POINT, POINTL, WPARAM};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::System::Com::{DVASPECT_CONTENT, FORMATETC, IDataObject, TYMED_HGLOBAL};
use windows::Win32::System::Ole::{
    CF_HDROP, DROPEFFECT, DROPEFFECT_LINK, DROPEFFECT_NONE, IDropTarget, IDropTarget_Impl,
    RegisterDragDrop, ReleaseStgMedium, RevokeDragDrop,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{SendMessageW, WM_APP};
// `#[implement]` expands to paths rooted at the `windows_core` crate, which is
// why Cargo.toml depends on it directly as well as through `windows`.
use windows::core::{Ref, Result, implement};

use crate::{log_info, log_warn};

/// Cursor moved over the panel mid-drag. lparam carries the client point;
/// answering non-zero means "flick would take this".
pub const WM_DRAG_OVER: u32 = WM_APP + 5;
/// The drag left, or was cancelled.
pub const WM_DRAG_LEAVE: u32 = WM_APP + 6;
/// Dropped. wparam is a `*const Vec<String>` valid for the length of the call.
pub const WM_DRAG_DROP: u32 = WM_APP + 7;

/// Register the panel window as a drop target. Returns the target, which the
/// caller keeps alive for as long as the window exists.
pub fn register(hwnd: HWND) -> Option<IDropTarget> {
    let target: IDropTarget = DropZone { hwnd }.into();
    // SAFETY: hwnd is this thread's window and OLE is initialized on it.
    match unsafe { RegisterDragDrop(hwnd, &target) } {
        Ok(()) => {
            log_info!("drop target registered");
            Some(target)
        }
        Err(e) => {
            log_warn!("could not register the drop target ({e}); drag-to-pin is off");
            None
        }
    }
}

pub fn revoke(hwnd: HWND) {
    // SAFETY: revoking a window that was never registered fails harmlessly.
    unsafe {
        let _ = RevokeDragDrop(hwnd);
    }
}

#[implement(IDropTarget)]
struct DropZone {
    hwnd: HWND,
}

impl IDropTarget_Impl for DropZone_Impl {
    fn DragEnter(
        &self,
        data: Ref<IDataObject>,
        _keys: MODIFIERKEYS_FLAGS,
        point: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> Result<()> {
        let wanted = data.as_ref().is_some_and(has_files) && self.ask(WM_DRAG_OVER, point);
        self.answer(effect, wanted);
        Ok(())
    }

    fn DragOver(
        &self,
        _keys: MODIFIERKEYS_FLAGS,
        point: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> Result<()> {
        let wanted = self.ask(WM_DRAG_OVER, point);
        self.answer(effect, wanted);
        Ok(())
    }

    fn DragLeave(&self) -> Result<()> {
        // SAFETY: our own window, on this thread.
        unsafe {
            SendMessageW(self.hwnd, WM_DRAG_LEAVE, None, None);
        }
        Ok(())
    }

    fn Drop(
        &self,
        data: Ref<IDataObject>,
        _keys: MODIFIERKEYS_FLAGS,
        point: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> Result<()> {
        let paths = data.as_ref().map(paths_in).unwrap_or_default();
        let taken = !paths.is_empty() && {
            // SAFETY: same thread, so this is a direct call and `paths` outlives
            // every read the panel makes from the pointer.
            let answer = unsafe {
                SendMessageW(
                    self.hwnd,
                    WM_DRAG_DROP,
                    Some(WPARAM(&paths as *const Vec<String> as usize)),
                    Some(client_point(self.hwnd, point)),
                )
            };
            answer.0 != 0
        };
        if !taken {
            // SAFETY: our own window.
            unsafe {
                SendMessageW(self.hwnd, WM_DRAG_LEAVE, None, None);
            }
        }
        self.answer(effect, taken);
        Ok(())
    }
}

impl DropZone_Impl {
    /// Ask the panel a yes/no question about the point under the cursor.
    fn ask(&self, message: u32, point: &POINTL) -> bool {
        // SAFETY: our own window, on this thread.
        let answer = unsafe {
            SendMessageW(self.hwnd, message, None, Some(client_point(self.hwnd, point)))
        };
        answer.0 != 0
    }

    /// Link, not copy: pinning records where something lives, it never moves or
    /// duplicates the thing itself.
    fn answer(&self, effect: *mut DROPEFFECT, wanted: bool) {
        // SAFETY: OLE always passes a writable effect out-param.
        unsafe {
            *effect = if wanted { DROPEFFECT_LINK } else { DROPEFFECT_NONE };
        }
    }
}

fn hdrop_format() -> FORMATETC {
    FORMATETC {
        cfFormat: CF_HDROP.0,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    }
}

fn has_files(data: &IDataObject) -> bool {
    // SAFETY: a query only; the format descriptor is a stack local.
    unsafe { data.QueryGetData(&hdrop_format()).is_ok() }
}

/// The dropped paths. Anything that is not a file list yields nothing.
fn paths_in(data: &IDataObject) -> Vec<String> {
    // SAFETY: the medium is released on every path out, including the early
    // return when the drop carries no file list.
    unsafe {
        let Ok(mut medium) = data.GetData(&hdrop_format()) else {
            return Vec::new();
        };
        let drop = HDROP(medium.u.hGlobal.0);
        let count = DragQueryFileW(drop, u32::MAX, None);

        let mut paths = Vec::with_capacity(count as usize);
        for index in 0..count {
            let length = DragQueryFileW(drop, index, None) as usize;
            let mut buffer = vec![0u16; length + 1];
            let written = DragQueryFileW(drop, index, Some(&mut buffer)) as usize;
            if written > 0 {
                paths.push(String::from_utf16_lossy(&buffer[..written]));
            }
        }

        ReleaseStgMedium(&mut medium);
        paths
    }
}

/// Screen point to the panel's client coordinates, packed the way every mouse
/// message packs one, so the panel's existing hit tests read it unchanged.
fn client_point(hwnd: HWND, point: &POINTL) -> LPARAM {
    let mut local = POINT { x: point.x, y: point.y };
    // SAFETY: `local` is a stack local; a failed conversion leaves it as screen
    // coordinates, which simply misses every tile.
    unsafe {
        let _ = ScreenToClient(hwnd, &mut local);
    }
    LPARAM(((local.y as i16 as u16 as isize) << 16) | (local.x as i16 as u16 as isize))
}
