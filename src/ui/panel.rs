//! The panel window and its composition tree.
//!
//! Unpackaged Win32 hosting of Windows.UI.Composition needs two things beyond
//! `Compositor::new()`: a dispatcher queue on this thread, and a
//! `DesktopWindowTarget` from `ICompositorDesktopInterop`. Both are set up in
//! `Panel::create`.
//!
//! The window is `WS_EX_NOREDIRECTIONBITMAP` so there is no GDI redirection
//! surface fighting the composition tree for per-pixel alpha.

use windows_numerics::{Vector2, Vector3};
use windows::UI::Color;
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{
    CompositionColorBrush, CompositionDrawingSurface, CompositionSpriteShape, Compositor,
    ContainerVisual, ShapeVisual,
};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    HBRUSH, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint, GetMonitorInfoW,
};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::Win32::System::WinRT::{
    CreateDispatcherQueueController, DQTAT_COM_NONE, DQTYPE_THREAD_CURRENT, DispatcherQueueOptions,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
// WM_MOUSELEAVE lives in Controls, not WindowsAndMessaging.
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, SetActiveWindow, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, UnregisterHotKey,
    VK_ESCAPE,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{Interface, PCWSTR, Result, w};

use crate::config::{self, Config};
use crate::model::Item;
use crate::model::store;
use crate::safety;
use crate::shell::icons;
use crate::ui::grid::{Layout, Metrics, Rect as GridRect};
use crate::ui::render::{Renderer, TextColors, TilePaint, d2d_color};
use crate::ui::tray;
use crate::{log_dry, log_error, log_info, log_warn};

const HOTKEY_ID: i32 = 1;
/// Drives the watchdog heartbeat while the panel is up.
const HEARTBEAT_TIMER: usize = 1;
const HEARTBEAT_MS: u32 = 250;

/// One tile's visuals. The brush is held so hover is a colour write rather than
/// a walk back down the visual tree.
struct Tile {
    root: ContainerVisual,
    brush: CompositionColorBrush,
    /// Where the icon and label are drawn. `None` if the renderer is missing, in
    /// which case the tile is a bare rectangle rather than nothing at all.
    surface: Option<CompositionDrawingSurface>,
    /// This tile wants an icon that has not arrived yet.
    awaiting_icon: bool,
}

pub struct Panel {
    hwnd: HWND,
    compositor: Compositor,
    _target: DesktopWindowTarget,
    /// Everything under here is rebuilt each time the panel is shown.
    content: ContainerVisual,
    /// Kept alive for the lifetime of the thread; dropping it tears down the
    /// dispatcher queue the compositor depends on.
    _dispatcher: windows::System::DispatcherQueueController,

    /// `None` if D3D/D2D could not start. flick still runs; tiles just lose
    /// their icons and labels, which beats refusing to launch.
    renderer: Option<Renderer>,

    config: Config,
    items: Vec<Item>,
    layout: Layout,
    scroll: f32,
    hover: Option<usize>,
    tiles: Vec<Tile>,
    visible: bool,
    /// Whether a `TrackMouseEvent` request is outstanding. Without one,
    /// WM_MOUSELEAVE never arrives and hover sticks on the last tile.
    tracking_mouse: bool,
    /// Foreground window at show time, so Esc can put it back.
    caller: HWND,
    hotkey_bound: bool,
}

impl Panel {
    pub fn create(config: Config) -> Result<Box<Panel>> {
        // SAFETY: standard apartment init for a UI thread; the composition
        // stack requires an initialized apartment before the queue exists.
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        }

        let hwnd = unsafe { create_window()? };

        // SAFETY: DQTYPE_THREAD_CURRENT binds the queue to this thread, which is
        // the thread that will own every composition object below.
        let dispatcher = unsafe {
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_NONE,
            })?
        };

        let compositor = Compositor::new()?;
        let interop: ICompositorDesktopInterop = compositor.cast()?;
        // SAFETY: hwnd is a valid top-level window owned by this thread.
        let target: DesktopWindowTarget =
            unsafe { interop.CreateDesktopWindowTarget(hwnd, false)? };

        let root = compositor.CreateContainerVisual()?;
        root.SetRelativeSizeAdjustment(Vector2 { X: 1.0, Y: 1.0 })?;
        target.SetRoot(&root)?;

        let content = compositor.CreateContainerVisual()?;
        content.SetRelativeSizeAdjustment(Vector2 { X: 1.0, Y: 1.0 })?;
        root.Children()?.InsertAtTop(&content)?;

        let renderer = match Renderer::new(&compositor) {
            Ok(renderer) => Some(renderer),
            Err(e) => {
                log_error!("no Direct2D renderer ({e}); tiles will have no icons or labels");
                None
            }
        };

        let mut panel = Box::new(Panel {
            hwnd,
            compositor,
            _target: target,
            content,
            _dispatcher: dispatcher,
            renderer,
            config,
            items: Vec::new(),
            layout: Layout::compute(0, Metrics {
                tile_w: 1.0, tile_h: 1.0, gap: 0.0, padding: 0.0, max_fraction: 0.8,
            }, GridRect { x: 0.0, y: 0.0, w: 1.0, h: 1.0 }),
            scroll: 0.0,
            hover: None,
            tiles: Vec::new(),
            visible: false,
            tracking_mouse: false,
            caller: HWND(std::ptr::null_mut()),
            hotkey_bound: false,
        });

        // Hand the window a back-pointer so the wndproc can find us. Messages
        // that arrive before this point fall through to DefWindowProcW.
        // SAFETY: the Box outlives the window; main drops it after the loop ends.
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, panel.as_mut() as *mut Panel as isize);
        }

        safety::register_window(hwnd);
        panel.bind_hotkey();
        Ok(panel)
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    fn bind_hotkey(&mut self) {
        let Some(hk) = config::parse_hotkey(&self.config.hotkey) else {
            log_error!(
                "hotkey '{}' could not be parsed; flick has no way to be summoned",
                self.config.hotkey
            );
            return;
        };
        // SAFETY: process-scoped registration, released by the OS on exit even
        // if we crash (safety rule 4 — this is why it is not a keyboard hook).
        match unsafe { RegisterHotKey(Some(self.hwnd), HOTKEY_ID, hk.modifiers, hk.vk) } {
            Ok(()) => {
                self.hotkey_bound = true;
                log_info!("hotkey bound: {}", self.config.hotkey);
            }
            Err(e) => log_error!(
                "could not bind hotkey '{}' ({e}); another app likely owns it",
                self.config.hotkey
            ),
        }
    }

    /// Config sizes are logical pixels at 96 DPI; the window and its visuals are
    /// physical.
    fn scale(&self) -> f32 {
        // SAFETY: hwnd is valid for the panel's lifetime.
        let dpi = unsafe { GetDpiForWindow(self.hwnd) };
        if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 }
    }

    fn metrics(&self) -> Metrics {
        let scale = self.scale();
        let g = &self.config.grid;
        Metrics {
            tile_w: g.tile_width * scale,
            tile_h: g.tile_height * scale,
            gap: g.gap * scale,
            padding: g.padding * scale,
            max_fraction: g.max_screen_fraction,
        }
    }

    pub fn toggle(&mut self) {
        if self.visible {
            self.hide(true);
        } else {
            self.show();
        }
    }

    pub fn show(&mut self) {
        if safety::is_neutralized() {
            log_warn!("refusing to show: the panel was neutralized after a fault");
            return;
        }

        // SAFETY: plain queries about current system state.
        self.caller = unsafe { GetForegroundWindow() };
        self.items = store::items();
        let work = work_area();
        self.layout = Layout::compute(self.items.len(), self.metrics(), work);
        self.scroll = 0.0;
        self.hover = None;

        let p = self.layout.panel;
        log_info!(
            "show: {} items, {}x{} grid, panel {}x{} at {},{}{}",
            self.items.len(),
            self.layout.cols,
            self.layout.rows,
            p.w as i32,
            p.h as i32,
            p.x as i32,
            p.y as i32,
            if self.layout.max_scroll > 0.0 { " (scrolls)" } else { "" }
        );

        if let Err(e) = self.rebuild_visuals() {
            log_error!("could not build the grid visuals: {e}");
            return;
        }

        // SAFETY: standard show sequence. SetForegroundWindow is permitted here
        // because WM_HOTKEY made us the last process to receive input.
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                p.x as i32,
                p.y as i32,
                p.w as i32,
                p.h as i32,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            let _ = SetForegroundWindow(self.hwnd);
            let _ = SetActiveWindow(self.hwnd);
            SetTimer(Some(self.hwnd), HEARTBEAT_TIMER, HEARTBEAT_MS, None);
        }

        self.visible = true;
        safety::mark_shown(true);
    }

    pub fn hide(&mut self, restore_caller: bool) {
        if !self.visible {
            return;
        }
        self.visible = false;
        self.tracking_mouse = false;
        safety::mark_shown(false);

        // SAFETY: our own window, on our own thread.
        unsafe {
            let _ = KillTimer(Some(self.hwnd), HEARTBEAT_TIMER);
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }

        // Restoring the caller is flick undoing its own activation, not acting
        // on a target, so it happens in dry run too.
        if restore_caller && !self.caller.is_invalid() && self.caller != self.hwnd {
            // SAFETY: a stale hwnd makes this fail harmlessly.
            let restored = unsafe { SetForegroundWindow(self.caller) };
            log_info!(
                "restoring caller {:#x}: {}",
                self.caller.0 as isize,
                if restored.as_bool() { "ok" } else { "declined by the OS" }
            );
        }

        // Drop the visual tree so hidden panels hold no GPU memory.
        if let Ok(children) = self.content.Children() {
            let _ = children.RemoveAll();
        }
        self.tiles.clear();
        self.items.clear();
        log_info!("hidden");
    }

    fn rebuild_visuals(&mut self) -> Result<()> {
        let children = self.content.Children()?;
        children.RemoveAll()?;
        self.tiles.clear();

        let p = self.layout.panel;
        let radius = self.config.grid.corner_radius;

        let (backdrop, _) = self.rounded_rect(
            Vector2 { X: p.w, Y: p.h },
            radius * 1.5,
            color_of(&self.config.theme.panel),
        )?;
        children.InsertAtTop(&backdrop)?;

        let tile_color = color_of(&self.config.theme.tile);
        let mut built = Vec::with_capacity(self.items.len());

        for (index, item) in self.items.iter().enumerate() {
            let rect = self.layout.tile_rect(index, self.scroll);
            let root = self.compositor.CreateContainerVisual()?;
            root.SetSize(Vector2 { X: rect.w, Y: rect.h })?;
            root.SetOffset(Vector3 { X: rect.x, Y: rect.y, Z: 0.0 })?;

            let (face, brush) =
                self.rounded_rect(Vector2 { X: rect.w, Y: rect.h }, radius, tile_color)?;
            root.Children()?.InsertAtTop(&face)?;

            let mut surface = None;
            let mut awaiting_icon = false;

            if let Some(renderer) = &self.renderer {
                match renderer.create_surface(rect.w, rect.h) {
                    Ok(drawn) => {
                        let icon = self.icon_for(item);
                        awaiting_icon = icon.is_none() && item.icon_source.is_some();
                        self.paint_tile(renderer, &drawn, rect.w, rect.h, item, icon.as_deref());

                        let sprite = self.compositor.CreateSpriteVisual()?;
                        sprite.SetSize(Vector2 { X: rect.w, Y: rect.h })?;
                        sprite.SetBrush(
                            &self.compositor.CreateSurfaceBrushWithSurface(&drawn)?,
                        )?;
                        root.Children()?.InsertAtTop(&sprite)?;
                        surface = Some(drawn);
                    }
                    Err(e) => log_warn!("could not create a drawing surface for a tile: {e}"),
                }
            }

            children.InsertAtTop(&root)?;
            built.push(Tile { root, brush, surface, awaiting_icon });
        }

        self.tiles = built;
        Ok(())
    }

    /// Icon size in physical pixels: big enough to fill the tile's image area
    /// without asking the shell for more than it will be shown at.
    fn icon_size(&self) -> u32 {
        let g = &self.config.grid;
        let area = (g.tile_height - g.label_height).max(16.0) * self.scale();
        (area * 0.6).clamp(32.0, 256.0) as u32
    }

    fn icon_for(&self, item: &Item) -> Option<std::sync::Arc<icons::IconPixels>> {
        // Never blocks: returns None and queues if the icon is not cached yet.
        icons::request(item.icon_source.as_ref()?, self.icon_size())
    }

    fn paint_tile(
        &self,
        renderer: &Renderer,
        surface: &CompositionDrawingSurface,
        w: f32,
        h: f32,
        item: &Item,
        icon: Option<&icons::IconPixels>,
    ) {
        let colors = TextColors {
            title: d2d_color(&self.config.theme.text),
            detail: dim(d2d_color(&self.config.theme.text)),
        };
        let paint = TilePaint {
            width: w,
            height: h,
            label_height: self.config.grid.label_height * self.scale(),
            title: &item.title,
            detail: &item.detail,
            icon,
            colors,
        };
        if let Err(e) = renderer.draw_tile(surface, paint) {
            log_warn!("could not draw tile \"{}\": {e}", item.title);
        }
    }

    /// Repaint only the tiles still waiting on an icon.
    fn on_icons_ready(&mut self) {
        if !self.visible {
            return;
        }
        let Some(renderer) = &self.renderer else { return };

        // Resolved once: the loop below borrows `self.tiles` mutably, so it
        // cannot call back into `&self` helpers.
        let icon_size = self.icon_size();
        let label_height = self.config.grid.label_height * self.scale();
        let colors = TextColors {
            title: d2d_color(&self.config.theme.text),
            detail: dim(d2d_color(&self.config.theme.text)),
        };

        let mut filled = 0;
        for (index, tile) in self.tiles.iter_mut().enumerate() {
            if !tile.awaiting_icon {
                continue;
            }
            let (Some(surface), Some(item)) = (tile.surface.as_ref(), self.items.get(index)) else {
                continue;
            };
            let Some(source) = item.icon_source.as_ref() else {
                tile.awaiting_icon = false;
                continue;
            };
            let Some(icon) = icons::request(source, icon_size) else {
                continue;
            };

            let rect = self.layout.tile_rect(index, self.scroll);
            let paint = TilePaint {
                width: rect.w,
                height: rect.h,
                label_height,
                title: &item.title,
                detail: &item.detail,
                icon: Some(&icon),
                colors,
            };
            if renderer.draw_tile(surface, paint).is_ok() {
                tile.awaiting_icon = false;
                filled += 1;
            }
        }
        if filled > 0 {
            log_info!("{filled} icon(s) painted");
        }
    }

    fn rounded_rect(
        &self,
        size: Vector2,
        radius: f32,
        color: Color,
    ) -> Result<(ShapeVisual, CompositionColorBrush)> {
        let geometry = self.compositor.CreateRoundedRectangleGeometry()?;
        geometry.SetSize(size)?;
        geometry.SetCornerRadius(Vector2 { X: radius, Y: radius })?;

        let brush = self.compositor.CreateColorBrushWithColor(color)?;
        let shape: CompositionSpriteShape =
            self.compositor.CreateSpriteShapeWithGeometry(&geometry)?;
        shape.SetFillBrush(&brush)?;

        let visual = self.compositor.CreateShapeVisual()?;
        visual.SetSize(size)?;
        visual.Shapes()?.Append(&shape)?;
        Ok((visual, brush))
    }

    /// Repositions existing tiles after a scroll, without rebuilding them.
    fn reposition_tiles(&self) {
        for (index, tile) in self.tiles.iter().enumerate() {
            let rect = self.layout.tile_rect(index, self.scroll);
            let _ = tile.root.SetOffset(Vector3 { X: rect.x, Y: rect.y, Z: 0.0 });
        }
    }

    /// Ask for one WM_MOUSELEAVE. The request is consumed when it fires, so it
    /// is re-armed on the next move.
    fn track_mouse_leave(&mut self) {
        if self.tracking_mouse {
            return;
        }
        let mut track = TRACKMOUSEEVENT {
            cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: self.hwnd,
            dwHoverTime: 0,
        };
        // SAFETY: `track` is fully initialized and outlives the call.
        if unsafe { TrackMouseEvent(&mut track) }.is_ok() {
            self.tracking_mouse = true;
        }
    }

    fn set_hover(&mut self, index: Option<usize>) {
        if self.hover == index {
            return;
        }
        let normal = color_of(&self.config.theme.tile);
        let hot = color_of(&self.config.theme.tile_hover);
        for (slot, want) in [(self.hover, normal), (index, hot)] {
            if let Some(i) = slot
                && let Some(tile) = self.tiles.get(i)
            {
                let _ = tile.brush.SetColor(want);
            }
        }
        self.hover = index;
    }

    fn scroll_by(&mut self, delta: f32) {
        if self.layout.max_scroll <= 0.0 {
            return;
        }
        let next = self.layout.clamp_scroll(self.scroll - delta);
        if (next - self.scroll).abs() < 0.5 {
            return;
        }
        self.scroll = next;
        self.reposition_tiles();
    }

    /// Milestone 1: log what would have happened, activate nothing.
    fn activate(&mut self, index: usize) {
        let Some(item) = self.items.get(index).cloned() else {
            return;
        };
        if self.config.dry_run {
            log_dry!("would {}", item.activation_summary());
            self.hide(true);
            return;
        }
        // Milestone 2 replaces this.
        log_warn!(
            "dry_run is off but activation is not implemented yet; would {}",
            item.activation_summary()
        );
        self.hide(true);
    }

    fn on_model_changed(&mut self) {
        if !self.visible {
            return;
        }
        let previous = self.hover.and_then(|i| self.items.get(i)).map(|i| i.id.clone());
        self.items = store::items();
        self.layout = Layout::compute(self.items.len(), self.metrics(), work_area());
        self.scroll = self.layout.clamp_scroll(self.scroll);

        let p = self.layout.panel;
        // SAFETY: our own window; SWP_NOACTIVATE keeps focus where it is.
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                p.x as i32,
                p.y as i32,
                p.w as i32,
                p.h as i32,
                SWP_NOACTIVATE,
            );
        }

        self.hover = None;
        if let Err(e) = self.rebuild_visuals() {
            log_error!("could not rebuild the grid: {e}");
            return;
        }
        // Follow the hovered item to its new position rather than whatever
        // landed under the cursor's old index.
        if let Some(id) = previous {
            let moved = self.items.iter().position(|i| i.id == id);
            self.set_hover(moved);
        }
    }

    fn cursor_index(&self, lparam: LPARAM) -> Option<usize> {
        let x = (lparam.0 & 0xFFFF) as i16 as f32;
        let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32;
        self.layout.hit_test(x, y, self.scroll, self.items.len())
    }

    fn handle(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_HOTKEY if wparam.0 as i32 == HOTKEY_ID => {
                self.toggle();
                Some(LRESULT(0))
            }
            WM_TIMER => {
                safety::beat();
                Some(LRESULT(0))
            }
            tray::WM_TRAY => {
                match tray::classify(wparam, lparam) {
                    tray::Click::Left => self.toggle(),
                    tray::Click::Right => match tray::show_menu(self.hwnd) {
                        Some(tray::CMD_TOGGLE) => self.toggle(),
                        Some(tray::CMD_EXIT) => {
                            log_info!("exit requested from the tray menu");
                            self.hide(false);
                            // SAFETY: our own window, on its owning thread.
                            unsafe {
                                let _ = DestroyWindow(self.hwnd);
                            }
                        }
                        _ => {}
                    },
                    tray::Click::Other => {}
                }
                Some(LRESULT(0))
            }
            store::WM_MODEL_CHANGED => {
                self.on_model_changed();
                Some(LRESULT(0))
            }
            icons::WM_ICON_READY => {
                self.on_icons_ready();
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                self.track_mouse_leave();
                let index = self.cursor_index(lparam);
                self.set_hover(index);
                Some(LRESULT(0))
            }
            WM_MOUSELEAVE => {
                self.tracking_mouse = false;
                self.set_hover(None);
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                match self.cursor_index(lparam) {
                    Some(index) => self.activate(index),
                    // A click on the panel's own padding dismisses, matching the
                    // click-outside behaviour.
                    None => self.hide(true),
                }
                Some(LRESULT(0))
            }
            WM_MOUSEWHEEL => {
                let delta = ((wparam.0 >> 16) & 0xFFFF) as i16 as f32;
                self.scroll_by(delta);
                Some(LRESULT(0))
            }
            WM_KEYDOWN if wparam.0 as u32 == VK_ESCAPE.0 as u32 => {
                self.hide(true);
                Some(LRESULT(0))
            }
            // Clicking away, or anything else stealing focus, dismisses.
            WM_ACTIVATE if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE => {
                self.hide(false);
                Some(LRESULT(0))
            }
            WM_DISPLAYCHANGE | WM_DPICHANGED if self.visible => {
                self.on_model_changed();
                Some(LRESULT(0))
            }
            _ => None,
        }
    }
}

