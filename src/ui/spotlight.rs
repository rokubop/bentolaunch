//! A ring drawn around the window the move bar acts on, out on the desktop.
//!
//! The tile ring says which tile. This says which window, which is the question
//! you actually have when six of them are open and two are called "Chrome".
//!
//! Safety rule 7 is the whole design here. A second topmost window is exactly
//! the thing that feels like a broken PC, so this one:
//!
//! - is `WS_EX_TRANSPARENT | WS_EX_NOACTIVATE`, so it can take neither a click
//!   nor the foreground,
//! - is shaped by `SetWindowRgn` to the border alone, so there is no middle to
//!   cover anything with,
//! - is registered with `safety`, so the panic hook and the watchdog hide it
//!   along with the panel,
//! - never outlives the panel: it is destroyed on `Drop`.

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CombineRgn, CreateRoundRectRgn, CreateSolidBrush, DeleteObject, EndPaint,
    FillRect, HBRUSH, PAINTSTRUCT, RGN_DIFF, SetWindowRgn,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA, GetWindowLongPtrW,
    RegisterClassExW, SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_HIDE, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, WM_DESTROY, WM_ERASEBKGND, WM_PAINT, WNDCLASSEXW, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{PCWSTR, Result, w};

use crate::safety;
use crate::shell::arrange;
use crate::{log_info, log_warn};

const CLASS_NAME: PCWSTR = w!("bentopick_spotlight");
/// Thick enough to find with your eyes across a 5760px desktop.
const THICKNESS: i32 = 4;
/// How far outside the window the ring sits, so it frames rather than covers.
const MARGIN: i32 = 2;
const CORNER: i32 = 10;

pub struct Spotlight {
    hwnd: HWND,
    brush: HBRUSH,
    /// Where it is now, so an unchanged target does not repaint every move.
    at: Option<arrange::Rect>,
}

impl Spotlight {
    /// `color` is 0xRRGGBB. The ring is opaque: this window has no alpha, which
    /// is what lets it be a plain shaped popup rather than a layered one.
    pub fn create(color: u32) -> Result<Spotlight> {
        // SAFETY: registering a class twice is harmless, and the brush lives as
        // long as the window that paints with it.
        unsafe {
            let instance = GetModuleHandleW(None)?;
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wndproc),
                hInstance: instance.into(),
                lpszClassName: CLASS_NAME,
                hbrBackground: HBRUSH(std::ptr::null_mut()),
                ..Default::default()
            };
            RegisterClassExW(&class);

            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE,
                CLASS_NAME,
                w!("BentoPick target"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance.into()),
                None,
            )?;

            let bgr = ((color & 0xFF) << 16) | (color & 0xFF00) | ((color >> 16) & 0xFF);
            let brush = CreateSolidBrush(COLORREF(bgr));
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, brush.0 as isize);
            safety::register_spotlight(hwnd);
            Ok(Spotlight { hwnd, brush, at: None })
        }
    }

    /// Frame a window. `below` keeps it under the panel, so the two never fight
    /// over the same pixels.
    pub fn show(&mut self, frame: arrange::Rect, below: HWND) {
        let outer = RECT {
            left: frame.left - MARGIN - THICKNESS,
            top: frame.top - MARGIN - THICKNESS,
            right: frame.right + MARGIN + THICKNESS,
            bottom: frame.bottom + MARGIN + THICKNESS,
        };
        let (w, h) = (outer.right - outer.left, outer.bottom - outer.top);
        if w <= 0 || h <= 0 {
            self.hide();
            return;
        }

        // SAFETY: both regions are owned here. SetWindowRgn takes the combined
        // one, and the other two are deleted before returning.
        unsafe {
            let ring = CreateRoundRectRgn(0, 0, w + 1, h + 1, CORNER, CORNER);
            let hole = CreateRoundRectRgn(
                THICKNESS,
                THICKNESS,
                w - THICKNESS + 1,
                h - THICKNESS + 1,
                CORNER,
                CORNER,
            );
            CombineRgn(Some(ring), Some(ring), Some(hole), RGN_DIFF);
            let _ = DeleteObject(hole.into());
            // The window owns `ring` from here; Windows deletes it on the next
            // SetWindowRgn or on destroy.
            if SetWindowRgn(self.hwnd, Some(ring), false) == 0 {
                log_warn!("could not shape the target ring");
                let _ = DeleteObject(ring.into());
                return;
            }

            let _ = SetWindowPos(
                self.hwnd,
                Some(below),
                outer.left,
                outer.top,
                w,
                h,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
        self.at = Some(frame);
    }

    pub fn hide(&mut self) {
        if self.at.take().is_none() {
            return;
        }
        // SAFETY: our own window, on our own thread.
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }
}

impl Drop for Spotlight {
    fn drop(&mut self) {
        // SAFETY: both handles were created here and are released once.
        unsafe {
            let _ = DestroyWindow(self.hwnd);
            let _ = DeleteObject(self.brush.into());
        }
        safety::register_spotlight(HWND(std::ptr::null_mut()));
        log_info!("target ring torn down");
    }
}

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: the brush is stashed at creation and cleared only when the window
    // is destroyed, so it is live for every message that reads it.
    unsafe {
        match message {
            // The region already limits this to the border, so filling the
            // whole client area paints exactly the ring.
            WM_PAINT => {
                let brush = HBRUSH(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut core::ffi::c_void);
                let mut ps = PAINTSTRUCT::default();
                let dc = BeginPaint(hwnd, &mut ps);
                if !brush.is_invalid() {
                    FillRect(dc, &ps.rcPaint, brush);
                }
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_DESTROY => {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }
}
