//! Moving a window around the screens.
//!
//! `SetWindowPos`, not a synthesized Win+arrow. bentopick already holds the
//! HWND, so none of this needs the target foregrounded, needs `super` held down
//! across a run, or raises snap assist to escape from afterwards.

use windows::Win32::Foundation::{HWND, LPARAM, RECT, TRUE};
use windows::core::BOOL;
use windows::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MonitorFromWindow,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowPlacement, GetWindowRect, IsIconic, IsWindow, IsZoomed, SHOW_WINDOW_CMD,
    SW_SHOWMAXIMIZED, SW_SHOWMINNOACTIVE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOZORDER,
    SetWindowPlacement, SetWindowPos, WINDOWPLACEMENT,
};

use std::sync::Mutex;

use crate::model::Handle;
use crate::model::windows::still_switchable;
use crate::{log_dry, log_info, log_warn};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Move {
    Left,
    Right,
    Up,
    Down,
    ScreenLeft,
    ScreenRight,
}

impl Move {
    pub fn label(self) -> &'static str {
        match self {
            Move::Left => "Left",
            Move::Right => "Right",
            Move::Up => "Up",
            Move::Down => "Down",
            Move::ScreenLeft => "Screen left",
            Move::ScreenRight => "Screen right",
        }
    }

    /// The name in an item id and in a log line.
    pub fn key(self) -> &'static str {
        match self {
            Move::Left => "left",
            Move::Right => "right",
            Move::Up => "up",
            Move::Down => "down",
            Move::ScreenLeft => "screen_left",
            Move::ScreenRight => "screen_right",
        }
    }
}

/// Reading order: which screen first, then where on it. Coarse to fine, the
/// order the decisions actually get made in.
pub const MOVES: [Move; 6] = [
    Move::ScreenLeft,
    Move::ScreenRight,
    Move::Left,
    Move::Right,
    Move::Up,
    Move::Down,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn w(self) -> i32 {
        self.right - self.left
    }

    pub fn h(self) -> i32 {
        self.bottom - self.top
    }
}

impl From<RECT> for Rect {
    fn from(r: RECT) -> Self {
        Rect { left: r.left, top: r.top, right: r.right, bottom: r.bottom }
    }
}

/// Where a side move puts a window: half the width, full height, like
/// `Win+Left` does. `None` for everything else, which changes the window's show
/// state or its screen rather than its rect.
pub fn placed(area: Rect, mv: Move) -> Option<Rect> {
    let half = area.w() / 2;
    match mv {
        Move::Left => Some(Rect { left: area.left, top: area.top, right: area.left + half, bottom: area.bottom }),
        Move::Right => Some(Rect { left: area.left + half, top: area.top, right: area.right, bottom: area.bottom }),
        _ => None,
    }
}

/// The same place on another screen, in proportion, so a half stays a half
/// whatever the two monitors measure.
pub fn across(from: Rect, to: Rect, window: Rect) -> Rect {
    let sx = f64::from(to.w()) / f64::from(from.w().max(1));
    let sy = f64::from(to.h()) / f64::from(from.h().max(1));
    let left = to.left + (f64::from(window.left - from.left) * sx).round() as i32;
    let top = to.top + (f64::from(window.top - from.top) * sy).round() as i32;
    let w = (f64::from(window.w()) * sx).round() as i32;
    let h = (f64::from(window.h()) * sy).round() as i32;
    Rect { left, top, right: left + w, bottom: top + h }
}

/// The screen a hop lands on. Clamped rather than wrapped: running off the end
/// of the row and leaving the window put beats it reappearing at the far side.
pub fn neighbour(areas: &[Rect], from: usize, mv: Move) -> Option<usize> {
    let step: isize = match mv {
        Move::ScreenLeft => -1,
        Move::ScreenRight => 1,
        _ => return None,
    };
    let next = from as isize + step;
    (next >= 0 && (next as usize) < areas.len()).then_some(next as usize)
}

// --- the Win32 edge ---

/// Work areas left to right, which is the order the two screen hops step
/// through.
fn work_areas() -> Vec<Rect> {
    let mut out: Vec<Rect> = Vec::new();
    // SAFETY: `collect` only writes through the pointer handed to it here, and
    // the vector outlives the enumeration.
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(collect), LPARAM(&raw mut out as isize));
    }
    out.sort_by_key(|area| area.left);
    out
}

