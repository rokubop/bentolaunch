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
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::Win32::System::WinRT::{
    CreateDispatcherQueueController, DQTAT_COM_NONE, DQTYPE_THREAD_CURRENT, DispatcherQueueOptions,
};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, ReleaseCapture, SetActiveWindow, SetCapture, TME_LEAVE, VK_CONTROL,
    TRACKMOUSEEVENT, TrackMouseEvent, UnregisterHotKey, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME,
    VK_LEFT, VK_RETURN, VK_RIGHT, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, Interface, PCWSTR, Result, w};
use windows_numerics::{Vector2, Vector3};

use crate::browser::{gate, server};
use crate::config::{self, Config, Source};
use crate::instance;
use crate::model::store;
use crate::model::{Handle, Item, Kind, Section, Target};
use crate::safety;
use crate::shell::{activate, arrange, icons, picker};
use crate::ui::filter;
use crate::ui::grid::{
    At, Band, BoxState, Command, Control, Layout, Metrics, Rect as GridRect, SectionShape,
    centred_grid, commands, controls, home_button, origin_run, reordered,
};
use crate::ui::menu;
use crate::ui::settings::{SETTINGS, Setting};
use crate::ui::render::{Mark, OptionPaint, Renderer, TextColors, TilePaint, d2d_color};
use crate::ui::spotlight::Spotlight;
use crate::ui::tray;
use crate::{pins, watch};
use crate::{log_dry, log_error, log_info, log_warn};

const HOTKEY_ID: i32 = 1;
/// Drives the watchdog heartbeat while the panel is up.
const HEARTBEAT_TIMER: usize = 1;
const HEARTBEAT_MS: u32 = 250;
/// Thick enough to read past a tile's own fill without eating into the icon.
const TARGET_STROKE: f32 = 3.0;
/// A press that never travels this far is a click, not a drag.
///
/// Taken from the shell rather than picked, so bentopick's idea of "that was a
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

    /// `None` if D3D/D2D could not start. bentopick still runs; tiles just lose
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
    /// The name and logo, riding the first header's row. `None` when there is no
    /// row to put it on.
    visible: bool,
    /// Whether a `TrackMouseEvent` request is outstanding. Without one,
    /// WM_MOUSELEAVE never arrives and hover sticks on the last tile.
    tracking_mouse: bool,
    /// Foreground window at show time, so Esc can put it back.
    caller: HWND,
    hotkey_bound: bool,

    /// The window the move tiles act on. `None` means the caller, which is what
    /// makes the common case free: summon over the window you want moved and
    /// the six are already pointed at it.
    target: Option<Handle>,
    /// The ring out on the desktop, around the window the bar acts on. Built
    /// on the first summon, because most sessions never open the panel.
    spotlight: Option<Spotlight>,
    /// An app was picked with nothing of it open. Its window gets adopted as
    /// the target when it turns up, so picking an app that is not running yet
    /// still ends in something to move.
    pending: Option<String>,
    /// A move is in flight. Windows activates what it maximizes or restores,
    /// and the panel dismisses on losing focus, so without this the first
    /// click on the bar would close the bar.
    arranging: bool,
    /// Clicking a window picks it as the target instead of switching to it.
    ///
    /// A latch, not a held key. Nothing that points with gaze or noise can hold
    /// a modifier, and the one control that does this has to be a square worth
    /// aiming at. Ctrl is the second path, never the only one.
    stay: bool,

    query: String,
    /// Held for the query's duration, from the unfiltered grid. 0 when idle.
    frozen_cols: usize,

    /// A context menu is up. It does not deactivate us, but a stray dismissal
    /// while a menu is open would be baffling, so it is treated the same.
    menu_open: bool,
    /// A button is down on a tile. It becomes a drag past the slop threshold and
    /// an activation if it never gets there.
    press: Option<Press>,
    /// The last button-down was taken by something drawn over the grid, so its
    /// release means nothing.
    ///
    /// Recorded rather than worked out again on the way up. What the down did
    /// is often to close the very surface that took it, and a release asking
    /// "is a menu open?" then gets the answer "no" and falls through to the
    /// rule that dismisses the panel.
    handled_down: bool,

    /// Layout edit mode is on. Holds the panel open, and takes the clicks the
    /// grid would otherwise treat as launches.
    ///
    /// A mode, unlike everything else here, because it changes the shape of the
    /// panel rather than one tile's place in it.
    editing: bool,
    /// Which box has been picked, as an index into `sections`. `None` means the
    /// mode is on but nothing is chosen yet, which is where it starts: the
    /// options belong to a box, so there is nothing to show until one is.
    edit: Option<usize>,
    /// The box under the pointer while editing. Separate from `hover`, which
    /// is a tile: in this mode the thing being pointed at is a whole box.
    hover_box: Option<usize>,
    /// One fill per band, in band order, so hovering repaints a box without
    /// rebuilding the grid under the pointer.
    box_faces: Vec<CompositionColorBrush>,
    /// The option tiles, their hit rects and their fills. Rebuilt whenever the
    /// selected box changes, repainted in place on hover.
    options: Vec<(Control, GridRect, CompositionColorBrush)>,
    hover_option: Option<usize>,
    /// The plate the option squares sit on. Anything landing on it belongs to
    /// the overlay, including the gaps between squares.
    options_plate: Option<GridRect>,

    /// The app's own button, always in the same corner, and its fill so it can
    /// light up under the pointer without a rebuild.
    home: Option<(GridRect, CompositionColorBrush)>,
    hover_home: bool,
    /// The button's surface, kept so the logo can be painted into it when the
    /// shell worker delivers it.
    home_surface: Option<CompositionDrawingSurface>,
    home_awaiting_icon: bool,
    /// The big menu that button opens. Not a mode: it is closed by picking
    /// something, by clicking the button again, or by Escape.
    menu_open_big: bool,
    menu_items: Vec<(Command, GridRect, CompositionColorBrush)>,
    hover_menu: Option<usize>,

    /// The settings surface is up. Not a mode either: it is closed by Done, by
    /// Escape, by the corner button, or by clicking off it, and the grid
    /// underneath is untouched the whole time.
    settings_open: bool,
    settings_items: Vec<(Setting, GridRect, CompositionColorBrush)>,
    hover_setting: Option<usize>,
}

/// A pressed tile, which may still turn out to be either a click or a drag.
struct Press {
    /// Flat index of the tile under the press.
    tile: usize,
    /// Its section, if this tile's order is bentopick's to rearrange.
    band: Option<usize>,
    /// The tiles this drag may reorder: flat index of the first, and how many.
    /// One source's run inside the band. See `Panel::origin_run`.
    run: (usize, usize),
    /// Where in the tile it was pressed, so a drag does not jump to the cursor.
    grab: (f32, f32),
    start: (f32, f32),
    /// Past the slop threshold: no longer a click.
    dragging: bool,
    /// Insertion slot within the run the cursor is currently over.
    slot: usize,
}