impl Drop for Panel {
    fn drop(&mut self) {
        if self.hotkey_bound {
            // SAFETY: matches the successful RegisterHotKey in bind_hotkey.
            unsafe {
                let _ = UnregisterHotKey(Some(self.hwnd), HOTKEY_ID);
            }
        }
    }
}

/// The detail line sits under the title; same hue, less presence.
fn dim(mut c: windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F) -> windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F {
    c.a *= 0.6;
    c
}

fn color_of(spec: &str) -> Color {
    let (a, r, g, b) = config::parse_color(spec);
    Color { A: a, R: r, G: g, B: b }
}

/// Work area of the monitor under the cursor — the panel should open where the
/// user is looking, not on the primary display.
fn work_area() -> GridRect {
    // SAFETY: all out-params are stack locals sized by the API's own contract.
    unsafe {
        let mut point = POINT::default();
        let _ = GetCursorPos(&mut point);
        let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTOPRIMARY);

        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            return rect_to_grid(info.rcWork);
        }

        log_warn!("GetMonitorInfoW failed; falling back to the virtual screen size");
        GridRect {
            x: 0.0,
            y: 0.0,
            w: GetSystemMetrics(SM_CXSCREEN) as f32,
            h: GetSystemMetrics(SM_CYSCREEN) as f32,
        }
    }
}