unsafe extern "system" fn collect(monitor: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: `data` is the vector work_areas passed in, alive for the whole
    // enumeration.
    unsafe {
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            (*(data.0 as *mut Vec<Rect>)).push(info.rcWork.into());
        }
    }
    TRUE
}

/// What the window claims, and what you can see of it.
///
/// A window rect includes the invisible resize border DWM leaves around it,
/// about 7px a side. Placing by that rect leaves a snapped window looking short
/// of the screen edge, so the arithmetic runs on the visible frame and the
/// difference is added back at the end.
fn frames(hwnd: HWND) -> Option<(Rect, Rect)> {
    let mut outer = RECT::default();
    let mut visible = RECT::default();
    // SAFETY: both rects outlive the calls, and the size matches the attribute.
    unsafe {
        if GetWindowRect(hwnd, &mut outer).is_err() {
            return None;
        }
        if DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&raw mut visible).cast(),
            size_of::<RECT>() as u32,
        )
        .is_err()
        {
            visible = outer;
        }
    }
    Some((outer.into(), visible.into()))
}

/// Whether the six mean anything for this window.
///
/// The same rule that keeps the desktop and the taskbar out of the grid: with
/// nothing but wallpaper in front of you there is no window to send anywhere,
/// and the bar says so rather than doing nothing when clicked.
pub fn movable(handle: Handle) -> bool {
    still_switchable(handle.hwnd())
}

/// Where a window sat before bentopick first moved it.
///
/// Windows keeps this itself, as `rcNormalPosition`, and that is what "restore"
/// means to it. `SetWindowPos` overwrites it, so the first move takes a copy:
/// without one, a snapped window has nowhere to go back to and down off a snap
/// could only minimize.
static ORIGINS: Mutex<Vec<(isize, RECT)>> = Mutex::new(Vec::new());

/// First touch only. A second call is the window's snapped position, which is
/// exactly what must not be remembered.
fn remember(handle: Handle, normal: RECT) {
    let Ok(mut origins) = ORIGINS.lock() else { return };
    if origins.iter().any(|(h, _)| *h == handle.raw()) {
        return;
    }
    // A closed window never comes back, so its entry never gets claimed.
    // SAFETY: IsWindow is the documented way to ask, and answers false for a
    // handle that has been reused or destroyed.
    origins.retain(|(h, _)| unsafe {
        IsWindow(Some(HWND(*h as *mut core::ffi::c_void))).as_bool()
    });
    origins.push((handle.raw(), normal));
}

fn forget(handle: Handle) -> Option<RECT> {
    let mut origins = ORIGINS.lock().ok()?;
    let at = origins.iter().position(|(h, _)| *h == handle.raw())?;
    Some(origins.remove(at).1)
}

/// Where a window is on screen, as you see it. `None` when there is nothing to
/// point at: it closed, or it is minimized.
pub fn visible_frame(handle: Handle) -> Option<Rect> {
    let hwnd = handle.hwnd();
    // SAFETY: both read flags off a handle; a dead one answers false.
    unsafe {
        if !IsWindow(Some(hwnd)).as_bool() || IsIconic(hwnd).as_bool() {
            return None;
        }
    }
    frames(hwnd).map(|(_, visible)| visible)
}

/// Every work area, and which one this window is on.
fn area_of(hwnd: HWND) -> Option<(Vec<Rect>, usize)> {
    let areas = work_areas();
    if areas.is_empty() {
        return None;
    }
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: MONITOR_DEFAULTTONEAREST always answers for a live hwnd.
    let here: Rect = unsafe {
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            log_warn!("could not read the monitor a window is on");
            return None;
        }
        info.rcWork.into()
    };
    let index = areas.iter().position(|area| *area == here).unwrap_or(0);
    Some((areas, index))
}