impl Panel {
    pub fn create(config: Config) -> Result<Box<Panel>> {
        // SAFETY: apartment-threaded init for a UI thread, which is what the
        // composition stack needs before the dispatcher queue exists.
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
            header_gap: 0.0,
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
            home_surface: None,
            home_awaiting_icon: false,
            visible: false,
            tracking_mouse: false,
            caller: HWND(std::ptr::null_mut()),
            target: None,
            spotlight: None,
            pending: None,
            arranging: false,
            stay: false,
            hotkey_bound: false,
            query: String::new(),
            frozen_cols: 0,
            menu_open: false,
            editing: false,
            edit: None,
            hover_box: None,
            box_faces: Vec::new(),
            options: Vec::new(),
            hover_option: None,
            options_plate: None,
            home: None,
            hover_home: false,
            handled_down: false,
            menu_open_big: false,
            menu_items: Vec::new(),
            hover_menu: None,
            settings_open: false,
            settings_items: Vec::new(),
            hover_setting: None,
            press: None,
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
                "hotkey '{}' could not be parsed; bentopick has no way to be summoned",
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
            header_gap: g.header_gap * scale,
            section_gap: g.section_gap * scale,
            search_h: if self.query.is_empty() { 0.0 } else { g.search_height * scale },
        }
    }

    /// Section layout comes from config, matched to the live sections by title:
    /// an empty section never reaches `self.sections`, so the two lists are the
    /// same order but not the same length.
    fn shapes(&self) -> Vec<SectionShape> {
        self.sections
            .iter()
            .map(|s| {
                let placed = self.config.sections.iter().find(|c| c.title == s.title);
                SectionShape {
                    title: s.title.clone(),
                    count: s.items.len(),
                    at: placed.and_then(|c| c.at.as_deref()).and_then(At::parse),
                    columns: placed.map_or(0, |c| c.columns),
                }
            })
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

        // One line per box. Nothing reads this panel's pixels back off a screen
        // DC, so the arrangement is otherwise only ever confirmed by eye.
        for band in self.layout.bands() {
            let title = self
                .sections
                .get(band.section)
                .map_or("", |section| section.title.as_str());
            log_info!(
                "  box \"{title}\": {} tile(s), {} col(s), at {},{} {}x{}",
                band.count,
                band.cols,
                band.rect.x as i32,
                band.rect.y as i32,
                band.rect.w as i32,
                band.rect.h as i32
            );
        }

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
        // Last, so it can ask whether the panel is up and get the truth.
        self.frame_target();
    }

    pub fn hide(&mut self, restore_caller: bool) {
        if !self.visible {
            return;
        }
        if self.editing {
            log_info!("hide while editing (restore_caller={restore_caller})");
        }
        self.visible = false;
        self.tracking_mouse = false;
        self.press = None;
        self.query.clear();
        self.frozen_cols = 0;
        self.selected = None;
        self.editing = false;
        self.edit = None;
        self.hover_box = None;
        self.hover_option = None;
        self.options.clear();
        self.options_plate = None;
        self.handled_down = false;
        self.menu_open_big = false;
        self.menu_items.clear();
        self.hover_menu = None;
        self.settings_open = false;
        self.settings_items.clear();
        self.hover_setting = None;
        self.hover_home = false;
        if let Some(ring) = self.spotlight.as_mut() {
            ring.hide();
        }
        self.target = None;
        self.pending = None;
        self.arranging = false;
        self.stay = false;
        safety::mark_shown(false);

        // SAFETY: our own window, on our own thread.
        unsafe {
            let _ = KillTimer(Some(self.hwnd), HEARTBEAT_TIMER);
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }

        // Restoring the caller is bentopick undoing its own activation, not acting
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
        self.items.clear();
        self.sections.clear();
    }

    fn rebuild_visuals(&mut self) -> Result<()> {
        let children = self.content.Children()?;
        children.RemoveAll()?;
        self.tiles.clear();
        self.headers.clear();

        let p = self.layout.panel;
        let scale = self.scale();
        let radius = self.config.grid.corner_radius * scale;

        let (backdrop, _) = self.rounded_rect(
            Vector2 { X: p.w, Y: p.h },
            radius * 1.5,
            color_of(&self.config.theme.panel),
        )?;
        children.InsertAtTop(&backdrop)?;
        // Straight after the backdrop, so a plate sits on it rather than under
        // it. The panel colour is 94% opaque; anything below it is not seen.
        self.build_box_plates(radius)?;

        // Boxes get a face of their own while editing. The tile is no longer
        // the thing being pointed at, so the whole box has to light up under
        // the pointer or there is nothing to aim at.
        if let Some(renderer) = &self.renderer {
            let header_color = d2d_color(&self.config.theme.header);
            // Edit mode says what each box is set to, and marks the one the
            // keys will land on. The grid underneath is left alone: the point
            // is to watch the real layout change as it is edited.
            let editing = self.editing;
            let selected_color = d2d_color(&self.config.theme.tile_selected);
            let labels: Vec<String> = if editing {
                self.layout
                    .headers(self.scroll)
                    .map(|(title, _, band)| self.edit_header(band, title))
                    .collect()
            } else {
                Vec::new()
            };

            let mut built = Vec::new();
            for (slot, (title, rect, band)) in self.layout.headers(self.scroll).enumerate() {
                let title = labels.get(slot).map_or(title, String::as_str);
                let section_of = self.layout.bands().get(band).map(|band| band.section);
                let header_color = if editing && self.edit == section_of {
                    selected_color
                } else {
                    header_color
                };
                let surface = match renderer.create_surface(rect.w, rect.h) {
                    Ok(surface) => surface,
                    Err(e) => {
                        log_warn!("could not create a header surface: {e}");
                        continue;
                    }
                };
                if let Err(e) =
                    renderer.draw_header(&surface, rect.w, rect.h, title, header_color)
                {
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
        // What an unavailable move tile is drawn in: the same grey the section
        // titles use, so it reads as label rather than as control.
        let muted = d2d_color(&self.config.theme.header);
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
            let (mark, relabel) = self.action_face(item);
            let colors = match self.inert(item) {
                true => TextColors { title: muted, detail: muted },
                false => colors,
            };

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
                            title: relabel.as_deref().unwrap_or(&item.title),
                            detail: if show_detail { &item.detail } else { "" },
                            icon: icon.as_deref(),
                            mark,
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

            if self.is_target(item) {
                let ring = self.rounded_ring(
                    Vector2 { X: rect.w, Y: rect.h },
                    radius,
                    color_of(&self.config.theme.tile_target),
                )?;
                root.Children()?.InsertAtTop(&ring)?;
            }

            children.InsertAtTop(&root)?;
            built.push(Tile { root, brush, surface, awaiting_icon });
        }

        self.tiles = built;
        // Over the tiles, which are no longer the thing being pointed at.
        self.build_box_scrims(radius)?;
        // After the tiles, so the grid scrolls underneath them.
        self.build_search();
        // Last of all, over the grid: the options for the box being edited, the
        // big menu if it is open, and the button that opens it.
        self.build_options(radius)?;
        self.build_menu(radius)?;
        self.build_settings(radius)?;
        self.build_home(radius)?;
        Ok(())
    }

    /// A tint behind a box, for the sections that asked for one in config.
    ///
    /// Under everything, header included: the point is to say where the box
    /// begins and ends, and a plate that stopped at the header would say it
    /// twice. `InsertAtBottom` rather than build order, so this stays
    /// independent of when the tiles go in.
    fn build_box_plates(&mut self, radius: f32) -> Result<()> {
        let plates: Vec<(GridRect, Color)> = self
            .layout
            .bands()
            .iter()
            .filter_map(|band| {
                let color = self.sections.get(band.section)?.color.as_deref()?;
                Some((band.rect.shifted_by(self.scroll), color_of(color)))
            })
            .collect();
        if plates.is_empty() {
            return Ok(());
        }

        let children = self.content.Children()?;
        for (rect, color) in plates {
            let (face, _) =
                self.rounded_rect(Vector2 { X: rect.w, Y: rect.h }, radius * 1.5, color)?;
            face.SetOffset(Vector3 { X: rect.x, Y: rect.y, Z: 0.0 })?;
            children.InsertAtTop(&face)?;
        }
        Ok(())
    }

    /// A translucent sheet over each box's tiles while editing.
    ///
    /// One layer doing two jobs. It dims the tiles, which are not what is being
    /// pointed at in this mode and should stop advertising themselves as
    /// clickable; and it is the big target that lights up under the pointer.
    ///
    /// Only over the tiles, never the header: the header is the label saying
    /// what the box is set to, and dimming it would hide the thing being read.
    fn build_box_scrims(&mut self, radius: f32) -> Result<()> {
        self.box_faces.clear();
        if !self.editing {
            return Ok(());
        }

        let sheets: Vec<(GridRect, Color)> = self
            .layout
            .bands()
            .iter()
            .enumerate()
            .map(|(index, band)| (self.tiles_of(band), self.box_color(index)))
            .collect();

        let children = self.content.Children()?;
        for (rect, color) in sheets {
            let (face, brush) =
                self.rounded_rect(Vector2 { X: rect.w, Y: rect.h }, radius * 1.5, color)?;
            face.SetOffset(Vector3 { X: rect.x, Y: rect.y, Z: 0.0 })?;
            children.InsertAtTop(&face)?;
            self.box_faces.push(brush);
        }
        Ok(())
    }

    /// A band's tile area, panel-local: the whole box minus the header above it.
    fn tiles_of(&self, band: &Band) -> GridRect {
        let rect = band.rect.shifted_by(self.scroll);
        let first = self.layout.tile_rect(band.first, self.scroll);
        let pad = self.config.grid.gap * self.scale() * 0.5;
        let top = (first.y - pad).max(rect.y);
        GridRect { x: rect.x, y: top, w: rect.w, h: (rect.y + rect.h - top).max(0.0) }
    }

    /// The option tiles for the box being edited, over the middle of the panel.
    ///
    /// Built as real tiles rather than drawn into the header: same size, same
    /// corner, same hover, because they are aimed at the same way.
    fn build_options(&mut self, radius: f32) -> Result<()> {
        self.options.clear();
        self.options_plate = None;
        self.hover_option = None;
        let placed = self.edit_controls();
        if placed.is_empty() {
            return Ok(());
        }

        let children = self.content.Children()?;
        let colors = self.text_colors();
        let idle = color_of(&self.config.theme.tile_alt);
        let panel = self.layout.panel;
        let state = self.edit_state();

        // Two layers of separation, because the options are a different kind of
        // thing from the grid they sit on and must not read as more tiles.
        //
        // A sheet over the whole panel first: it pushes the grid back and makes
        // the middle of the screen the only lit thing.
        let (scrim, _) = self.rounded_rect(
            Vector2 { X: panel.w, Y: panel.h },
            radius * 1.5,
            veil(color_of(&self.config.theme.panel), 0.82),
        )?;
        children.InsertAtTop(&scrim)?;

        // Then a plate under the squares themselves, so they are one object
        // rather than eight floating ones.
        let bounds = surround(placed.iter().map(|(_, rect)| *rect), self.config.grid.gap * self.scale());
        let (plate, _) = self.rounded_rect(
            Vector2 { X: bounds.w, Y: bounds.h },
            radius * 2.0,
            veil(color_of(&self.config.theme.tile), 0.96),
        )?;
        plate.SetOffset(Vector3 { X: bounds.x, Y: bounds.y, Z: 0.0 })?;
        children.InsertAtTop(&plate)?;
        self.options_plate = Some(bounds);

        for (control, rect) in placed {
            let allowed = self.allows(control);
            let root = self.compositor.CreateContainerVisual()?;
            root.SetSize(Vector2 { X: rect.w, Y: rect.h })?;
            root.SetOffset(Vector3 { X: rect.x, Y: rect.y, Z: 0.0 })?;

            // Kept in place rather than removed. The squares must not reshuffle
            // as the answer changes: a control that moves is one a gaze pointer
            // has to hunt for again.
            let holds = state.as_ref().is_some_and(|state| control.holds(state));
            let face_color = if holds {
                color_of(&self.config.theme.tile_selected)
            } else if allowed {
                idle
            } else {
                veil(idle, 0.35)
            };
            let (face, brush) =
                self.rounded_rect(Vector2 { X: rect.w, Y: rect.h }, radius, face_color)?;
            root.Children()?.InsertAtTop(&face)?;

            let colors = if allowed {
                colors
            } else {
                TextColors { title: dim(colors.detail), detail: dim(colors.detail) }
            };
            if let Some(renderer) = &self.renderer
                && let Ok(surface) = renderer.create_surface(rect.w, rect.h)
            {
                let (glyph, label) = match &state {
                    Some(state) => control.wording(state),
                    None => (control.glyph(), control.label()),
                };
                let paint = OptionPaint {
                    width: rect.w,
                    height: rect.h,
                    glyph,
                    label,
                    colors,
                    icon: None,
                };
                if let Err(e) = renderer.draw_option(&surface, paint) {
                    log_warn!("could not draw the \"{}\" option: {e}", control.label());
                } else {
                    let sprite = self.compositor.CreateSpriteVisual()?;
                    sprite.SetSize(Vector2 { X: rect.w, Y: rect.h })?;
                    sprite.SetBrush(&self.compositor.CreateSurfaceBrushWithSurface(&surface)?)?;
                    root.Children()?.InsertAtTop(&sprite)?;
                }
            }

            children.InsertAtTop(&root)?;
            self.options.push((control, rect, brush));
        }
        Ok(())
    }

    /// The app's own button. Always drawn, always in the same corner.
    ///
    /// So nothing needs a right-click to be found. A menu you have to know
    /// about is a menu that is not there for someone meeting the app.
    fn build_home(&mut self, radius: f32) -> Result<()> {
        self.home = None;
        self.hover_home = false;
        self.home_surface = None;
        self.home_awaiting_icon = false;
        let Some(renderer) = &self.renderer else { return Ok(()) };

        let scale = self.scale();
        let g = &self.config.grid;
        let rect = home_button(
            GridRect { x: 0.0, y: 0.0, w: self.layout.panel.w, h: self.layout.panel.h },
            g.tile_width * scale,
            g.tile_height * scale,
            g.gap * scale,
        );
        if rect.w < 24.0 || rect.h < 24.0 {
            return Ok(());
        }

        let (label, glyph) = if self.editing {
            ("Stop editing", "\u{2713}")
        } else {
            ("BentoPick", "\u{25A6}")
        };

        // Our own logo on our own button, off the same cache the tiles use.
        // `None` on a cold summon - it is queued like any other icon - so the
        // glyph stands in and `fill_home_icon` paints the logo over it.
        // Editing borrows the button for "Stop editing", which is not us.
        let icon = (!self.editing).then(|| app_icon(self.icon_size())).flatten();

        let children = self.content.Children()?;
        let root = self.compositor.CreateContainerVisual()?;
        root.SetSize(Vector2 { X: rect.w, Y: rect.h })?;
        root.SetOffset(Vector3 { X: rect.x, Y: rect.y, Z: 0.0 })?;

        let (face, brush) = self.rounded_rect(
            Vector2 { X: rect.w, Y: rect.h },
            radius,
            color_of(&self.config.theme.tile_selected),
        )?;
        root.Children()?.InsertAtTop(&face)?;

        let mut painted = None;
        if let Ok(surface) = renderer.create_surface(rect.w, rect.h)
            && renderer
                .draw_option(
                    &surface,
                    OptionPaint {
                        width: rect.w,
                        height: rect.h,
                        glyph,
                        label,
                        colors: self.text_colors(),
                        icon: icon.as_deref(),
                    },
                )
                .is_ok()
        {
            let sprite = self.compositor.CreateSpriteVisual()?;
            sprite.SetSize(Vector2 { X: rect.w, Y: rect.h })?;
            sprite.SetBrush(&self.compositor.CreateSurfaceBrushWithSurface(&surface)?)?;
            root.Children()?.InsertAtTop(&sprite)?;
            painted = Some(surface);
        }

        children.InsertAtTop(&root)?;
        self.home = Some((rect, brush));
        self.home_awaiting_icon = icon.is_none() && !self.editing && painted.is_some();
        self.home_surface = painted;
        Ok(())
    }

    /// The big menu, same squares as the edit options and for the same reason.
    fn build_menu(&mut self, radius: f32) -> Result<()> {
        self.menu_items.clear();
        self.hover_menu = None;
        if !self.menu_open_big {
            return Ok(());
        }

        let scale = self.scale();
        let g = &self.config.grid;
        let panel = self.layout.panel;
        let placed = commands(
            GridRect { x: 0.0, y: 0.0, w: panel.w, h: panel.h },
            g.tile_width * scale,
            g.tile_height * scale,
            g.gap * scale,
        );

        let children = self.content.Children()?;
        let colors = self.text_colors();
        let idle = color_of(&self.config.theme.tile_alt);

        // Same two layers of separation the edit options get: the menu is a
        // different kind of thing from the grid and must not read as more tiles.
        let (scrim, _) = self.rounded_rect(
            Vector2 { X: panel.w, Y: panel.h },
            radius * 1.5,
            veil(color_of(&self.config.theme.panel), 0.82),
        )?;
        children.InsertAtTop(&scrim)?;

        let bounds = surround(placed.iter().map(|(_, rect)| *rect), g.gap * scale);
        let (plate, _) = self.rounded_rect(
            Vector2 { X: bounds.w, Y: bounds.h },
            radius * 2.0,
            veil(color_of(&self.config.theme.tile), 0.96),
        )?;
        plate.SetOffset(Vector3 { X: bounds.x, Y: bounds.y, Z: 0.0 })?;
        children.InsertAtTop(&plate)?;

        for (command, rect) in placed {
            let root = self.compositor.CreateContainerVisual()?;
            root.SetSize(Vector2 { X: rect.w, Y: rect.h })?;
            root.SetOffset(Vector3 { X: rect.x, Y: rect.y, Z: 0.0 })?;

            let (face, brush) =
                self.rounded_rect(Vector2 { X: rect.w, Y: rect.h }, radius, idle)?;
            root.Children()?.InsertAtTop(&face)?;

            if let Some(renderer) = &self.renderer
                && let Ok(surface) = renderer.create_surface(rect.w, rect.h)
                && renderer
                    .draw_option(
                        &surface,
                        OptionPaint {
                            width: rect.w,
                            height: rect.h,
                            glyph: command.glyph(),
                            label: command.label(),
                            colors,
                            icon: None,
                        },
                    )
                    .is_ok()
            {
                let sprite = self.compositor.CreateSpriteVisual()?;
                sprite.SetSize(Vector2 { X: rect.w, Y: rect.h })?;
                sprite.SetBrush(&self.compositor.CreateSurfaceBrushWithSurface(&surface)?)?;
                root.Children()?.InsertAtTop(&sprite)?;
            }

            children.InsertAtTop(&root)?;
            self.menu_items.push((command, rect, brush));
        }
        Ok(())
    }

    fn refresh_menu(&self) {
        let idle = color_of(&self.config.theme.tile_alt);
        let hot = color_of(&self.config.theme.tile_hover);
        for (index, (_, _, brush)) in self.menu_items.iter().enumerate() {
            let _ = brush.SetColor(if self.hover_menu == Some(index) { hot } else { idle });
        }
        if let Some((_, brush)) = &self.home {
            let base = color_of(&self.config.theme.tile_selected);
            let _ = brush.SetColor(if self.hover_home { hot } else { base });
        }
    }


    /// The settings surface. The same squares again: this is the third place
    /// they appear, and the sameness is the point - one shape, one way to aim
    /// at it, wherever you are in the app.
    fn build_settings(&mut self, radius: f32) -> Result<()> {
        self.settings_items.clear();
        self.hover_setting = None;
        if !self.settings_open {
            return Ok(());
        }

        let scale = self.scale();
        let g = &self.config.grid;
        let panel = self.layout.panel;
        let placed = centred_grid(
            GridRect { x: 0.0, y: 0.0, w: panel.w, h: panel.h },
            SETTINGS.len(),
            g.tile_width * scale,
            g.tile_height * scale,
            g.gap * scale,
        );

        let children = self.content.Children()?;
        let colors = self.text_colors();
        let idle = color_of(&self.config.theme.tile_alt);

        let (scrim, _) = self.rounded_rect(
            Vector2 { X: panel.w, Y: panel.h },
            radius * 1.5,
            veil(color_of(&self.config.theme.panel), 0.82),
        )?;
        children.InsertAtTop(&scrim)?;

        let bounds = surround(placed.iter().copied(), g.gap * scale);
        let (plate, _) = self.rounded_rect(
            Vector2 { X: bounds.w, Y: bounds.h },
            radius * 2.0,
            veil(color_of(&self.config.theme.tile), 0.96),
        )?;
        plate.SetOffset(Vector3 { X: bounds.x, Y: bounds.y, Z: 0.0 })?;
        children.InsertAtTop(&plate)?;

        for (setting, rect) in SETTINGS.iter().copied().zip(placed) {
            let root = self.compositor.CreateContainerVisual()?;
            root.SetSize(Vector2 { X: rect.w, Y: rect.h })?;
            root.SetOffset(Vector3 { X: rect.x, Y: rect.y, Z: 0.0 })?;

            let (face, brush) =
                self.rounded_rect(Vector2 { X: rect.w, Y: rect.h }, radius, idle)?;
            root.Children()?.InsertAtTop(&face)?;

            if let Some(renderer) = &self.renderer
                && let Ok(surface) = renderer.create_surface(rect.w, rect.h)
                && renderer
                    .draw_option(
                        &surface,
                        OptionPaint {
                            width: rect.w,
                            height: rect.h,
                            glyph: setting.glyph(),
                            label: setting.label(&self.config),
                            colors,
                            icon: None,
                        },
                    )
                    .is_ok()
            {
                let sprite = self.compositor.CreateSpriteVisual()?;
                sprite.SetSize(Vector2 { X: rect.w, Y: rect.h })?;
                sprite.SetBrush(&self.compositor.CreateSurfaceBrushWithSurface(&surface)?)?;
                root.Children()?.InsertAtTop(&sprite)?;
            }

            children.InsertAtTop(&root)?;
            self.settings_items.push((setting, rect, brush));
        }
        Ok(())
    }

    fn refresh_settings(&self) {
        let idle = color_of(&self.config.theme.tile_alt);
        let hot = color_of(&self.config.theme.tile_hover);
        for (index, (_, _, brush)) in self.settings_items.iter().enumerate() {
            let _ = brush.SetColor(if self.hover_setting == Some(index) { hot } else { idle });
        }
    }

    /// One settings square was clicked.
    ///
    /// The surface stays up, unlike the menu: settings are the one thing you
    /// do several of in a row, and closing after each would mean reopening to
    /// see what the last click did.
    fn run_setting(&mut self, setting: Setting) {
        match setting {
            Setting::Done => {
                self.settings_open = false;
                let _ = self.rebuild_visuals();
                return;
            }
            Setting::OpenFile => {
                self.settings_open = false;
                open_config();
                let _ = self.rebuild_visuals();
                return;
            }
            _ => {}
        }

        let Some(change) = setting.next(&self.config) else { return };
        if !pins::set(change) {
            log_warn!("could not write the {} setting", setting.label(&self.config));
            return;
        }
        // Read straight back rather than waiting on the watcher. The square has
        // to say its new value on the click that caused it, and re-reading the
        // file is what keeps the panel and the file from ever disagreeing.
        self.reload_config();
        log_info!("setting: {}", setting.label(&self.config));
        let _ = self.rebuild_visuals();
        self.keep_focus();
    }

    /// Whatever the big menu was asked for. Each one closes it: none of them
    /// are things you do twice in a row.
    fn run_command(&mut self, command: Command) {
        self.menu_open_big = false;
        match command {
            Command::EditLayout => {
                self.enter_edit();
                return;
            }
            Command::AddApp => {
                let picked = picker::pick_app(self.hwnd);
                self.pin(picked);
                return;
            }
            Command::AddFolder => {
                let picked = picker::pick_folder(self.hwnd);
                self.pin(picked);
                return;
            }
            Command::AddFile => {
                let picked = picker::pick_file(self.hwnd);
                self.pin(picked);
                return;
            }
            // The squares, not the file. `Open the file` is one of them, for
            // everything they do not cover.
            Command::Settings => self.settings_open = true,
            Command::Close => {}
        }
        let _ = self.rebuild_visuals();
    }

    /// The app's own button was clicked: stop editing, or open and close the
    /// big menu.
    fn press_home(&mut self) {
        if self.editing {
            self.leave_edit();
            return;
        }
        // Backs out one surface at a time, the same order Escape unwinds in.
        if self.settings_open {
            self.settings_open = false;
            let _ = self.rebuild_visuals();
            self.keep_focus();
            return;
        }
        self.menu_open_big = !self.menu_open_big;
        let _ = self.rebuild_visuals();
        self.keep_focus();
    }

    /// Repaint the option tiles in place, so hovering one does not rebuild the
    /// overlay out from under the pointer.
    fn refresh_options(&self) {
        let idle = color_of(&self.config.theme.tile_alt);
        let hot = color_of(&self.config.theme.tile_hover);
        let lit = color_of(&self.config.theme.tile_selected);
        let state = self.edit_state();
        for (index, (control, _, brush)) in self.options.iter().enumerate() {
            let color = if self.hover_option == Some(index) && self.allows(*control) {
                hot
            } else if state.as_ref().is_some_and(|state| control.holds(state)) {
                lit
            } else if self.allows(*control) {
                idle
            } else {
                veil(idle, 0.35)
            };
            let _ = brush.SetColor(color);
        }
    }

    /// Returns whether the pointer is over the overlay at all, so the box
    /// underneath does not light up through it.
    fn set_hover_option(&mut self, x: f32, y: f32) -> bool {
        let next = self
            .options
            .iter()
            .position(|(_, rect, _)| rect.contains(x, y))
            .filter(|index| self.options.get(*index).is_some_and(|(c, _, _)| self.allows(*c)));
        if next != self.hover_option {
            self.hover_option = next;
            self.refresh_options();
        }
        self.options_plate.is_some_and(|plate| plate.contains(x, y))
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

    /// How much of the grid survived the query, for the filter strip.
    fn match_count(&self) -> String {
        match self.items.len() {
            0 => "no matches".into(),
            shown => format!("{shown} of {}", self.total),
        }
    }

    /// The strip swallows clicks. Dismissing on a click there would read as a
    /// bug. Whole row, not just the drawn text.
    fn search_hit(&self, y: f32) -> bool {
        !self.query.is_empty() && y >= 0.0 && y < self.layout.search_rect().h
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

    /// A ring, not a filled rect: it goes over a tile that already has a fill
    /// from hover or selection and must not replace it.
    fn rounded_ring(&self, size: Vector2, radius: f32, color: Color) -> Result<ShapeVisual> {
        let geometry = self.compositor.CreateRoundedRectangleGeometry()?;
        geometry.SetSize(Vector2 {
            X: (size.X - TARGET_STROKE).max(0.0),
            Y: (size.Y - TARGET_STROKE).max(0.0),
        })?;
        geometry.SetCornerRadius(Vector2 { X: radius, Y: radius })?;

        let shape: CompositionSpriteShape =
            self.compositor.CreateSpriteShapeWithGeometry(&geometry)?;
        shape.SetStrokeBrush(&self.compositor.CreateColorBrushWithColor(color)?)?;
        shape.SetStrokeThickness(TARGET_STROKE)?;
        // The stroke is centred on the path, so half of it would fall outside.
        shape.SetOffset(Vector2 { X: TARGET_STROKE / 2.0, Y: TARGET_STROKE / 2.0 })?;

        let visual = self.compositor.CreateShapeVisual()?;
        visual.SetSize(size)?;
        visual.Shapes()?.Append(&shape)?;
        Ok(visual)
    }

    /// Repositions existing visuals after a scroll, without rebuilding them.
    fn reposition(&self) {
        for (index, tile) in self.tiles.iter().enumerate() {
            let rect = self.layout.tile_rect(index, self.scroll);
            let _ = tile.root.SetOffset(Vector3 { X: rect.x, Y: rect.y, Z: 0.0 });
        }
        for (visual, (_, rect, _)) in self.headers.iter().zip(self.layout.headers(self.scroll)) {
            let _ = visual.SetOffset(Vector3 { X: rect.x, Y: rect.y, Z: 0.0 });
        }
    }

    /// Hover beats selection: a tile that did not light up under the pointer
    /// reads as dead.
    fn tile_color(&self, index: usize) -> Color {
        let theme = &self.config.theme;
        color_of(if self.hover == Some(index) {
            &theme.tile_hover
        } else if self.lit(index) {
            &theme.tile_drag
        } else if self.selected == Some(index) {
            &theme.tile_selected
        } else if self.alternating(index) {
            &theme.tile_alt
        } else {
            &theme.tile
        })
    }

    /// The window the move tiles will act on, and the latch while it is
    /// holding the panel open. One fill for both: they are the same statement,
    /// that this tile is what the bar is pointed at.
    fn lit(&self, index: usize) -> bool {
        self.items
            .get(index)
            .is_some_and(|item| item.target == Target::Stay && self.stay)
    }

    /// The tile the move bar acts on, ringed so it is answerable at a glance.
    /// Its own tile, and any pin of the same app: both are that window here.
    fn is_target(&self, item: &Item) -> bool {
        self.moving().is_some_and(|handle| self.window_for(item) == Some(handle))
    }

    /// Merging cost the groups their headers. Alternating the fill is what is
    /// left to say "these belong together and those do not" — browser windows
    /// and the tabs after them read as one block, the rest as another.
    ///
    /// Banded by parity of the group's position, not by source: two groups can
    /// share a source and still need telling apart.
    fn alternating(&self, index: usize) -> bool {
        self.items.get(index).is_some_and(|item| item.group % 2 == 1)
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

        // The action tiles work the panel rather than leaving it.
        match item.target {
            Target::Stay => {
                self.toggle_stay();
                return;
            }
            Target::Arrange(mv) => {
                self.arrange(mv);
                return;
            }
            // Held open, a click picks what to move rather than leaving for
            // it. A pin resolves to the window its app already has open: the
            // Discord in Launch and the Discord in Active are the same app,
            // and clicking either has to mean the same thing here.
            // Not a window, so holding the panel open has nothing to pick from
            // it. Falls through and opens, the way it would with stay off.
            Target::NewTab { .. } => {}
            _ if self.stay => {
                self.pick(&item);
                return;
            }
            _ => {}
        }

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

    /// Stay open mode, on or off. The tile and ctrl are two ways into the one
    /// flag, so neither can disagree with the other about what is on.
    fn toggle_stay(&mut self) {
        self.stay = !self.stay;
        if !self.stay {
            self.target = None;
            self.pending = None;
        }
        log_info!("stay open {}", if self.stay { "on" } else { "off" });
        self.frame_target();
        let _ = self.rebuild_visuals();
    }

    /// Take a tile as the thing to move, without leaving for it.
    fn pick(&mut self, item: &Item) {
        self.stay = true;
        if let Some(handle) = self.window_for(item) {
            self.target = Some(handle);
            self.pending = None;
            log_info!("moving \"{}\"", item.title);
        } else if matches!(item.kind, Kind::App | Kind::Folder) {
            // Nothing of it open. Start it and take its window when it turns
            // up, so picking an app works from cold as well as from running.
            self.pending = item.app.clone();
            self.target = None;
            if self.config.dry_run {
                log_dry!("would {}", item.activation_summary());
            } else {
                activate::activate(item);
            }
            log_info!("waiting for \"{}\" to open", item.title);
        } else {
            // A tab or a link is not a window. bentopick cannot map a tab onto
            // an HWND, which is why the browser raises its own.
            log_info!("nothing to move for \"{}\"", item.title);
        }
        self.frame_target();
        let _ = self.rebuild_visuals();
    }

    /// Put the desktop ring around whatever the bar acts on, or take it away
    /// when there is nothing to point at.
    fn frame_target(&mut self) {
        if !self.visible {
            if let Some(ring) = self.spotlight.as_mut() {
                ring.hide();
            }
            return;
        }
        if self.spotlight.is_none() {
            match Spotlight::create(rgb_of(&self.config.theme.tile_target)) {
                Ok(ring) => self.spotlight = Some(ring),
                // A ring that will not build costs the feature, not the panel.
                Err(e) => {
                    log_warn!("could not build the target ring: {e}");
                    return;
                }
            }
        }
        let frame = self.moving().and_then(arrange::visible_frame);
        let (Some(ring), panel) = (self.spotlight.as_mut(), self.hwnd) else {
            return;
        };
        match frame {
            Some(frame) => ring.show(frame, panel),
            None => ring.hide(),
        }
    }

    /// The live window behind a tile. A pin has none of its own, so it answers
    /// through the app both it and the window name.
    fn window_for(&self, item: &Item) -> Option<Handle> {
        if let Target::Window(handle) = item.target {
            return Some(handle);
        }
        self.window_named(item.app.as_deref()?)
    }

    /// First is most recent: the store orders windows roughly by Z.
    fn window_named(&self, app: &str) -> Option<Handle> {
        self.items.iter().find_map(|other| match other.target {
            Target::Window(handle) if other.app.as_deref() == Some(app) => Some(handle),
            _ => None,
        })
    }

    /// Move the target window and stay up. These get clicked in runs: left,
    /// then up, then a screen over.
    fn arrange(&mut self, mv: arrange::Move) {
        let Some((handle, title)) = self.arrange_target() else {
            log_info!("nothing to move {}", mv.key());
            return;
        };
        self.arranging = true;
        let moved = arrange::apply(handle, mv, &title, self.config.dry_run);
        // The window went somewhere. The ring has to go with it.
        self.frame_target();
        // SAFETY: our own window. Only ever called back to back with a move
        // that may have taken foreground away.
        if moved {
            unsafe {
                let _ = SetForegroundWindow(self.hwnd);
            }
        }
        self.arranging = false;
    }

    /// The window the move tiles act on: the pick if there is one, otherwise
    /// whatever the panel came up over. The fallback is what makes the common
    /// case free, and the ring is what stops it being a guess.
    fn moving(&self) -> Option<Handle> {
        let handle = self.target.or_else(|| {
            (!self.caller.is_invalid() && self.caller != self.hwnd).then(|| Handle::new(self.caller))
        })?;
        arrange::movable(handle).then_some(handle)
    }

    /// A move tile with nothing to act on. Drawn dim rather than taken away:
    /// these tiles are found by aiming at where they were last time, so the bar
    /// keeps its shape and says it is unavailable instead.
    fn inert(&self, item: &Item) -> bool {
        matches!(item.target, Target::Arrange(_)) && self.moving().is_none()
    }

    /// The window the move tiles act on, and what to call it in the log.
    fn arrange_target(&self) -> Option<(Handle, String)> {
        let handle = self.moving()?;
        let title = self
            .items
            .iter()
            .find(|item| item.target == Target::Window(handle))
            .map_or_else(|| format!("{:#x}", handle.raw()), |item| item.title.clone());
        Some((handle, title))
    }

    /// The figure on an action tile, and what to call it now. `None` twice
    /// over for an ordinary tile, which has an icon and a fixed name.
    fn action_face(&self, item: &Item) -> (Option<Mark>, Option<String>) {
        match item.target {
            Target::Arrange(mv) => (Some(mark_of(mv)), None),
            Target::NewTab { .. } => (Some(Mark::Plus), None),
            Target::Stay => {
                let name = self.stay.then(|| {
                    self.target
                        .and_then(|handle| {
                            self.items.iter().find(|i| i.target == Target::Window(handle))
                        })
                        .map_or_else(|| "Pick a window".to_owned(), |i| i.title.clone())
                });
                (Some(Mark::Latch { on: self.stay }), name)
            }
            _ => (None, None),
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
                // Only reached by a tile waiting on an icon, which an action
                // tile never is.
                mark: None,
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
        self.fill_home_icon();
    }


    /// The corner button: built before the shell
    /// worker has our own icon, repainted over the glyph when it lands.
    fn fill_home_icon(&mut self) {
        if !self.home_awaiting_icon {
            return;
        }
        let colors = self.text_colors();
        let Some(icon) = app_icon(self.icon_size()) else { return };
        let Some(renderer) = &self.renderer else { return };
        let Some(surface) = &self.home_surface else { return };
        let Some((rect, _)) = &self.home else { return };

        let drawn = renderer
            .draw_option(
                surface,
                OptionPaint {
                    width: rect.w,
                    height: rect.h,
                    glyph: "",
                    label: "BentoPick",
                    colors,
                    icon: Some(&icon),
                },
            )
            .is_ok();
        if drawn {
            self.home_awaiting_icon = false;
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

        // The app picked a moment ago has opened. It is what the bar was
        // pointed at, so take it.
        if let Some(stem) = self.pending.take() {
            match self.window_named(&stem) {
                Some(handle) => {
                    self.target = Some(handle);
                    log_info!("moving the window {stem} just opened");
                }
                None => self.pending = Some(stem),
            }
        }

        // `reload` picks the query's best match, which is wrong when a window
        // merely opened. Put the selection back if it still exists.
        if let Some(id) = held
            && let Some(moved) = self.items.iter().position(|i| i.id == id)
        {
            self.selected = Some(moved);
        }

        self.frame_target();

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
        // Edit mode owns the keyboard. Its keys are all virtual-key ones, so
        // anything reaching here would only start a filter the mode then hides.
        if self.editing {
            return;
        }

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
    fn on_key(&mut self, vk: u16, repeat: bool) -> bool {
        if self.editing {
            return self.edit_key(vk);
        }

        const CTRL: u16 = VK_CONTROL.0;
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
            // The keyboard's way into the same latch the tile is.
            CTRL => {
                if !repeat {
                    self.toggle_stay();
                }
                true
            }
            // Query first, panel second. Backspacing out of a long mistyped
            // filter is not what Escape is reached for.
            ESCAPE => {
                // Menu first, then query, then the panel. Each Escape undoes
                // one thing rather than throwing the whole panel away.
                if self.settings_open {
                    self.settings_open = false;
                    let _ = self.rebuild_visuals();
                } else if self.menu_open_big {
                    self.menu_open_big = false;
                    let _ = self.rebuild_visuals();
                } else if self.stay {
                    self.stay = false;
                    self.target = None;
                    let _ = self.rebuild_visuals();
                } else if self.query.is_empty() {
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

    // --- layout edit mode ---

    /// Entered from the panel's own right-click menu. Never while a filter is
    /// live: the shape being edited is the unfiltered one, and half the boxes
    /// are missing from the other.
    fn enter_edit(&mut self) {
        if !self.query.is_empty() || self.sections.is_empty() {
            return;
        }
        self.editing = true;
        // Nothing picked yet. The options belong to a box, so they wait for one
        // to be clicked rather than guessing at the first.
        self.edit = None;
        self.set_hover(None);
        self.set_selected(None);
        self.reload();
        log_info!("edit layout: on, {} box(es); click one", self.layout.bands().len());
        let _ = self.rebuild_visuals();
        self.reposition();
    }

    /// Put the keyboard and the mouse back on the panel.
    ///
    /// Every option rewrites the config and rebuilds the grid, and a window
    /// that has lost activation swallows the next click waking up - which is
    /// why one click in three seemed to do nothing and the second one worked.
    fn keep_focus(&self) {
        // SAFETY: our own window, on its owning thread.
        unsafe {
            let _ = SetActiveWindow(self.hwnd);
        }
    }

    /// Leaves the panel up. Arranging the boxes is something you do on the way
    /// to using them, so finishing should hand back a working panel rather
    /// than dismissing it.
    fn leave_edit(&mut self) {
        self.editing = false;
        self.edit = None;
        self.hover_box = None;
        self.reload();
        log_info!("edit layout: off");
        let _ = self.rebuild_visuals();
        self.reposition();

        self.keep_focus();
    }

    fn edit_title(&self) -> Option<String> {
        let index = self.edit?;
        Some(self.sections.get(index)?.title.clone())
    }

    /// Sections that actually have a box on screen, in order.
    ///
    /// An emptied section stays in `sections` so unpin can still resolve a band
    /// to it, but the layout skips it. Edit mode moves between boxes, so it
    /// walks this rather than counting section indices.
    fn boxes(&self) -> Vec<usize> {
        self.layout.bands().iter().map(|band| band.section).collect()
    }

    /// The option tiles for the selected box. One place, so the drawn rect and
    /// the clicked rect are the same rect.
    fn edit_controls(&self) -> Vec<(Control, GridRect)> {
        if self.edit.is_none() {
            return Vec::new();
        }
        let scale = self.scale();
        let g = &self.config.grid;
        controls(
            GridRect { x: 0.0, y: 0.0, w: self.layout.panel.w, h: self.layout.panel.h },
            g.tile_width * scale,
            g.tile_height * scale,
            g.gap * scale,
        )
    }

    /// Run whatever the clicked button means. Every one of these is also a key,
    /// but the button is the way it is meant to be reached.
    fn apply_control(&mut self, control: Control) {
        // An option that is not offered is not applied either. The overlay
        // greys them out; this is the half that makes it true.
        if !self.allows(control) {
            return;
        }
        let shown = self.edited_state().1;
        let placed = self.edit_title().map(|title| self.placement_of(&title));
        let at = placed
            .as_ref()
            .and_then(|placement| placement.at.as_deref())
            .and_then(At::parse);

        match control {
            Control::Done => self.leave_edit(),
            Control::Fewer => {
                self.edit_placement(|p| p.max_items = shown.saturating_sub(1).max(1));
            }
            Control::More => {
                self.edit_placement(|p| p.max_items = shown + 1);
            }
            // Claiming a side takes the whole of it: anything else sitting
            // there is moved off, so the button does what its label says. A
            // box claiming the left becomes the full-height left of the panel.
            Control::ClaimLeft
            | Control::ClaimRight
            | Control::ClaimTop
            | Control::ClaimBottom => {
                let (Some(side), Some(title)) = (control.side(), self.edit_title()) else {
                    return;
                };
                // The lit square is the side the box already holds, so clicking
                // it gives that side back. One button, both directions, and the
                // state is visible instead of being something to remember.
                let holds = self
                    .edit_state()
                    .is_some_and(|state| control.holds(&state));
                if holds {
                    self.edit_placement(|p| p.at = None);
                } else {
                    let keep = at.map_or(0.0, |at| at.share);
                    if pins::claim_side(&title, side.word(), keep) {
                        self.reload_config();
                    }
                }
            }
            Control::MoveUp | Control::MoveDown => {
                let Some(title) = self.edit_title() else { return };
                let delta = if control == Control::MoveUp { -1 } else { 1 };
                if pins::move_section(&title, delta) {
                    self.reload_config();
                    // Follow the section rather than the slot: it is the box
                    // being moved that should stay selected.
                    self.edit = self
                        .sections
                        .iter()
                        .position(|section| section.title == title)
                        .or(self.edit);
                }
            }
            Control::Grow | Control::Shrink => {
                // Worked out by the same rule that decided this button was
                // available, so the two cannot disagree about where the edge
                // is now.
                let (Some(state), Some(mut at)) = (self.edit_state(), at) else {
                    return;
                };
                let Some(share) = control.resized(&state) else { return };
                at.share = share;
                let spelled = at.spell();
                self.edit_placement(|p| p.at = Some(spelled));
            }
        }
    }

    /// Columns the selected box currently occupies, and tiles it currently
    /// shows. Both as laid out, which is what the user is looking at.
    fn edited_state(&self) -> (usize, usize) {
        let cols = self
            .layout
            .bands()
            .iter()
            .find(|band| Some(band.section) == self.edit)
            .map_or(1, |band| band.cols);
        let shown = self.edit.and_then(|s| self.sections.get(s)).map_or(0, |s| s.items.len());
        (cols, shown)
    }

    /// What the selected box currently is. `None` when nothing is picked.
    fn edit_state(&self) -> Option<BoxState> {
        let section = self.edit?;
        let title = self.sections.get(section)?.title.clone();
        // The boxes with no claimed side, in the order they stack into what is
        // left over. Moving up and down walks this, not the whole list.
        let stack: Vec<&str> = self
            .sections
            .iter()
            .filter(|s| self.placement_of(&s.title).at.is_none())
            .map(|s| s.title.as_str())
            .collect();
        let at_stack = stack.iter().position(|name| *name == title).unwrap_or(0);

        let at = self.placement_of(&title).at.as_deref().and_then(At::parse);
        let band = self
            .layout
            .bands()
            .iter()
            .find(|band| band.section == section);
        let panel = self.layout.panel;
        // The fraction it fills along whichever way its cut runs.
        let share_now = band.map_or(0.5, |band| {
            if at.as_ref().is_some_and(At::splits_width) {
                band.rect.w / panel.w.max(1.0)
            } else {
                band.rect.h / self.layout.content_h.max(1.0)
            }
        });

        Some(BoxState {
            shown: self.sections.get(section).map_or(0, |s| s.items.len()),
            total: self.sections.get(section).map_or(0, |s| s.total),
            at,
            boxes: self.layout.bands().len(),
            at_stack,
            stack: stack.len(),
            cols: band.map_or(1, |band| band.cols),
            panel_cols: self.layout.cols,
            share_now,
        })
    }

    fn allows(&self, control: Control) -> bool {
        self.edit_state()
            .is_some_and(|state| control.allowed(&state))
    }

    /// A click while editing: a button if it hit one, otherwise the box under it.
    fn edit_click(&mut self, x: f32, y: f32) {
        // The overlay is on top, so it answers first - and it answers for its
        // whole plate, greyed squares and the gaps between them included.
        // Anything else clicks through to a box behind the thing being clicked,
        // which silently moves the selection somewhere else.
        if self.options_plate.is_some_and(|plate| plate.contains(x, y)) {
            let hit = self
                .options
                .iter()
                .find(|(_, rect, _)| rect.contains(x, y))
                .map(|(control, _, _)| *control);
            match hit {
                Some(control) if self.allows(control) => {
                    log_info!("edit layout: {control:?}");
                    self.apply_control(control);
                }
                Some(control) => log_info!("edit layout: {control:?} does not apply here"),
                None => {}
            }
            self.keep_focus();
            return;
        }
        log_info!("edit layout: pick box at {x:.0},{y:.0}");
        self.pick_box(x, y);
        self.keep_focus();
    }

    /// Selected beats hovered: the box the buttons belong to has to stay
    /// obvious while the pointer wanders over its neighbours.
    fn box_color(&self, band: usize) -> Color {
        let section = self.layout.bands().get(band).map(|band| band.section);
        let theme = &self.config.theme;
        if section == self.edit {
            veil(color_of(&theme.tile_selected), 0.62)
        } else if section == self.hover_box {
            veil(color_of(&theme.tile_hover), 0.52)
        } else {
            // Idle. Nearly opaque over the panel colour, so the tiles beneath
            // read as texture rather than as buttons.
            veil(color_of(&theme.panel), 0.72)
        }
    }

    /// Repaint the box faces in place. Rebuilding the grid on every mouse move
    /// would tear the thing being pointed at out from under the pointer.
    fn refresh_boxes(&self) {
        for (band, brush) in self.box_faces.iter().enumerate() {
            let _ = brush.SetColor(self.box_color(band));
        }
    }

    fn set_hover_box(&mut self, x: f32, y: f32) {
        let content_y = y + self.scroll;
        let next = self
            .layout
            .bands()
            .iter()
            .find(|band| band.rect.contains(x, content_y))
            .map(|band| band.section);
        if next != self.hover_box {
            self.hover_box = next;
            self.refresh_boxes();
        }
    }

    /// Point edit mode at whichever box covers a panel-local point. Bands tile
    /// the panel, so anywhere inside it answers.
    fn pick_box(&mut self, x: f32, y: f32) {
        let content_y = y + self.scroll;
        let Some(band) = self
            .layout
            .bands()
            .iter()
            .find(|band| band.rect.contains(x, content_y))
        else {
            return;
        };
        // Clicking the picked box again puts the options away. The overlay sits
        // over the middle of the panel, so there has to be a way to clear it
        // without leaving the mode.
        let section = band.section;
        self.edit = if self.edit == Some(section) { None } else { Some(section) };
        let _ = self.rebuild_visuals();
    }

    /// Step the selection `delta` boxes along, clamped at both ends.
    fn edit_step(&mut self, delta: isize) -> bool {
        let boxes = self.boxes();
        if boxes.is_empty() {
            return true;
        }
        let at = self
            .edit
            .and_then(|section| boxes.iter().position(|&index| index == section))
            .unwrap_or(0);
        let next = (at as isize + delta).clamp(0, boxes.len() as isize - 1) as usize;
        self.edit = Some(boxes[next]);
        let _ = self.rebuild_visuals();
        true
    }

    /// This section's placement as config has it. Edit mode reads config rather
    /// than the computed layout: what a box was *given* and what it ended up
    /// with differ whenever a row was too narrow to honour every request.
    fn placement_of(&self, title: &str) -> pins::Placement {
        self.config
            .sections
            .iter()
            .find(|section| section.title == title)
            .map_or(pins::Placement::default(), |section| pins::Placement {
                at: section.at.clone(),
                columns: section.columns,
                max_items: section.max_items,
            })
    }

    /// Change the selected box's placement and write it. The config watcher
    /// would reload eventually; doing it here is what makes a keypress land on
    /// screen at once.
    fn edit_placement(&mut self, change: impl FnOnce(&mut pins::Placement)) -> bool {
        let Some(title) = self.edit_title() else {
            return true;
        };
        let mut placement = self.placement_of(&title);
        let before = placement.clone();
        change(&mut placement);
        if placement != before && pins::set_placement(&title, placement) {
            self.reload_config();
        }
        true
    }

    /// Every key edit mode takes. Nothing falls through to the filter: a mode
    /// that sometimes types into a search box is a mode that eats work.
    ///
    /// A second path only. The squares are how this is meant to be reached.
    fn edit_key(&mut self, vk: u16) -> bool {
        const ESCAPE: u16 = VK_ESCAPE.0;
        const ENTER: u16 = VK_RETURN.0;
        const LEFT: u16 = VK_LEFT.0;
        const RIGHT: u16 = VK_RIGHT.0;
        const UP: u16 = VK_UP.0;
        const DOWN: u16 = VK_DOWN.0;

        if self.edit.is_none() {
            // Nothing picked: arrows walk the boxes so the mode is reachable
            // without the mouse at all.
            return match vk {
                ESCAPE | ENTER => {
                    self.leave_edit();
                    true
                }
                LEFT => self.edit_step(-1),
                RIGHT => self.edit_step(1),
                _ => false,
            };
        }

        match vk {
            ESCAPE | ENTER => {
                self.leave_edit();
                true
            }
            LEFT => self.click_control(Control::ClaimLeft),
            RIGHT => self.click_control(Control::ClaimRight),
            UP => self.click_control(Control::ClaimTop),
            DOWN => self.click_control(Control::ClaimBottom),
            _ => false,
        }
    }

    /// Always claims the key, whether or not the option applies: an arrow
    /// falling through to the shell is worse than one doing nothing.
    fn click_control(&mut self, control: Control) -> bool {
        self.apply_control(control);
        true
    }

    /// What a header says while its layout is being edited: where the box sits
    /// and how much of its list it is showing.
    fn edit_header(&self, _band: usize, title: &str) -> String {
        let placement = self.placement_of(title);
        let shown = match placement.max_items {
            0 => String::from("all"),
            capped => format!("{capped}"),
        };
        // The path exactly as the config spells it, so anyone wanting a shape
        // the squares cannot reach can read off how to type it.
        let sits = placement.at.as_deref().unwrap_or("fills the rest");

        format!("{title}   {sits} \u{b7} {shown}")
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

    /// Only pins bentopick owns can be removed. A taskbar entry belongs to the
    /// taskbar, and unpinning it there is Windows' business, not bentopick's
    /// (safety rule 3).
    fn removable(&self, tile: usize) -> bool {
        self.items.get(tile).is_some_and(|i| i.origin == Source::Manual)
    }

    /// Window tiles are MRU ordered by the foreground hook, so a saved order
    /// would fight the hook on every focus change. Pinned sections have an order
    /// that is bentopick's to keep.
    ///
    /// Never while filtering: writing back a subset's order would drop every
    /// pin the query hid.
    fn draggable(&self, tile: usize) -> bool {
        self.query.is_empty()
            && self
                .items
                .get(tile)
                .is_some_and(|i| matches!(i.origin, Source::Manual | Source::Taskbar))
    }

    /// The tiles a drag may move within. See `grid::origin_run`.
    fn origin_run(&self, tile: usize) -> Option<(usize, usize)> {
        let band = self.layout.bands().get(self.layout.band_of(tile)?)?;
        let groups: Vec<usize> = self.items.iter().map(|item| item.group).collect();
        Some(origin_run(&groups, band.first, band.count, tile))
    }

    /// Take a press on a tile. Whether it turns out to be a click or a drag is
    /// decided later, by how far the cursor travels.
    fn begin_press(&mut self, tile: usize, x: f32, y: f32) {
        let rect = self.layout.tile_rect(tile, self.scroll);
        let run = self.origin_run(tile).filter(|_| self.draggable(tile));
        let band = self.layout.band_of(tile).filter(|_| run.is_some());
        let run = run.unwrap_or((tile, 0));
        // SAFETY: our own window. Capture keeps the moves coming when the cursor
        // leaves the panel mid-drag, and makes the release ours whatever it is
        // over.
        unsafe {
            SetCapture(self.hwnd);
        }
        self.press = Some(Press {
            tile,
            band,
            run,
            grab: (x - rect.x, y - rect.y),
            start: (x, y),
            dragging: false,
            slot: tile - run.0,
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
            // Past the threshold on a tile bentopick cannot rearrange: nothing to
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
            // The slot comes back measured against the whole band. Clamp it to
            // this run so a pin cannot be dropped past the seam into tiles a
            // different source owns.
            let offset = press.run.0 - self.layout.bands()[band].first;
            let slot = self.layout.insert_slot(band, x, y, self.scroll);
            press.slot = slot.clamp(offset, offset + press.run.1) - offset;
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
        if press.band.is_none() {
            return;
        }
        let (first, count) = press.run;
        let from = press.tile - first;
        for (position, slot) in reordered(count, from, press.slot).iter().enumerate() {
            if *slot == from {
                continue;
            }
            let rect = self.layout.tile_rect(first + position, self.scroll);
            if let Some(tile) = self.tiles.get(first + slot) {
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

        let (first, count) = press.run;
        let from = press.tile - first;
        let slots = reordered(count, from, press.slot);
        if slots.iter().enumerate().all(|(position, slot)| position == *slot) {
            self.cancel_press(press);
            return;
        }

        // Which of the section's sources was dragged decides both how an entry
        // is named and which list it is written back to.
        let Some(origin) = self.items.get(press.tile).map(|i| i.origin) else {
            self.cancel_press(press);
            return;
        };

        // What identifies an entry in config: manual sections list parsing
        // names, taskbar sections list pin names.
        let key = |item: &Item| match origin {
            Source::Manual => item.shell_target().map(str::to_owned),
            _ => Some(item.title.clone()),
        };
        let Some(keys) = slots
            .iter()
            .map(|slot| self.items.get(first + slot).and_then(key))
            .collect::<Option<Vec<String>>>()
        else {
            log_warn!("could not identify every tile in \"{}\"; order not saved", section.title);
            self.cancel_press(press);
            return;
        };

        let title = section.title.clone();
        let saved = match origin {
            Source::Manual => pins::reorder(&title, &keys),
            Source::Taskbar => pins::set_order(&title, &keys),
            // Ordered by the foreground hook and the browser, not by bentopick.
            Source::Windows | Source::Tabs | Source::Bookmarks => false,
            // A fixed set in a fixed order. Nowhere to write one down.
            Source::Moves => false,
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
    /// is the thing a user most often wants to pin, and bentopick is already showing
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
            Some(menu::CMD_EDIT_LAYOUT) => self.enter_edit(),
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
                // already on screen and bentopick already knows its path.
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
                // Fixed tiles. Nothing to pin, unpin or locate.
                Target::Arrange(_) | Target::Stay | Target::NewTab { .. } => {}
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
        // Kept as a second path. The button in the corner is the first one.
        if self.query.is_empty() && !self.sections.is_empty() {
            entries.push(Some(menu::Entry::new(menu::CMD_EDIT_LAYOUT, "Edit layout")));
        }
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

    /// Pairing, start to finish. A modal is the whole mechanism, not just the
    /// message: the pairing window is open for exactly as long as this box is
    /// on screen, which is a rule the user can see instead of a countdown they
    /// have to beat. `MessageBoxW` runs its own message loop, so the panel and
    /// the socket threads keep going underneath it.
    fn pair_browser(&mut self) {
        if !matches!(server::status().0, server::Status::Listening) {
            // Pairing needs a socket, and asking the user to hand-edit the
            // config to reach the pairing flow is how the old one went wrong.
            if !self.config.browser.enabled {
                log_info!("turning the browser bridge on to pair");
                pins::set_browser_enabled(true);
                self.config.browser.enabled = true;
            }
            let bridge = self.config.browser.clone();
            server::start(self.hwnd, &bridge);
        }

        let (status, port) = server::status();
        if status != server::Status::Listening {
            // Refusing to pair here is the point. Something else on the port
            // is the one situation where the extension could be talking to
            // something that is not bentopick, and handing out a code in that
            // state would be handing it to whatever answered.
            self.say(
                "Cannot pair right now",
                &format!(
                    "Another process is using port {port}, so BentoPick's browser \
                     bridge is not listening.\n\n\
                     Close whatever is using it, or set a different browser.port in \
                     bentopick.toml, then try again."
                ),
            );
            return;
        }

        let Some(code) = gate::open_pairing() else {
            self.say("Cannot pair right now", "Could not generate a pairing code.");
            return;
        };

        self.say(
            "Pair a browser",
            &format!(
                "Pairing code:  {}  {}\n\n\
                 Open the BentoPick extension's options page, choose \"Pair with \
                 BentoPick\", and type this code.\n\n\
                 Pairing stays open until you close this window.",
                &code[..3],
                &code[3..]
            ),
        );

        gate::close_pairing();
        tray::refresh(self.hwnd);
    }

    /// Index into the same list the tray menu was built from. A peer forgotten
    /// between the menu opening and the click just is not there any more.
    fn forget_browser(&mut self, index: usize) {
        let Some(peer) = crate::browser::peers::all().into_iter().nth(index) else {
            return;
        };
        if crate::browser::peers::forget(&peer.origin) {
            // Its tabs are still on screen until it notices; the next refusal
            // closes the connection and takes them with it.
            self.say(
                "Browser forgotten",
                &format!(
                    "{} is no longer paired. Its tabs disappear as soon as it \
                     reconnects and is turned away.",
                    peer.name
                ),
            );
            tray::refresh(self.hwnd);
        }
    }

    /// The only dialog bentopick has. Everything else it draws itself, but a
    /// pairing code has to be readable while the user is typing into another
    /// window, and a stock modal is exactly that.
    fn say(&self, caption: &str, text: &str) {
        let (text, caption) = (HSTRING::from(text), HSTRING::from(caption));
        // SAFETY: both strings outlive the call, and MessageBoxW pumps its own
        // message loop so nothing on this thread is starved while it is up.
        unsafe {
            MessageBoxW(
                Some(self.hwnd),
                PCWSTR(text.as_ptr()),
                PCWSTR(caption.as_ptr()),
                MB_OK | MB_ICONINFORMATION | MB_SETFOREGROUND,
            );
        }
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
            // A second copy was launched. Same intent as the hotkey.
            instance::WM_SUMMON => {
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
                        Some(tray::CMD_PAIR_BROWSER) => self.pair_browser(),
                        Some(chosen) if chosen >= tray::CMD_FORGET_BASE => {
                            self.forget_browser(chosen - tray::CMD_FORGET_BASE)
                        }
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
            crate::browser::server::WM_PAIRED => {
                tray::refresh(self.hwnd);
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
                let (hx, hy) = point_of(lparam);
                let over_home =
                    self.home.as_ref().is_some_and(|(rect, _)| rect.contains(hx, hy));
                if over_home != self.hover_home {
                    self.hover_home = over_home;
                    self.refresh_menu();
                }
                if self.settings_open {
                    let next = self
                        .settings_items
                        .iter()
                        .position(|(_, rect, _)| rect.contains(hx, hy));
                    if next != self.hover_setting {
                        self.hover_setting = next;
                        self.refresh_settings();
                    }
                    return Some(LRESULT(0));
                }
                if self.menu_open_big {
                    let next = self
                        .menu_items
                        .iter()
                        .position(|(_, rect, _)| rect.contains(hx, hy));
                    if next != self.hover_menu {
                        self.hover_menu = next;
                        self.refresh_menu();
                    }
                    return Some(LRESULT(0));
                }
                // Editing points at boxes and at the options over them.
                if self.editing {
                    if !self.set_hover_option(hx, hy) {
                        self.set_hover_box(hx, hy);
                    }
                    return Some(LRESULT(0));
                }
                if over_home {
                    self.set_hover(None);
                    return Some(LRESULT(0));
                }
                self.set_hover(self.cursor_index(lparam));
                Some(LRESULT(0))
            }
            WM_MOUSELEAVE if self.editing => {
                self.tracking_mouse = false;
                if self.hover_box.take().is_some() {
                    self.refresh_boxes();
                }
                if self.hover_option.take().is_some() {
                    self.refresh_options();
                }
                Some(LRESULT(0))
            }
            WM_MOUSELEAVE => {
                self.tracking_mouse = false;
                self.set_hover(None);
                Some(LRESULT(0))
            }
            // A press is not yet a click. Which one it becomes is decided on
            // release, by whether the cursor travelled far enough to be a drag.
            WM_LBUTTONDOWN => {
                let (x, y) = point_of(lparam);
                // The app's own button is over everything and answers first.
                if self.home.as_ref().is_some_and(|(rect, _)| rect.contains(x, y)) {
                    self.handled_down = true;
                    self.press_home();
                    return Some(LRESULT(0));
                }
                if self.settings_open {
                    self.handled_down = true;
                    if let Some(setting) = self
                        .settings_items
                        .iter()
                        .find(|(_, rect, _)| rect.contains(x, y))
                        .map(|(setting, _, _)| *setting)
                    {
                        self.run_setting(setting);
                    } else {
                        // Anywhere else closes it, the way the menu does.
                        self.settings_open = false;
                        let _ = self.rebuild_visuals();
                    }
                    return Some(LRESULT(0));
                }
                if self.menu_open_big {
                    self.handled_down = true;
                    if let Some(command) = self
                        .menu_items
                        .iter()
                        .find(|(_, rect, _)| rect.contains(x, y))
                        .map(|(command, _, _)| *command)
                    {
                        self.run_command(command);
                    } else {
                        // Anywhere else closes it, the way a menu should.
                        self.menu_open_big = false;
                        let _ = self.rebuild_visuals();
                    }
                    return Some(LRESULT(0));
                }
                // While editing, a click picks a box or an option. Launching
                // from a layout editor would be a click nobody meant.
                if self.editing {
                    self.handled_down = true;
                    self.edit_click(x, y);
                    return Some(LRESULT(0));
                }
                if let Some(index) = self.layout.hit_test(x, y, self.scroll) {
                    self.begin_press(index, x, y);
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                let handled = std::mem::take(&mut self.handled_down);
                if let Some(press) = self.press.take() {
                    match (press.dragging, press.band) {
                        // Travelled, and over something bentopick can rearrange.
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
                // Acted on the button-down already. None of these may fall
                // through to the dismiss-on-padding rule below. The state
                // checks are for a release with no press of ours behind it -
                // a capture lost mid-click, a panel summoned under a held
                // button - where there is no flag to read.
                if handled || self.editing || self.menu_open_big || self.settings_open {
                    return Some(LRESULT(0));
                }
                let (x, y) = point_of(lparam);
                if self.home.as_ref().is_some_and(|(rect, _)| rect.contains(x, y)) {
                    return Some(LRESULT(0));
                }
                let (_, y) = point_of(lparam);
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
            WM_MOUSEWHEEL => {
                let delta = ((wparam.0 >> 16) & 0xFFFF) as i16 as f32;
                self.scroll_by(delta);
                Some(LRESULT(0))
            }
            // A hidden panel has no keyboard. Focus guarantees that, except
            // for posted messages, which bypass it.
            // Bit 30 is the previous key state. Modifiers auto-repeat while
            // held, and a toggle that fired on every repeat would flicker.
            WM_KEYDOWN if self.visible => {
                let repeat = lparam.0 & (1 << 30) != 0;
                self.on_key(wparam.0 as u16, repeat).then_some(LRESULT(0))
            }
            WM_CHAR if self.visible => {
                self.on_char(wparam.0 as u32);
                Some(LRESULT(0))
            }
            // Clicking away, or anything else stealing focus, dismisses — unless
            // a menu of ours is up.
            // Editing holds the panel open: the layout is being changed while
            // it is looked at, and a config write must not dismiss it.
            WM_ACTIVATE
                if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE
                    && !self.menu_open
                    && !self.editing
                    && !self.arranging =>
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
        if self.hotkey_bound {
            // SAFETY: matches the successful RegisterHotKey in bind_hotkey.
            unsafe {
                let _ = UnregisterHotKey(Some(self.hwnd), HOTKEY_ID);
            }
        }
    }
}

/// The app's own icon, off the same cache the tiles use. Our exe is a shell item
/// like any other, so this needs no separate resource-loading path.
fn app_icon(size: u32) -> Option<std::sync::Arc<icons::IconPixels>> {
    let exe = std::env::current_exe().ok()?;
    icons::request(exe.to_str()?, size.max(16))
}

/// Open `bentopick.toml` in whatever the user edits TOML with. Falls back to
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
/// The rectangle around a set of option squares, plus a margin. The plate they
/// sit on has to be derived from them, not guessed, or the two drift apart.
fn surround(placed: impl Iterator<Item = GridRect> + Clone, margin: f32) -> GridRect {
    let left = placed.clone().map(|r| r.x).fold(f32::MAX, f32::min) - margin;
    let top = placed.clone().map(|r| r.y).fold(f32::MAX, f32::min) - margin;
    let right = placed.clone().map(|r| r.x + r.w).fold(f32::MIN, f32::max) + margin;
    let bottom = placed.map(|r| r.y + r.h).fold(f32::MIN, f32::max) + margin;
    GridRect { x: left, y: top, w: right - left, h: bottom - top }
}

/// The same colour at a chosen opacity. Edit mode is built out of sheets laid
/// over the grid, and every one of them needs what is underneath to show
/// through by a controlled amount.
fn veil(color: Color, alpha: f32) -> Color {
    Color { A: (alpha.clamp(0.0, 1.0) * 255.0) as u8, ..color }
}

fn dim(mut c: D2D1_COLOR_F) -> D2D1_COLOR_F {
    c.a *= 0.6;
    c
}

fn color_of(spec: &str) -> Color {
    let (a, r, g, b) = config::parse_color(spec);
    Color { A: a, R: r, G: g, B: b }
}

/// The same colour as a plain `0xRRGGBB`. The desktop ring is a GDI window with
/// no alpha, so the theme's alpha byte is dropped rather than blended.
fn rgb_of(spec: &str) -> u32 {
    let (_, r, g, b) = config::parse_color(spec);
    (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
}

/// A picture of where the window lands, drawn rather than set in a glyph.
fn mark_of(mv: arrange::Move) -> Mark {
    match mv {
        arrange::Move::Left => Mark::Half { left: 0.0, top: 0.0, right: 0.5, bottom: 1.0 },
        arrange::Move::Right => Mark::Half { left: 0.5, top: 0.0, right: 1.0, bottom: 1.0 },
        // Maximize fills the screen; down off it is the smaller window it
        // restores to, and off the bottom of that, minimized.
        arrange::Move::Up => Mark::Half { left: 0.0, top: 0.0, right: 1.0, bottom: 1.0 },
        arrange::Move::Down => Mark::Half { left: 0.24, top: 0.26, right: 0.76, bottom: 0.74 },
        arrange::Move::ScreenLeft => Mark::Screen { second: false },
        arrange::Move::ScreenRight => Mark::Screen { second: true },
    }
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

pub const CLASS_NAME: PCWSTR = w!("bentopick_panel");

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
        // WS_EX_TOOLWINDOW: keeps bentopick out of alt-tab and the taskbar.
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOREDIRECTIONBITMAP,
            CLASS_NAME,
            w!("BentoPick"),
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