fn rect_to_grid(r: RECT) -> GridRect {
    GridRect {
        x: r.left as f32,
        y: r.top as f32,
        w: (r.right - r.left) as f32,
        h: (r.bottom - r.top) as f32,
    }
}

const CLASS_NAME: PCWSTR = w!("flick_panel");

unsafe fn create_window() -> Result<HWND> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: CLASS_NAME,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hbrBackground: HBRUSH(std::ptr::null_mut()),
            ..Default::default()
        };
        if RegisterClassExW(&class) == 0 {
            return Err(windows::core::Error::from_thread());
        }

        // WS_EX_NOREDIRECTIONBITMAP: no GDI redirection surface, so the
        // composition tree owns every pixel including alpha.
        // WS_EX_TOOLWINDOW: keeps flick out of alt-tab and the taskbar.
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOREDIRECTIONBITMAP,
            CLASS_NAME,
            w!("flick"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )
    }
}

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: GWLP_USERDATA holds the Panel pointer installed in Panel::create,
    // or null before that. The Panel outlives the window.
    let panel = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Panel };

    if !panel.is_null()
        && let Some(result) = unsafe { (*panel).handle(msg, wparam, lparam) }
    {
        return result;
    }

    if msg == WM_DESTROY {
        // SAFETY: ends the message loop in main.
        unsafe { PostQuitMessage(0) };
        return LRESULT(0);
    }

    // SAFETY: standard fallback for every message we do not claim.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}