/// Change a window's show state without activating it.
///
/// `ShowWindow` foregrounds whatever it restores or maximizes, and the panel
/// dismisses on losing focus, so the first click on the bar would close the bar.
/// `SetWindowPlacement` reaches the same states and the no-activate show
/// commands leave focus alone.
fn show(hwnd: HWND, cmd: SHOW_WINDOW_CMD) -> bool {
    let mut placement = WINDOWPLACEMENT {
        length: size_of::<WINDOWPLACEMENT>() as u32,
        ..Default::default()
    };
    // SAFETY: the placement outlives both calls and its length is set.
    unsafe {
        if GetWindowPlacement(hwnd, &mut placement).is_err() {
            return false;
        }
        placement.showCmd = cmd.0 as u32;
        SetWindowPlacement(hwnd, &placement).is_ok()
    }
}

/// SAFETY on both: read a flag off a handle already checked to be live.
fn maximized(hwnd: HWND) -> bool {
    unsafe { IsZoomed(hwnd).as_bool() }
}

fn minimized(hwnd: HWND) -> bool {
    unsafe { IsIconic(hwnd).as_bool() }
}

/// Up and down the show state, the way `Win+Up` and `Win+Down` go.
fn step(handle: Handle, hwnd: HWND, mv: Move) -> Option<bool> {
    match mv {
        Move::Up => Some(show(hwnd, SW_SHOWMAXIMIZED)),
        Move::Down => Some(unwind(handle, hwnd)),
        _ => None,
    }
}

/// One rung down: maximized or snapped goes back to where the window was before
/// bentopick touched it, and a window already there minimizes.
///
/// The middle rung is the one Windows loses once something calls
/// `SetWindowPos`. Restoring through the placement rather than by rect keeps it
/// in the workspace coordinates `rcNormalPosition` is written in.
fn unwind(handle: Handle, hwnd: HWND) -> bool {
    let Some(normal) = forget(handle) else {
        return match maximized(hwnd) {
            true => show(hwnd, SW_SHOWNOACTIVATE),
            false => show(hwnd, SW_SHOWMINNOACTIVE),
        };
    };

    let mut placement = WINDOWPLACEMENT {
        length: size_of::<WINDOWPLACEMENT>() as u32,
        ..Default::default()
    };
    // SAFETY: the placement outlives both calls and its length is set.
    unsafe {
        if GetWindowPlacement(hwnd, &mut placement).is_err() {
            return false;
        }
        placement.showCmd = SW_SHOWNOACTIVATE.0 as u32;
        placement.rcNormalPosition = normal;
        SetWindowPlacement(hwnd, &placement).is_ok()
    }
}

/// The rect the window would restore to, read before anything overwrites it.
fn normal_position(hwnd: HWND) -> Option<RECT> {
    let mut placement = WINDOWPLACEMENT {
        length: size_of::<WINDOWPLACEMENT>() as u32,
        ..Default::default()
    };
    // SAFETY: the placement outlives the call and its length is set.
    unsafe {
        GetWindowPlacement(hwnd, &mut placement)
            .is_ok()
            .then_some(placement.rcNormalPosition)
    }
}

/// Put a window somewhere, frame inset and all.
///
/// A window rect includes the invisible resize border DWM leaves around it,
/// about 7px a side. Placing by that rect leaves a snapped window looking short
/// of the screen edge, so the arithmetic runs on the visible frame and the
/// difference goes back on here.
fn put(hwnd: HWND, target: Rect, outer: Rect, visible: Rect) -> bool {
    let left = target.left - (visible.left - outer.left);
    let top = target.top - (visible.top - outer.top);
    let w = target.w() + (outer.w() - visible.w());
    let h = target.h() + (outer.h() - visible.h());
    // SAFETY: no z-order and no activation, so the panel keeps focus and stays
    // in front while a run of these is clicked.
    unsafe { SetWindowPos(hwnd, None, left, top, w, h, SWP_NOZORDER | SWP_NOACTIVATE).is_ok() }
}

