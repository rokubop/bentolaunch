//! The panel window and its composition tree.
//!
//! Unpackaged Win32 hosting of Windows.UI.Composition needs two things beyond
//! `Compositor::new()`: a dispatcher queue on this thread, and a
//! `DesktopWindowTarget` from `ICompositorDesktopInterop`. Both are set up in
//! `Panel::create`.
//!
//! The window is `WS_EX_NOREDIRECTIONBITMAP` so there is no GDI redirection
//! surface fighting the composition tree for per-pixel alpha.

use windows::UI::Color;
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{
    CompositionColorBrush, CompositionDrawingSurface, CompositionSpriteShape, Compositor,
    ContainerVisual, ShapeVisual, SpriteVisual,
};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, HBRUSH, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::{IDropTarget, OleInitialize};
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::Win32::System::WinRT::{
    CreateDispatcherQueueController, DQTAT_COM_NONE, DQTYPE_THREAD_CURRENT, DispatcherQueueOptions,
};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, ReleaseCapture, SetActiveWindow, SetCapture, TME_LEAVE, TRACKMOUSEEVENT,
    TrackMouseEvent, UnregisterHotKey, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME, VK_LEFT, VK_RETURN,
    VK_RIGHT, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{Interface, PCWSTR, Result, w};
use windows_numerics::{Vector2, Vector3};

use crate::config::{self, Config, Source};
use crate::model::store;
use crate::model::{Item, Section, Target};
use crate::safety;
use crate::shell::{activate, icons, picker};
use crate::ui::dropzone;
use crate::ui::filter;
use crate::ui::grid::{Layout, Metrics, Rect as GridRect, SectionShape, reordered};
use crate::ui::menu;
use crate::ui::render::{PIN_GLYPH, Renderer, TextColors, TilePaint, d2d_color};
use crate::ui::tray;
use crate::{pins, watch};
use crate::{log_dry, log_error, log_info, log_warn};

const HOTKEY_ID: i32 = 1;
/// Drives the watchdog heartbeat while the panel is up.
const HEARTBEAT_TIMER: usize = 1;
const HEARTBEAT_MS: u32 = 250;
/// A press that never travels this far is a click, not a drag.
///
/// Taken from the shell rather than picked, so flick's idea of "that was a
/// drag" is the same as every other window's on this machine. This is what
/// makes an explicit edit mode unnecessary: a 3px wobble activates, a real drag
/// rearranges, and the two are never confused.
fn drag_slop() -> (f32, f32) {
    // SAFETY: plain system metric reads.
    unsafe {
        (
            GetSystemMetrics(SM_CXDRAG).max(2) as f32,
            GetSystemMetrics(SM_CYDRAG).max(2) as f32,
        )
    }
}

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
    /// Sections as shown, empty ones already dropped by the store.
    sections: Vec<Section>,
    /// Every item flattened in section order. Tile index == this index.
    items: Vec<Item>,
    /// Unfiltered count, for the strip's "3 of 47".
    total: usize,
    layout: Layout,
    scroll: f32,
    hover: Option<usize>,
    /// Independent of `hover`: cursor and keyboard may disagree.
    selected: Option<usize>,
    tiles: Vec<Tile>,
    /// Header visuals, in the same order as `layout.headers()`.
    headers: Vec<SpriteVisual>,
    /// The keep-open button: its pill's brush, and its glyph. `None` if the
    /// renderer is missing.
    chrome: Option<(CompositionColorBrush, SpriteVisual)>,
    /// Cursor is on the keep-open button.
    chrome_hot: bool,
    visible: bool,
    /// Whether a `TrackMouseEvent` request is outstanding. Without one,
    /// WM_MOUSELEAVE never arrives and hover sticks on the last tile.
    tracking_mouse: bool,
    /// Foreground window at show time, so Esc can put it back.
    caller: HWND,
    hotkey_bound: bool,

    query: String,
    /// Held for the query's duration, from the unfiltered grid. 0 when idle.
    frozen_cols: usize,

    /// Keep the panel up when it loses focus. Off by default — the panel's whole
    /// job is to get out of the way — but a drag that starts in Explorer takes
    /// focus away before there is any drag to react to, so dropping onto flick
    /// needs the panel pinned open first.
    keep_open: bool,
    /// A context menu is up. It does not deactivate us, but a stray dismissal
    /// while a menu is open would be baffling, so it is treated the same.
    menu_open: bool,
    /// A button is down on a tile. It becomes a drag past the slop threshold and
    /// an activation if it never gets there.
    press: Option<Press>,
    /// Section highlighted for a drop that is still in flight.
    drop_band: Option<usize>,
    /// Held for the window's lifetime; dropping it would unregister the target.
    _drop_target: Option<IDropTarget>,
}

/// A pressed tile, which may still turn out to be either a click or a drag.
struct Press {
    /// Flat index of the tile under the press.
    tile: usize,
    /// Its section, if that section's order is flick's to rearrange.
    band: Option<usize>,
    /// Where in the tile it was pressed, so a drag does not jump to the cursor.
    grab: (f32, f32),
    start: (f32, f32),
    /// Past the slop threshold: no longer a click.
    dragging: bool,
    /// Insertion slot within the section the cursor is currently over.
    slot: usize,
}