/// Move one window. Says whether it went anywhere.
pub fn apply(handle: Handle, mv: Move, title: &str, dry_run: bool) -> bool {
    let hwnd = handle.hwnd();
    // SAFETY: a window closed between the summon and this click fails here
    // rather than being moved.
    unsafe {
        if !IsWindow(Some(hwnd)).as_bool() {
            log_warn!("window \"{title}\" closed before it could be moved");
            return false;
        }
    }

    if dry_run {
        log_dry!("would move \"{title}\" {}", mv.key());
        return true;
    }

    // Before anything writes over it. Down is the one move that reads this
    // rather than adding to it, so it must not claim the rect it restores.
    if !matches!(mv, Move::Down)
        && let Some(normal) = normal_position(hwnd)
    {
        remember(handle, normal);
    }

    if let Some(stepped) = step(handle, hwnd, mv) {
        if stepped {
            log_info!("moved \"{title}\" {}", mv.key());
        } else {
            log_warn!("the OS declined to move \"{title}\" {}", mv.key());
        }
        return stepped;
    }

    // The rect has to come off a restored window: a maximized one reports the
    // whole screen, and a minimized one reports nothing useful.
    if maximized(hwnd) || minimized(hwnd) {
        show(hwnd, SW_SHOWNOACTIVATE);
    }

    let Some((outer, visible)) = frames(hwnd) else {
        log_warn!("could not read the frame of \"{title}\"");
        return false;
    };
    let Some((areas, index)) = area_of(hwnd) else {
        return false;
    };
    let area = areas[index];

    let target = match placed(area, mv) {
        Some(rect) => rect,
        None => match neighbour(&areas, index, mv) {
            Some(next) => across(area, areas[next], visible),
            None => {
                log_info!("no screen {} of \"{title}\"", mv.key());
                return false;
            }
        },
    };

    let moved = put(hwnd, target, outer, visible);
    if moved {
        log_info!("moved \"{title}\" {}", mv.key());
    } else {
        log_warn!("the OS declined to move \"{title}\" {}", mv.key());
    }
    moved
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Rect = Rect { left: 0, top: 0, right: 1920, bottom: 1040 };

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> Rect {
        Rect { left, top, right, bottom }
    }

    #[test]
    fn a_side_takes_half_the_width_and_the_whole_height() {
        assert_eq!(placed(SCREEN, Move::Left), Some(rect(0, 0, 960, 1040)));
        assert_eq!(placed(SCREEN, Move::Right), Some(rect(960, 0, 1920, 1040)));
    }

    #[test]
    fn the_two_halves_meet_and_lose_no_pixels() {
        let odd = rect(0, 0, 1921, 1040);
        let left = placed(odd, Move::Left).unwrap();
        let right = placed(odd, Move::Right).unwrap();
        assert_eq!(left.right, right.left, "a gap down the middle");
        assert_eq!(right.right, odd.right, "the right edge went missing");
    }

    #[test]
    fn a_side_move_ignores_where_the_window_was() {
        // Left is left, whatever shape it was in. Windows works this way and a
        // run of these has to be predictable.
        assert_eq!(placed(SCREEN, Move::Left), placed(SCREEN, Move::Left));
    }

    #[test]
    fn only_the_sides_are_a_rect() {
        // Up and down change the show state, and the hops need two work areas.
        for mv in [Move::Up, Move::Down, Move::ScreenLeft, Move::ScreenRight] {
            assert_eq!(placed(SCREEN, mv), None, "{}", mv.key());
        }
    }

    #[test]
    fn crossing_screens_keeps_the_share_of_the_screen() {
        let big = rect(1920, 0, 4480, 1440);
        let left_half = placed(SCREEN, Move::Left).unwrap();
        assert_eq!(across(SCREEN, big, left_half), rect(1920, 0, 3200, 1440));
    }

    #[test]
    fn the_end_of_the_row_does_not_wrap() {
        let areas = [SCREEN, rect(1920, 0, 3840, 1040)];
        assert_eq!(neighbour(&areas, 0, Move::ScreenRight), Some(1));
        assert_eq!(neighbour(&areas, 0, Move::ScreenLeft), None);
        assert_eq!(neighbour(&areas, 1, Move::ScreenRight), None);
        assert_eq!(neighbour(&areas, 1, Move::ScreenLeft), Some(0));
    }

    #[test]
    fn only_the_hops_have_a_neighbour() {
        let areas = [SCREEN, rect(1920, 0, 3840, 1040)];
        for mv in [Move::Left, Move::Right, Move::Up, Move::Down] {
            assert_eq!(neighbour(&areas, 0, mv), None, "{}", mv.key());
        }
    }
}