impl Panel {
    pub fn create(config: Config) -> Result<Box<Panel>> {
        // SAFETY: apartment-threaded init for a UI thread, which is what the
        // composition stack needs before the dispatcher queue exists. The OLE
        // form rather than `CoInitializeEx` because `RegisterDragDrop` below
        // needs the drag-and-drop half initialized too.
        unsafe {
            OleInitialize(None)?;
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

        let placeholder = Metrics {
            tile_w: 1.0,
            tile_h: 1.0,
            gap: 0.0,
            padding: 0.0,
            max_fraction: 0.8,
            max_cols: 0,
            fixed_cols: 0,
            header_h: 0.0,
            section_gap: 0.0,
            search_h: 0.0,
        };

        let mut panel = Box::new(Panel {
            hwnd,
            compositor,
            _target: target,
            content,
            _dispatcher: dispatcher,
            renderer,
            config,
            sections: Vec::new(),
            items: Vec::new(),
            total: 0,
            layout: Layout::compute(
                &[],
                placeholder,
                GridRect { x: 0.0, y: 0.0, w: 1.0, h: 1.0 },
            ),
            scroll: 0.0,
            hover: None,
            selected: None,
            tiles: Vec::new(),
            headers: Vec::new(),
            chrome: None,
            chrome_hot: false,
            visible: false,
            tracking_mouse: false,
            caller: HWND(std::ptr::null_mut()),
            hotkey_bound: false,
            query: String::new(),
            frozen_cols: 0,
            keep_open: false,
            menu_open: false,
            press: None,
            drop_band: None,
            _drop_target: dropzone::register(hwnd),
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
            max_cols: g.max_columns,
            fixed_cols: self.frozen_cols,
            header_h: g.header_height * scale,
            section_gap: g.section_gap * scale,
            search_h: if self.query.is_empty() { 0.0 } else { g.search_height * scale },
        }
    }

    fn shapes(&self) -> Vec<SectionShape> {
        self.sections
            .iter()
            .map(|s| SectionShape { title: s.title.clone(), count: s.items.len() })
            .collect()
    }

    /// Pull the model, apply the query, recompute geometry.
    fn reload(&mut self) {
        let all = store::sections();
        self.total = all.iter().map(|s| s.items.len()).sum();
        let (sections, best) = self.filtered(all);
        self.sections = sections;
        self.selected = best;
        self.items = self
            .sections
            .iter()
            .flat_map(|s| s.items.iter().cloned())
            .collect();
        self.layout = Layout::compute(&self.shapes(), self.metrics(), work_area());
    }

    /// Emptied sections stay in the list: the layout skips them, and removing
    /// them would break the band-to-section mapping unpin resolves through.
    fn filtered(&self, sections: Vec<Section>) -> (Vec<Section>, Option<usize>) {
        if self.query.trim().is_empty() {
            return (sections, None);
        }

        let mut best: Option<(u32, usize)> = None;
        let mut chosen = None;
        let mut flat = 0usize;
        let mut out = Vec::with_capacity(sections.len());

        for section in sections {
            let mut kept = Vec::with_capacity(section.items.len());
            for item in section.items {
                let Some(score) = filter::score(&self.query, &item.title, &item.detail) else {
                    continue;
                };
                // Strict improvement to displace, so ties keep the tile
                // nearest the top.
                let length = item.title.chars().count();
                if best.is_none_or(|(top, len)| score > top || (score == top && length < len)) {
                    best = Some((score, length));
                    chosen = Some(flat);
                }
                flat += 1;
                kept.push(item);
            }
            out.push(Section { items: kept, ..section });
        }
        (out, chosen)
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

        // SAFETY: plain query about current system state.
        self.caller = unsafe { GetForegroundWindow() };
        // A query belongs to one summoning.
        self.query.clear();
        self.frozen_cols = 0;
        self.reload();
        self.scroll = 0.0;
        self.hover = None;

        let p = self.layout.panel;
        log_info!(
            "show: {} items in {} section(s), {} cols, panel {}x{} at {},{}{}",
            self.items.len(),
            self.sections.len(),
            self.layout.cols,
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
        // Keep-open lasts as long as the panel is on screen. A pin that outlives
        // the thing it pinned would leave the next summon behaving oddly with no
        // memory of why.
        self.keep_open = false;
        self.press = None;
        self.drop_band = None;
        self.query.clear();
        self.frozen_cols = 0;
        self.selected = None;
        safety::mark_shown(false);

        // SAFETY: our own window, on our own thread.
        unsafe {
            let _ = KillTimer(Some(self.hwnd), HEARTBEAT_TIMER);
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }

        // Restoring the caller is flick undoing its own activation, not acting
        // on a target. Skipped when we are about to activate something else.
        if restore_caller && !self.caller.is_invalid() && self.caller != self.hwnd {
            // SAFETY: a stale hwnd makes this fail harmlessly.
            unsafe {
                let _ = SetForegroundWindow(self.caller);
            }
        }

        // Drop the visual tree so hidden panels hold no GPU memory.
        if let Ok(children) = self.content.Children() {
            let _ = children.RemoveAll();
        }
        self.tiles.clear();
        self.headers.clear();
        self.chrome = None;
        self.chrome_hot = false;
        self.items.clear();
        self.sections.clear();
    }

    fn rebuild_visuals(&mut self) -> Result<()> {
        let children = self.content.Children()?;
        children.RemoveAll()?;
        self.tiles.clear();
        self.headers.clear();
        // `chrome_hot` deliberately survives: a rebuild under a resting cursor
        // should not make the button forget it is being pointed at.
        self.chrome = None;

        let p = self.layout.panel;
        let scale = self.scale();
        let radius = self.config.grid.corner_radius * scale;

        let (backdrop, _) = self.rounded_rect(
            Vector2 { X: p.w, Y: p.h },
            radius * 1.5,
            color_of(&self.config.theme.panel),
        )?;
        children.InsertAtTop(&backdrop)?;

        if let Some(renderer) = &self.renderer {
            let header_color = d2d_color(&self.config.theme.header);
            let mut built = Vec::new();
            for (title, rect) in self.layout.headers(self.scroll) {
                let surface = match renderer.create_surface(rect.w, rect.h) {
                    Ok(surface) => surface,
                    Err(e) => {
                        log_warn!("could not create a header surface: {e}");
                        continue;
                    }
                };
                if let Err(e) = renderer.draw_header(&surface, rect.w, rect.h, title, header_color) {
                    log_warn!("could not draw header \"{title}\": {e}");
                    continue;
                }
                let sprite = self.compositor.CreateSpriteVisual()?;
                sprite.SetSize(Vector2 { X: rect.w, Y: rect.h })?;
                sprite.SetOffset(Vector3 { X: rect.x, Y: rect.y, Z: 0.0 })?;
                sprite.SetBrush(&self.compositor.CreateSurfaceBrushWithSurface(&surface)?)?;
                children.InsertAtTop(&sprite)?;
                built.push(sprite);
            }
            self.headers = built;
        }

        let icon_size = self.icon_size();
        let label_height = self.config.grid.label_height * scale;
        let show_detail = self.config.grid.show_detail;
        let colors = self.text_colors();
        let mut built = Vec::with_capacity(self.items.len());

        for (index, item) in self.items.iter().enumerate() {
            let rect = self.layout.tile_rect(index, self.scroll);
            let root = self.compositor.CreateContainerVisual()?;
            root.SetSize(Vector2 { X: rect.w, Y: rect.h })?;
            root.SetOffset(Vector3 { X: rect.x, Y: rect.y, Z: 0.0 })?;

            let (face, brush) =
                self.rounded_rect(Vector2 { X: rect.w, Y: rect.h }, radius, self.tile_color(index))?;
            root.Children()?.InsertAtTop(&face)?;

            let mut surface = None;
            let mut awaiting_icon = false;

            if let Some(renderer) = &self.renderer {
                match renderer.create_surface(rect.w, rect.h) {
                    Ok(drawn) => {
                        // Never blocks: None means the shell worker is still busy.
                        let icon = item
                            .icon_source
                            .as_deref()
                            .and_then(|name| icons::request(name, icon_size));
                        awaiting_icon = icon.is_none() && item.icon_source.is_some();

                        let paint = TilePaint {
                            width: rect.w,
                            height: rect.h,
                            label_height,
                            title: &item.title,
                            detail: if show_detail { &item.detail } else { "" },
                            icon: icon.as_deref(),
                            colors,
                        };
                        if let Err(e) = renderer.draw_tile(&drawn, paint) {
                            log_warn!("could not draw tile \"{}\": {e}", item.title);
                        }

                        let sprite = self.compositor.CreateSpriteVisual()?;
                        sprite.SetSize(Vector2 { X: rect.w, Y: rect.h })?;
                        sprite.SetBrush(&self.compositor.CreateSurfaceBrushWithSurface(&drawn)?)?;
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
        // After the tiles, so the grid scrolls underneath them.
        self.build_search();
        self.build_chrome(radius);
        Ok(())
    }

    /// Nothing to build without a query, which is most of the time.
    fn build_search(&mut self) {
        let Some(renderer) = &self.renderer else { return };
        let rect = self.layout.search_rect();
        if self.query.is_empty() || rect.w < 32.0 || rect.h < 10.0 {
            return;
        }

        let built = (|| -> Result<()> {
            let surface = renderer.create_surface(rect.w, rect.h)?;
            renderer.draw_search(
                &surface,
                rect.w,
                rect.h,
                &self.query,
                &self.match_count(),
                self.text_colors(),
            )?;
            let sprite = self.compositor.CreateSpriteVisual()?;
            sprite.SetSize(Vector2 { X: rect.w, Y: rect.h })?;
            sprite.SetOffset(Vector3 { X: rect.x, Y: rect.y, Z: 0.0 })?;
            sprite.SetBrush(&self.compositor.CreateSurfaceBrushWithSurface(&surface)?)?;
            self.content.Children()?.InsertAtTop(&sprite)?;
            Ok(())
        })();

        if let Err(e) = built {
            log_warn!("could not draw the filter strip: {e}");
        }
    }

    fn match_count(&self) -> String {
        match self.items.len() {
            0 => "no matches".into(),
            shown => format!("{shown} of {}", self.total),
        }
    }

    /// The keep-open pushpin, built last so it sits above the tiles: the grid
    /// scrolls under it rather than carrying it off the top.
    fn build_chrome(&mut self, radius: f32) {
        let Some(renderer) = &self.renderer else { return };
        let rect = self.layout.chrome();
        if rect.w < 16.0 || rect.h < 12.0 {
            return;
        }

        let hot = self.chrome_hot;
        let built = (|| -> Result<(CompositionColorBrush, SpriteVisual)> {
            let children = self.content.Children()?;
            let (pill, brush) = self.rounded_rect(
                Vector2 { X: rect.w, Y: rect.h },
                (rect.h / 2.0).min(radius * 2.0),
                self.chrome_color(hot),
            )?;
            pill.SetOffset(Vector3 { X: rect.x, Y: rect.y, Z: 0.0 })?;
            children.InsertAtTop(&pill)?;

            let surface = renderer.create_surface(rect.w, rect.h)?;
            renderer.draw_glyph(
                &surface,
                rect.w,
                rect.h,
                PIN_GLYPH,
                d2d_color(&self.config.theme.text),
            )?;
            let sprite = self.compositor.CreateSpriteVisual()?;
            sprite.SetSize(Vector2 { X: rect.w, Y: rect.h })?;
            sprite.SetOffset(Vector3 { X: rect.x, Y: rect.y, Z: 0.0 })?;
            sprite.SetBrush(&self.compositor.CreateSurfaceBrushWithSurface(&surface)?)?;
            children.InsertAtTop(&sprite)?;
            Ok((brush, sprite))
        })();

        match built {
            // A missing button is not worth refusing to show the panel over:
            // the right-click menu carries the same toggle.
            Err(e) => log_warn!("could not draw the keep-open button: {e}"),
            Ok(chrome) => self.chrome = Some(chrome),
        }
    }

    fn chrome_color(&self, hot: bool) -> Color {
        let theme = &self.config.theme;
        color_of(if hot {
            &theme.tile_hover
        } else if self.keep_open {
            &theme.tile_drag
        } else {
            &theme.tile
        })
    }

    fn chrome_hit(&self, x: f32, y: f32) -> bool {
        self.chrome.is_some() && self.layout.chrome().contains(x, y)
    }

    /// The strip swallows clicks. Dismissing on a click there would read as a
    /// bug. Whole row, not just the drawn text.
    fn search_hit(&self, y: f32) -> bool {
        !self.query.is_empty() && y >= 0.0 && y < self.layout.search_rect().h
    }

    fn set_chrome_hot(&mut self, hot: bool) {
        if self.chrome_hot == hot {
            return;
        }
        self.chrome_hot = hot;
        if let Some((brush, _)) = &self.chrome {
            let _ = brush.SetColor(self.chrome_color(hot));
        }
    }

    fn text_colors(&self) -> TextColors {
        let text = d2d_color(&self.config.theme.text);
        TextColors { title: text, detail: dim(text) }
    }

    /// Icon size in physical pixels: big enough to fill the tile's image area
    /// without asking the shell for more than it will be shown at.
    fn icon_size(&self) -> u32 {
        let g = &self.config.grid;
        let area = (g.tile_height - g.label_height).max(16.0) * self.scale();
        (area * 0.6).clamp(32.0, 256.0) as u32
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

    /// Repositions existing visuals after a scroll, without rebuilding them.
    fn reposition(&self) {
        for (index, tile) in self.tiles.iter().enumerate() {
            let rect = self.layout.tile_rect(index, self.scroll);
            let _ = tile.root.SetOffset(Vector3 { X: rect.x, Y: rect.y, Z: 0.0 });
        }
        for (visual, (_, rect)) in self.headers.iter().zip(self.layout.headers(self.scroll)) {
            let _ = visual.SetOffset(Vector3 { X: rect.x, Y: rect.y, Z: 0.0 });
        }
    }

    /// Hover beats selection: a tile that did not light up under the pointer
    /// reads as dead.
    fn tile_color(&self, index: usize) -> Color {
        let theme = &self.config.theme;
        color_of(if self.hover == Some(index) {
            &theme.tile_hover
        } else if self.selected == Some(index) {
            &theme.tile_selected
        } else {
            &theme.tile
        })
    }

    fn repaint_tile(&self, index: usize) {
        if let Some(tile) = self.tiles.get(index) {
            let _ = tile.brush.SetColor(self.tile_color(index));
        }
    }

    fn set_hover(&mut self, index: Option<usize>) {
        if self.hover == index {
            return;
        }
        let previous = self.hover;
        self.hover = index;
        for slot in [previous, index].into_iter().flatten() {
            self.repaint_tile(slot);
        }
    }

    fn set_selected(&mut self, index: Option<usize>) {
        if self.selected == index {
            return;
        }
        let previous = self.selected;
        self.selected = index;
        for slot in [previous, index].into_iter().flatten() {
            self.repaint_tile(slot);
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

    fn scroll_by(&mut self, delta: f32) {
        if self.layout.max_scroll <= 0.0 {
            return;
        }
        let next = self.layout.clamp_scroll(self.scroll - delta);
        if (next - self.scroll).abs() < 0.5 {
            return;
        }
        self.scroll = next;
        self.reposition();
    }

    fn activate(&mut self, index: usize) {
        let Some(item) = self.items.get(index).cloned() else {
            return;
        };

        if self.config.dry_run {
            log_dry!("would {}", item.activation_summary());
            self.hide(true);
            return;
        }

        // Get out of the way first, and do not restore the caller: we are about
        // to replace it. Foreground rights still hold, because the hotkey that
        // summoned the panel made this process the last input recipient.
        self.hide(false);
        activate::activate(&item);
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
        let show_detail = self.config.grid.show_detail;
        let text = d2d_color(&self.config.theme.text);
        let colors = TextColors { title: text, detail: dim(text) };

        let mut filled = 0;
        for (index, tile) in self.tiles.iter_mut().enumerate() {
            if !tile.awaiting_icon {
                continue;
            }
            let (Some(surface), Some(item)) = (tile.surface.as_ref(), self.items.get(index)) else {
                continue;
            };
            let Some(source) = item.icon_source.as_deref() else {
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
                detail: if show_detail { &item.detail } else { "" },
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

    fn on_model_changed(&mut self) {
        if !self.visible {
            return;
        }
        let previous = self.hover.and_then(|i| self.items.get(i)).map(|i| i.id.clone());
        let held = self.selected.and_then(|i| self.items.get(i)).map(|i| i.id.clone());

        self.reload();
        self.scroll = self.layout.clamp_scroll(self.scroll);

        // `reload` picks the query's best match, which is wrong when a window
        // merely opened. Put the selection back if it still exists.
        if let Some(id) = held
            && let Some(moved) = self.items.iter().position(|i| i.id == id)
        {
            self.selected = Some(moved);
        }

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

    /// Add a picked target to the config. The watcher would catch the write on
    /// its own, but reloading here makes the tile appear immediately.
    fn pin(&mut self, target: Option<String>) {
        let Some(target) = target else { return };
        if pins::add(&target).is_some() {
            self.reload_config();
        }
    }

    // --- type to filter ---

    /// Column count freezes on the first character, so narrowing only shortens
    /// the panel. Re-deriving width per keystroke would slide the grid sideways
    /// under the eye reading it.
    fn set_query(&mut self, query: String) {
        if self.query == query {
            return;
        }
        if self.query.is_empty() {
            self.frozen_cols = self.layout.cols;
        }
        self.query = query;
        if self.query.is_empty() {
            self.frozen_cols = 0;
        }
        // A changed query gets a fresh answer to what Enter takes.
        self.selected = None;
        self.on_model_changed();
        let p = self.layout.panel;
        log_info!(
            "filter \"{}\": {} of {} item(s), {} cols, panel {}x{}",
            self.query,
            self.items.len(),
            self.total,
            self.layout.cols,
            p.w as i32,
            p.h as i32
        );
    }

    /// Printable extends, backspace shortens. Escape, Enter and Tab arrive
    /// here too and were already handled as key presses.
    fn on_char(&mut self, code: u32) {
        const BACKSPACE: u32 = 0x08;
        /// Ctrl+Backspace. No words to walk back through, so all of it goes.
        const CTRL_BACKSPACE: u32 = 0x7F;

        // WM_CHAR is UTF-16, so astral characters arrive as unpaired
        // surrogates and are dropped. Nothing worth filtering on is spelled
        // in them.
        let Some(c) = char::from_u32(code) else { return };

        let mut query = self.query.clone();
        match code {
            BACKSPACE => {
                query.pop();
            }
            CTRL_BACKSPACE => query.clear(),
            // A leading space would raise an empty strip and mean nothing.
            _ if c == ' ' && query.is_empty() => return,
            _ if c.is_control() => return,
            _ => query.push(c),
        }
        self.set_query(query);
    }

    /// `false` hands the key back to `DefWindowProcW`. Arrows and Enter work
    /// on the whole grid, filtered or not.
    fn on_key(&mut self, vk: u16) -> bool {
        const ESCAPE: u16 = VK_ESCAPE.0;
        const ENTER: u16 = VK_RETURN.0;
        const LEFT: u16 = VK_LEFT.0;
        const RIGHT: u16 = VK_RIGHT.0;
        const UP: u16 = VK_UP.0;
        const DOWN: u16 = VK_DOWN.0;
        const HOME: u16 = VK_HOME.0;
        const END: u16 = VK_END.0;

        let row = self.layout.cols.max(1) as isize;
        match vk {
            // Query first, panel second. Backspacing out of a long mistyped
            // filter is not what Escape is reached for.
            ESCAPE => {
                if self.query.is_empty() {
                    self.hide(true);
                } else {
                    self.set_query(String::new());
                }
                true
            }
            ENTER => {
                if let Some(index) = self.selected {
                    self.activate(index);
                }
                true
            }
            LEFT => self.move_selection(-1),
            RIGHT => self.move_selection(1),
            UP => self.move_selection(-row),
            DOWN => self.move_selection(row),
            HOME => self.select_index(0),
            END => self.select_index(usize::MAX),
            _ => false,
        }
    }

    /// Clamped to the grid. Always claims the key: an arrow falling through to
    /// the shell is worse than one doing nothing.
    fn move_selection(&mut self, delta: isize) -> bool {
        if self.items.is_empty() {
            return true;
        }
        let last = self.items.len() as isize - 1;
        let next = match self.selected {
            // First press picks an end, not the middle.
            None if delta > 0 => 0,
            None => last,
            Some(current) => (current as isize).saturating_add(delta).clamp(0, last),
        };
        self.select_index(next as usize)
    }

    /// Home and End name a place, not a direction. Through `move_selection`,
    /// End would read as "forwards from nowhere" and land on the first tile.
    fn select_index(&mut self, index: usize) -> bool {
        if self.items.is_empty() {
            return true;
        }
        let index = index.min(self.items.len() - 1);
        self.set_selected(Some(index));
        self.scroll_into_view(index);
        true
    }

    /// Keeps the selection from being walked off a scrolling grid.
    fn scroll_into_view(&mut self, index: usize) {
        if self.layout.max_scroll <= 0.0 {
            return;
        }
        let rect = self.layout.tile_rect(index, self.scroll);
        // The grid scrolls under the strip, so visible starts below it.
        let top = self.layout.search_rect().h;
        let above = top - rect.y;
        let below = (rect.y + rect.h) - self.layout.panel.h;

        let delta = if above > 0.0 {
            -above
        } else if below > 0.0 {
            below
        } else {
            return;
        };
        let next = self.layout.clamp_scroll(self.scroll + delta);
        if (next - self.scroll).abs() < 0.5 {
            return;
        }
        self.scroll = next;
        self.reposition();
    }

    // --- rearranging, without a mode ---

    /// Which config section a tile belongs to.
    fn section_of(&self, tile: usize) -> Option<&Section> {
        let band = self.layout.band_of(tile)?;
        self.section_in_band(band)
    }

    fn section_in_band(&self, band: usize) -> Option<&Section> {
        let band = self.layout.bands().get(band)?;
        self.sections.get(band.section)
    }

    /// Only pins flick owns can be removed. A taskbar entry belongs to the
    /// taskbar, and unpinning it there is Windows' business, not flick's
    /// (safety rule 3).
    fn removable(&self, tile: usize) -> bool {
        self.section_of(tile).is_some_and(|s| s.source == Source::Manual)
    }

    /// Window tiles are MRU ordered by the foreground hook, so a saved order
    /// would fight the hook on every focus change. Pinned sections have an order
    /// that is flick's to keep.
    ///
    /// Never while filtering: writing back a subset's order would drop every
    /// pin the query hid.
    fn draggable(&self, tile: usize) -> bool {
        self.query.is_empty()
            && self
                .section_of(tile)
                .is_some_and(|s| matches!(s.source, Source::Manual | Source::Taskbar))
    }

    /// Take a press on a tile. Whether it turns out to be a click or a drag is
    /// decided later, by how far the cursor travels.
    fn begin_press(&mut self, tile: usize, x: f32, y: f32) {
        let rect = self.layout.tile_rect(tile, self.scroll);
        let band = self.layout.band_of(tile).filter(|_| self.draggable(tile));
        // SAFETY: our own window. Capture keeps the moves coming when the cursor
        // leaves the panel mid-drag, and makes the release ours whatever it is
        // over.
        unsafe {
            SetCapture(self.hwnd);
        }
        self.press = Some(Press {
            tile,
            band,
            grab: (x - rect.x, y - rect.y),
            start: (x, y),
            dragging: false,
            slot: band.map_or(0, |band| tile - self.layout.bands()[band].first),
        });
    }

    fn press_moved_to(&mut self, x: f32, y: f32) {
        let Some(mut press) = self.press.take() else { return };

        if !press.dragging {
            let (slop_x, slop_y) = drag_slop();
            if (x - press.start.0).abs() <= slop_x && (y - press.start.1).abs() <= slop_y {
                self.press = Some(press);
                return;
            }
            press.dragging = true;
            // Past the threshold on a tile flick cannot rearrange: nothing to
            // drag, and no activation either — this was not a click.
            if press.band.is_none() {
                self.press = Some(press);
                return;
            }
            if let Some(item) = self.items.get(press.tile) {
                log_info!("picked up \"{}\"", item.title);
            }
            // Lift the tile out of the flow: on top of its neighbours, and
            // coloured so it reads as held rather than hovered.
            if let Some(tile) = self.tiles.get(press.tile)
                && let Ok(children) = self.content.Children()
            {
                let _ = children.Remove(&tile.root);
                let _ = children.InsertAtTop(&tile.root);
                let _ = tile.brush.SetColor(color_of(&self.config.theme.tile_drag));
            }
        }

        if let Some(band) = press.band {
            press.slot = self.layout.insert_slot(band, x, y, self.scroll);
            self.preview(&press);

            if let Some(tile) = self.tiles.get(press.tile) {
                let _ = tile.root.SetOffset(Vector3 {
                    X: x - press.grab.0,
                    Y: y - press.grab.1,
                    Z: 0.0,
                });
            }
        }
        self.press = Some(press);
    }

    /// Slide the section's other tiles into the order they would take if the
    /// drag ended here.
    fn preview(&self, press: &Press) {
        let Some(band) = press.band.and_then(|band| self.layout.bands().get(band)) else {
            return;
        };
        let from = press.tile - band.first;
        for (position, slot) in reordered(band.count, from, press.slot).iter().enumerate() {
            if *slot == from {
                continue;
            }
            let rect = self.layout.tile_rect(band.first + position, self.scroll);
            if let Some(tile) = self.tiles.get(band.first + slot) {
                let _ = tile.root.SetOffset(Vector3 { X: rect.x, Y: rect.y, Z: 0.0 });
            }
        }
    }

    /// Put everything back where the layout says it goes.
    fn cancel_press(&mut self, press: &Press) {
        // SAFETY: releasing a capture we no longer hold is harmless.
        unsafe {
            let _ = ReleaseCapture();
        }
        self.repaint_tile(press.tile);
        self.reposition();
    }

    /// Write the new order out, then reload so the grid and the file agree.
    fn commit_drag(&mut self, press: &Press) {
        // SAFETY: releasing a capture we no longer hold is harmless.
        unsafe {
            let _ = ReleaseCapture();
        }
        let Some(band) = press
            .band
            .and_then(|band| self.layout.bands().get(band))
            .cloned()
        else {
            return;
        };
        let Some(section) = self.sections.get(band.section) else {
            return;
        };

        let from = press.tile - band.first;
        let slots = reordered(band.count, from, press.slot);
        if slots.iter().enumerate().all(|(position, slot)| position == *slot) {
            self.cancel_press(press);
            return;
        }

        // What identifies an entry in config: manual sections list parsing
        // names, taskbar sections list pin names.
        let key = |item: &Item| match section.source {
            Source::Manual => item.shell_target().map(str::to_owned),
            _ => Some(item.title.clone()),
        };
        let Some(keys) = slots
            .iter()
            .map(|slot| self.items.get(band.first + slot).and_then(key))
            .collect::<Option<Vec<String>>>()
        else {
            log_warn!("could not identify every tile in \"{}\"; order not saved", section.title);
            self.cancel_press(press);
            return;
        };

        let title = section.title.clone();
        let saved = match section.source {
            Source::Manual => pins::reorder(&title, &keys),
            Source::Taskbar => pins::set_order(&title, &keys),
            // Ordered by the foreground hook and the browser, not by flick.
            Source::Windows | Source::Tabs => false,
        };
        if saved {
            self.reload_config();
        } else {
            self.cancel_press(press);
        }
    }

    // --- the tile menu ---

    /// Right-click on a tile, or on the panel itself.
    ///
    /// Managing a pin lives here rather than in a mode, and the most useful
    /// entry is on the tiles that are not pins at all: something already running
    /// is the thing a user most often wants to pin, and flick is already showing
    /// it.
    fn show_menu(&mut self, lparam: LPARAM) {
        let (x, y) = point_of(lparam);
        let tile = self.layout.hit_test(x, y, self.scroll);
        let entries = self.menu_for(tile);

        self.menu_open = true;
        let chosen = menu::show(self.hwnd, &entries);
        self.menu_open = false;

        match chosen {
            Some(menu::CMD_PIN_APP) => self.pin_app_of(tile),
            Some(menu::CMD_UNPIN) => self.unpin(tile),
            Some(menu::CMD_OPEN_LOCATION) => self.open_location(tile),
            Some(menu::CMD_ADD_APP) => {
                let picked = picker::pick_app(self.hwnd);
                self.pin(picked);
            }
            Some(menu::CMD_ADD_FOLDER) => {
                let picked = picker::pick_folder(self.hwnd);
                self.pin(picked);
            }
            Some(menu::CMD_ADD_FILE) => {
                let picked = picker::pick_file(self.hwnd);
                self.pin(picked);
            }
            Some(menu::CMD_KEEP_OPEN) => self.set_keep_open(!self.keep_open),
            Some(menu::CMD_SETTINGS) => open_config(),
            _ => {}
        }
    }

    fn menu_for(&self, tile: Option<usize>) -> Vec<Option<menu::Entry>> {
        let mut entries = Vec::new();

        if let Some(index) = tile
            && let Some(item) = self.items.get(index)
        {
            match item.target {
                // The pin-what-is-in-front case. No picker, no typing: the app is
                // already on screen and flick already knows its path.
                Target::Window(_) => {
                    if item.icon_source.is_some() {
                        // Not "Pin <name>": the name available here is the
                        // executable's stem, which reads as "Pin obs64".
                        entries.push(Some(menu::Entry::new(menu::CMD_PIN_APP, "Pin this app")));
                    }
                }
                Target::Shell(_) => {
                    if self.removable(index) {
                        entries.push(Some(menu::Entry::new(menu::CMD_UNPIN, "Unpin")));
                    }
                }
                // Bookmarking a tab arrives with the rest of Milestone 4.
                Target::Tab { .. } => {}
            }
            if self.locatable(index) {
                entries.push(Some(menu::Entry::new(
                    menu::CMD_OPEN_LOCATION,
                    "Open file location",
                )));
            }
            if !entries.is_empty() {
                entries.push(None);
            }
        }

        entries.push(Some(menu::Entry::new(menu::CMD_ADD_APP, "Add app...")));
        entries.push(Some(menu::Entry::new(menu::CMD_ADD_FOLDER, "Add folder...")));
        entries.push(Some(menu::Entry::new(
            menu::CMD_ADD_FILE,
            "Add file or shortcut...",
        )));
        entries.push(None);
        entries.push(Some(menu::Entry::checkable(
            menu::CMD_KEEP_OPEN,
            "Keep panel open",
            self.keep_open,
        )));
        entries.push(Some(menu::Entry::new(menu::CMD_SETTINGS, "Settings...")));
        entries
    }

    /// Pin the app behind a running window. Its icon source is its executable,
    /// which is exactly the parsing name a pin stores.
    fn pin_app_of(&mut self, tile: Option<usize>) {
        let target = tile
            .and_then(|index| self.items.get(index))
            .filter(|item| matches!(item.target, Target::Window(_)))
            .and_then(|item| item.icon_source.clone());
        self.pin(target);
    }

    fn unpin(&mut self, tile: Option<usize>) {
        let Some(tile) = tile else { return };
        let (Some(section), Some(item)) = (self.section_of(tile), self.items.get(tile)) else {
            return;
        };
        let (Some(target), title) = (item.shell_target(), section.title.clone()) else {
            return;
        };
        if pins::remove(&title, target) {
            self.reload_config();
        }
    }

    /// Only for tiles backed by something on disk. A settings page or a URL has
    /// no folder to show.
    fn locatable(&self, tile: usize) -> bool {
        self.items
            .get(tile)
            .and_then(|item| item.icon_source.as_deref())
            .is_some_and(|source| std::path::Path::new(source).exists())
    }

    fn open_location(&mut self, tile: Option<usize>) {
        let Some(source) = tile
            .and_then(|index| self.items.get(index))
            .and_then(|item| item.icon_source.clone())
        else {
            return;
        };
        // Explorer's own "show me this file" verb, so the target is revealed and
        // selected rather than opened.
        let arguments = windows::core::HSTRING::from(format!("/select,\"{source}\""));
        self.hide(false);
        // SAFETY: both strings outlive the call, and `open` never elevates.
        let launched = unsafe {
            windows::Win32::UI::Shell::ShellExecuteW(
                None,
                w!("open"),
                w!("explorer.exe"),
                &arguments,
                None,
                SW_SHOWNORMAL,
            )
        };
        if launched.0 as isize <= 32 {
            log_warn!("could not show {source} in Explorer");
        }
    }

    fn set_keep_open(&mut self, on: bool) {
        if self.keep_open == on {
            return;
        }
        self.keep_open = on;
        log_info!("keep panel open: {on}");
        if let Some((brush, _)) = &self.chrome {
            let _ = brush.SetColor(self.chrome_color(self.chrome_hot));
        }
    }

    // --- drops from Explorer ---

    /// Which section a drop at this point would land in: the one under the
    /// cursor when it takes pins, otherwise the first section that does.
    fn drop_target(&self, lparam: LPARAM) -> Option<usize> {
        if !self.visible {
            return None;
        }
        let (x, y) = point_of(lparam);
        let under = self
            .layout
            .band_at(x, y, self.scroll)
            .filter(|band| {
                self.section_in_band(*band).is_some_and(|s| s.source == Source::Manual)
            });
        under.or_else(|| {
            (0..self.layout.bands().len()).find(|band| {
                self.section_in_band(*band).is_some_and(|s| s.source == Source::Manual)
            })
        })
    }

    /// Tint a whole section to show where a drop would go. There is no cursor to
    /// follow here — the drag belongs to Explorer — so the target has to be
    /// visible in the panel itself.
    fn set_drop_band(&mut self, band: Option<usize>) {
        if self.drop_band == band {
            return;
        }
        let normal = color_of(&self.config.theme.tile);
        let hot = color_of(&self.config.theme.tile_hover);
        for (slot, want) in [(self.drop_band, normal), (band, hot)] {
            let Some(band) = slot.and_then(|index| self.layout.bands().get(index)) else {
                continue;
            };
            for tile in self.tiles.iter().skip(band.first).take(band.count) {
                let _ = tile.brush.SetColor(want);
            }
        }
        self.drop_band = band;
    }

    fn on_drop(&mut self, paths: &[String], lparam: LPARAM) -> bool {
        let Some(band) = self.drop_target(lparam) else {
            return false;
        };
        self.set_drop_band(None);
        let title = self.section_in_band(band).map(|s| s.title.clone());

        let mut pinned = 0;
        for path in paths {
            if pins::add_into(title.as_deref(), path).is_some() {
                pinned += 1;
            }
        }
        log_info!("dropped {pinned} of {} item(s)", paths.len());
        if pinned > 0 {
            self.reload_config();
        }
        pinned > 0
    }

    /// Re-read the config and apply it live. Only the hotkey needs unbinding;
    /// everything else is read fresh on the next show.
    fn reload_config(&mut self) {
        let next = Config::load();
        let hotkey_changed = next.hotkey != self.config.hotkey;
        self.config = next;

        if hotkey_changed {
            if self.hotkey_bound {
                // SAFETY: matches the registration in bind_hotkey.
                unsafe {
                    let _ = UnregisterHotKey(Some(self.hwnd), HOTKEY_ID);
                }
                self.hotkey_bound = false;
            }
            self.bind_hotkey();
        }

        store::reconfigure(&self.config.sections);
        log_info!("config reloaded");

        if self.visible {
            self.on_model_changed();
        }
    }

    fn cursor_index(&self, lparam: LPARAM) -> Option<usize> {
        let (x, y) = point_of(lparam);
        self.layout.hit_test(x, y, self.scroll)
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
                        Some(tray::CMD_ADD_APP) => {
                            let picked = picker::pick_app(self.hwnd);
                            self.pin(picked);
                        }
                        Some(tray::CMD_ADD_FOLDER) => {
                            let picked = picker::pick_folder(self.hwnd);
                            self.pin(picked);
                        }
                        Some(tray::CMD_ADD_FILE) => {
                            let picked = picker::pick_file(self.hwnd);
                            self.pin(picked);
                        }
                        Some(tray::CMD_EDIT_CONFIG) => open_config(),
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
            store::WM_MODEL_CHANGED | crate::browser::server::WM_TABS_CHANGED => {
                self.on_model_changed();
                Some(LRESULT(0))
            }
            icons::WM_ICON_READY => {
                self.on_icons_ready();
                Some(LRESULT(0))
            }
            watch::WM_CONFIG_RELOAD => {
                self.reload_config();
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                if self.press.is_some() {
                    let (x, y) = point_of(lparam);
                    self.press_moved_to(x, y);
                    return Some(LRESULT(0));
                }
                self.track_mouse_leave();
                let (x, y) = point_of(lparam);
                let on_chrome = self.chrome_hit(x, y);
                self.set_chrome_hot(on_chrome);
                let index = if on_chrome { None } else { self.cursor_index(lparam) };
                self.set_hover(index);
                Some(LRESULT(0))
            }
            WM_MOUSELEAVE => {
                self.tracking_mouse = false;
                self.set_hover(None);
                self.set_chrome_hot(false);
                Some(LRESULT(0))
            }
            // A press is not yet a click. Which one it becomes is decided on
            // release, by whether the cursor travelled far enough to be a drag.
            WM_LBUTTONDOWN => {
                let (x, y) = point_of(lparam);
                if self.chrome_hit(x, y) {
                    // The button answers on release, like any button.
                    return Some(LRESULT(0));
                }
                if let Some(index) = self.layout.hit_test(x, y, self.scroll) {
                    self.begin_press(index, x, y);
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                if let Some(press) = self.press.take() {
                    match (press.dragging, press.band) {
                        // Travelled, and over something flick can rearrange.
                        (true, Some(_)) => self.commit_drag(&press),
                        // Travelled, but not a rearrangeable tile. A drag that
                        // went nowhere is not an activation.
                        (true, None) => self.cancel_press(&press),
                        // Never travelled: an ordinary click.
                        (false, _) => {
                            self.cancel_press(&press);
                            let (x, y) = point_of(lparam);
                            if self.layout.hit_test(x, y, self.scroll) == Some(press.tile) {
                                self.activate(press.tile);
                            }
                        }
                    }
                    return Some(LRESULT(0));
                }
                let (x, y) = point_of(lparam);
                if self.chrome_hit(x, y) {
                    self.set_keep_open(!self.keep_open);
                    return Some(LRESULT(0));
                }
                if self.search_hit(y) {
                    return Some(LRESULT(0));
                }
                // A click on the panel's own padding dismisses, matching the
                // click-outside behaviour.
                if self.cursor_index(lparam).is_none() {
                    self.hide(true);
                }
                Some(LRESULT(0))
            }
            WM_RBUTTONUP => {
                self.show_menu(lparam);
                Some(LRESULT(0))
            }
            // Capture lost to something else — an alt-tab, a system dialog.
            WM_CAPTURECHANGED => {
                if let Some(press) = self.press.take() {
                    self.cancel_press(&press);
                }
                Some(LRESULT(0))
            }
            dropzone::WM_DRAG_OVER => {
                let band = self.drop_target(lparam);
                self.set_drop_band(band);
                Some(LRESULT(band.is_some() as isize))
            }
            dropzone::WM_DRAG_LEAVE => {
                self.set_drop_band(None);
                Some(LRESULT(0))
            }
            dropzone::WM_DRAG_DROP => {
                // SAFETY: the sender blocks in `SendMessageW` on this thread for
                // the whole call, so the Vec it points at is alive and unaliased.
                let paths = unsafe { &*(wparam.0 as *const Vec<String>) };
                let taken = self.on_drop(paths, lparam);
                Some(LRESULT(taken as isize))
            }
            WM_MOUSEWHEEL => {
                let delta = ((wparam.0 >> 16) & 0xFFFF) as i16 as f32;
                self.scroll_by(delta);
                Some(LRESULT(0))
            }
            // A hidden panel has no keyboard. Focus guarantees that, except
            // for posted messages, which bypass it.
            WM_KEYDOWN if self.visible => self.on_key(wparam.0 as u16).then_some(LRESULT(0)),
            WM_CHAR if self.visible => {
                self.on_char(wparam.0 as u32);
                Some(LRESULT(0))
            }
            // Clicking away, or anything else stealing focus, dismisses — unless
            // the panel is pinned open, or a menu of ours is up.
            WM_ACTIVATE
                if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE
                    && !self.keep_open
                    && !self.menu_open =>
            {
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
        dropzone::revoke(self.hwnd);
        if self.hotkey_bound {
            // SAFETY: matches the successful RegisterHotKey in bind_hotkey.
            unsafe {
                let _ = UnregisterHotKey(Some(self.hwnd), HOTKEY_ID);
            }
        }
    }
}

/// Open `flick.toml` in whatever the user edits TOML with. Falls back to
/// Notepad, since a bare `.toml` often has no registered handler.
fn open_config() {
    let Some(path) = Config::path() else { return };
    let target = windows::core::HSTRING::from(path.as_os_str());

    // SAFETY: the strings outlive the calls. `open` never elevates.
    let opened = unsafe {
        windows::Win32::UI::Shell::ShellExecuteW(
            None,
            w!("open"),
            &target,
            None,
            None,
            SW_SHOWNORMAL,
        )
    };
    if opened.0 as isize > 32 {
        log_info!("opened {} for editing", path.display());
        return;
    }

    // SAFETY: same contract; notepad.exe is always present.
    let fallback = unsafe {
        windows::Win32::UI::Shell::ShellExecuteW(
            None,
            w!("open"),
            w!("notepad.exe"),
            &target,
            None,
            SW_SHOWNORMAL,
        )
    };
    if fallback.0 as isize <= 32 {
        log_warn!("could not open {} in an editor", path.display());
    }
}

/// Client point out of a mouse message's lparam. Signed, because a captured
/// drag reports positions outside the window.
fn point_of(lparam: LPARAM) -> (f32, f32) {
    (
        (lparam.0 & 0xFFFF) as i16 as f32,
        ((lparam.0 >> 16) & 0xFFFF) as i16 as f32,
    )
}

/// The detail line sits under the title; same hue, less presence.
fn dim(mut c: D2D1_COLOR_F) -> D2D1_COLOR_F {
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

        log_warn!("GetMonitorInfoW failed; falling back to the primary screen size");
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
