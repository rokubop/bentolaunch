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
    ContainerVisual, ShapeVisual, SpriteVisual, Visual,
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
use crate::model::{Handle, Item, Kind, Mode, Section, Target};
use crate::safety;
use crate::shell::{activate, arrange, icons, picker};
use crate::ui::filter;
use crate::ui::grid::{
    Band, BoxState, CenterState, Command, Control, Lane, Layout, Metrics, Rect as GridRect,
    SectionShape,
    centred_grid, commands, controls, origin_run, reordered,
};
use crate::ui::menu;
use crate::ui::settings::{self, SETTINGS, Setting};
use crate::ui::render::{Badge, Mark, OptionPaint, Renderer, TextColors, TilePaint, d2d_color};
use crate::ui::spotlight::Spotlight;
use crate::ui::tray;
use crate::{pins, watch};
use crate::{log_dry, log_error, log_info, log_warn};

const HOTKEY_ID: i32 = 1;
/// Drives the watchdog heartbeat while the panel is up.
const HEARTBEAT_TIMER: usize = 1;
const HEARTBEAT_MS: u32 = 250;
/// Asks, a moment after a mode lost focus, whether it really lost it.
const DEACTIVATED_TIMER: usize = 2;
/// Long enough for a write or a `WM_CLOSE` to hand focus back, short enough
/// that a click on another window has visibly dismissed the panel.
const DEACTIVATED_MS: u32 = 120;
/// Thick enough to read past a tile's own fill without eating into the icon.
const TARGET_STROKE: f32 = 3.0;

/// How far the face of a tile the current mode cannot act on is faded back. Far
/// enough to read as a field of what is *not* in play at a glance, not so far
/// that the grid loses its shape - tiles are found by aiming where they were
/// last time, so an unavailable one stays exactly where it was.
const INERT_OPACITY: f32 = 0.4;
/// A press that never travels this far is a click, not a drag.
///
/// Taken from the shell rather than picked, so bentolaunch's idea of "that was a
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
    /// The ring saying the next favorite lands here. Built on the block's empty
    /// squares while center mode is on, and shown or hidden as the pointer
    /// moves - built once rather than rebuilt, because the grid under the
    /// pointer must not be torn down while it is being pointed at.
    landing: Option<ShapeVisual>,
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

    /// `None` if D3D/D2D could not start. bentolaunch still runs; tiles just lose
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

    /// What a click on a tile means right now.
    ///
    /// Modes, unlike everything else here, because each of these changes what
    /// clicking does rather than what one tile is. All of them hold the panel
    /// open and all of them are left by the same button in the corner, so there
    /// is never one with no visible way out.
    mode: Mode,
    /// Which box has been picked, as an index into `sections`. `None` means the
    /// mode is on but nothing is chosen yet, which is where it starts: the
    /// options belong to a box, so there is nothing to show until one is.
    edit: Option<usize>,
    /// The half of the centre block center mode was aimed at, which is the
    /// empty square that was clicked to get into it. `None` when the mode was
    /// entered from its own square instead, and both halves are then waiting.
    ///
    /// It steers nothing about where a pick is written - a page belongs in the
    /// sites list whichever square was clicked - it only says which empty
    /// square is ringed while nothing is under the pointer.
    filling: Option<pins::Half>,
    /// The box under the pointer while editing. Separate from `hover`, which
    /// is a tile: in this mode the thing being pointed at is a whole box.
    hover_box: Option<usize>,
    /// One fill per band, in band order, so hovering repaints a box without
    /// rebuilding the grid under the pointer.
    /// One per band while editing. Not a colour brush - the face follows the
    /// box's cells, which can be an L or a C, and composition has rectangle
    /// geometry and nothing else.
    box_faces: Vec<BoxFace>,
    /// Content-space chrome that is not a tile: the ring round each box, the
    /// plate behind it, the frame round the centre block.
    ///
    /// Kept because a scroll moves them. `reposition` walked the tiles and the
    /// headers and nothing else, so the borders stayed where they were drawn
    /// while the grid slid past underneath them.
    scrolled: Vec<Scrolled>,
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
    /// The reset question is up, in place of the settings squares. Cleared
    /// whenever the surface opens, so a question never waits across a close.
    asking_reset: bool,
}

/// One piece of chrome that scrolls with the grid.
struct Scrolled {
    visual: Visual,
    /// Which box it belongs to, so the foot's own ring stays put with it.
    band: usize,
    /// Where it sits with the grid at the top, which is what a scroll is
    /// measured from.
    rest: f32,
}

/// What the menu calls adding a tile: the box it will land in, named.
///
/// Falls back to the bare verb when the config cannot be read, which is the one
/// case where nothing can be promised about where it goes.
fn add_label() -> String {
    match pins::destination() {
        Some(title) if !title.is_empty() => format!("Add to {title}"),
        _ => "Add this app".to_string(),
    }
}

/// A pressed tile, which may still turn out to be either a click or a drag.
struct Press {
    /// Flat index of the tile under the press.
    tile: usize,
    /// Its section, if this tile's order is bentolaunch's to rearrange.
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
            split: 0.5,
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
            mode: Mode::Grid,
            edit: None,
            filling: None,
            hover_box: None,
            box_faces: Vec::new(),
            scrolled: Vec::new(),
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
            asking_reset: false,
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
                "hotkey '{}' could not be parsed; bentolaunch has no way to be summoned",
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
            split: g.split,
        }
    }

    /// Section layout comes from config, matched to the live sections by title:
    /// an empty section never reaches `self.sections`, so the two lists are the
    /// same order but not the same length.
    fn shapes(&self) -> Vec<SectionShape> {
        self.sections
            .iter()
            .map(|s| {
                // The centre is not in `[[sections]]` and must never be matched
                // against it by title: a user's own section called the same
                // thing would hand the centre a placement it cannot use.
                if s.center.is_some() {
                    return SectionShape {
                        title: String::new(),
                        count: s.items.len(),
                        lane: Lane::default(),
                        columns: s.columns,
                        center: s.center,
                        pinned: false,
                    };
                }
                let placed = self.config.sections.iter().find(|c| c.title == s.title);
                SectionShape {
                    title: s.title.clone(),
                    count: s.items.len(),
                    lane: lane_of(placed),
                    columns: placed.map_or(0, |c| c.columns),
                    center: None,
                    // The bar at the foot of the panel, by what it holds rather
                    // than by what it is called: these are the squares aimed at
                    // by position, and position is what scrolling takes away.
                    pinned: !s.items.is_empty()
                        && s.items
                            .iter()
                            .all(|i| matches!(i.origin, Source::Modes | Source::Moves)),
                }
            })
            .collect()
    }

    /// Pull the model, apply the query, recompute geometry.
    fn reload(&mut self) {
        let all = store::sections(self.mode);
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
        self.fit_window();
    }

    /// Size the window to the layout it now has.
    ///
    /// Every path that recomputes the layout while the panel is up needs this.
    /// A mode adds a box on the way in and takes it away on the way out, and
    /// the panel is positioned from its own height, so both the size and the
    /// place change. Without it the grid is drawn for a panel taller than the
    /// window and the bottom row is clipped off the edge - which is the modes
    /// bar, and the corner button with it.
    ///
    /// Only while visible: `show` calls this before it has a window on screen,
    /// and does its own `SWP_SHOWWINDOW` afterwards.
    fn fit_window(&self) {
        if !self.visible {
            return;
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
        if self.in_mode() {
            log_info!("hide in {:?} mode (restore_caller={restore_caller})", self.mode);
        }
        self.visible = false;
        self.tracking_mouse = false;
        self.press = None;
        self.query.clear();
        self.frozen_cols = 0;
        self.selected = None;
        self.mode = Mode::Grid;
        self.edit = None;
        self.filling = None;
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
            // A pending "did it really lose focus?" has been answered by the
            // panel going away.
            let _ = KillTimer(Some(self.hwnd), DEACTIVATED_TIMER);
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }

        // Restoring the caller is bentolaunch undoing its own activation, not acting
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
        self.scrolled.clear();
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

        let icon_size = self.icon_size();
        let label_height = self.config.grid.label_height * scale;
        let show_detail = self.show_detail();
        let colors = self.text_colors();
        // What an unavailable move tile is drawn in: the same grey the section
        // titles use, so it reads as label rather than as control.
        let muted = d2d_color(&self.config.theme.header);
        let accent = d2d_color(&self.config.theme.tile_target);
        let mut built = Vec::with_capacity(self.items.len());

        // Between the grid and the bar that does not scroll with it. Inserted
        // here rather than after the loop, because everything goes in at the
        // top: this is what puts it over the tiles already placed and under the
        // ones still to come.
        let foot_from = self.layout.foot_from();
        let mut foot_chrome: Vec<Visual> = Vec::new();
        for (index, item) in self.items.iter().enumerate() {
            if index == foot_from
                && let Some(strip) = self.layout.foot_rect()
            {
                let (backing, _) = self.rounded_rect(
                    Vector2 { X: strip.w, Y: strip.h },
                    0.0,
                    veil(color_of(&self.config.theme.panel), 1.0),
                )?;
                backing.SetOffset(Vector3 { X: strip.x, Y: strip.y, Z: 0.0 })?;
                children.InsertAtTop(&backing)?;
                // A line where the grid disappears under it. The bar is chrome
                // now, and chrome that just floats over the content reads as a
                // row that failed to scroll.
                let (edge, _) = self.rounded_rect(
                    Vector2 { X: strip.w, Y: 1.0 },
                    0.0,
                    color_of(&self.config.theme.box_edge),
                )?;
                edge.SetOffset(Vector3 { X: strip.x, Y: strip.y, Z: 0.0 })?;
                children.InsertAtTop(&edge)?;
                foot_chrome.push(backing.cast()?);
                foot_chrome.push(edge.cast()?);
            }
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
                // An empty square in center mode is the thing being aimed
                // at, so its plus is drawn in the colour that means "this one",
                // the same one the ring round the landing square is in.
                false if self.mode == Mode::Center && matches!(item.target, Target::Slot) => {
                    TextColors { title: accent, detail: accent }
                }
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
                            running: item
                                .running
                                .map(|_| d2d_color(&self.config.theme.tile_target)),
                            badge: self.badge(item),
                            colors,
                        };
                        if let Err(e) = renderer.draw_tile(&drawn, paint) {
                            log_warn!("could not draw tile \"{}\": {e}", item.title);
                        }

                        let sprite = self.compositor.CreateSpriteVisual()?;
                        sprite.SetSize(Vector2 { X: rect.w, Y: rect.h })?;
                        sprite.SetBrush(&self.compositor.CreateSurfaceBrushWithSurface(&drawn)?)?;
                        // A mode should read as a field rather than tile by
                        // tile: what it can act on stays where it was and
                        // everything else recedes, so there is nothing to hunt
                        // for. The icon and the words fade, never the fill -
                        // the panel is translucent, and a tile faded whole is a
                        // hole with the desktop showing through it.
                        if self.inert(item) {
                            sprite.SetOpacity(INERT_OPACITY)?;
                        }
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

            // One on every empty square, hidden until the state calls for it.
            // Which square is next changes with the pointer, and a rebuild to
            // move a ring would tear the grid down under the hand moving it.
            let landing = match self.mode == Mode::Center
                && matches!(item.target, Target::Slot)
            {
                true => {
                    let ring = self.rounded_ring(
                        Vector2 { X: rect.w, Y: rect.h },
                        radius,
                        color_of(&self.config.theme.tile_target),
                    )?;
                    ring.SetIsVisible(false)?;
                    root.Children()?.InsertAtTop(&ring)?;
                    Some(ring)
                }
                false => None,
            };

            children.InsertAtTop(&root)?;
            built.push(Tile { root, brush, surface, awaiting_icon, landing });
        }

        self.tiles = built;
        self.refresh_landing();
        // Over the tiles: a seam runs between the tiles it separates, and the
        // centre's frame has to read as being in front of the layout.
        self.build_edges(radius)?;
        // And the titles over the rings, because a title is a break in its
        // ring and a ring drawn afterwards would run straight through it.
        self.build_titles()?;

        // The bar at the foot goes on last of all. Rings and titles are drawn
        // over every tile, and the grid runs *under* the bar rather than
        // stopping at it, so both would otherwise be drawn straight across it.
        let children = self.content.Children()?;
        for visual in &foot_chrome {
            children.Remove(visual)?;
            children.InsertAtTop(visual)?;
        }
        for tile in &self.tiles[foot_from.min(self.tiles.len())..] {
            children.Remove(&tile.root)?;
            children.InsertAtTop(&tile.root)?;
        }
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

    /// The section titles, each one a mark on the ring round its own box.
    ///
    /// Not headers any more: they take no row, so they sit on the line rather
    /// than above it, and they are drawn in the ring's own colour. What used to
    /// identify a box was a word costing a whole row of the panel. It is the
    /// colour of the line now, and the word is what confirms it.
    ///
    /// After `build_edges`, so a title's plate breaks the line it sits on.
    fn build_titles(&mut self) -> Result<()> {
        let Some(renderer) = &self.renderer else {
            return Ok(());
        };
        let children = self.content.Children()?;
        let plate_color = d2d_color(&self.config.theme.panel);
        // Edit mode says what each box is set to, and marks the one the keys
        // will land on. The grid underneath is left alone: the point is to
        // watch the real layout change as it is edited.
        let editing = self.editing();
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
            if rect.w < 1.0 || rect.h < 1.0 {
                continue;
            }
            let section_of = self.layout.bands().get(band).map(|band| band.section);
            // The ring's colour at full strength: the line says which box this
            // is, and the title has to read as the same statement.
            let color = if editing && self.edit == section_of {
                selected_color
            } else {
                opaque(self.section_edge(section_of.unwrap_or(0)))
            };
            let surface = match renderer.create_surface(rect.w, rect.h) {
                Ok(surface) => surface,
                Err(e) => {
                    log_warn!("could not create a title surface: {e}");
                    continue;
                }
            };
            if let Err(e) =
                renderer.draw_legend(&surface, rect.w, rect.h, title, color, plate_color)
            {
                log_warn!("could not draw title \"{title}\": {e}");
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
        Ok(())
    }

    /// The lines that say where one box ends and the next begins, and the frame
    /// around the centre block.
    ///
    /// Over the tiles, under everything a mode puts up. The seams have to be
    /// visible across the tiles they run between - drawn underneath, a box's
    /// own edge would be hidden by the very tiles it is separating - and the
    /// centre's frame has to read as being in front of the layout, because that
    /// is exactly what the block is.
    fn build_edges(&mut self, radius: f32) -> Result<()> {
        let children = self.content.Children()?;
        let mut keep: Vec<Scrolled> = Vec::new();
        let scale = self.scale();
        let hairline = (1.0 * scale).max(1.0);

        // Boxes only. The centre gets its own frame below, at its own weight,
        // and two lines round it would be one too many.
        //
        // Not a rectangle round each: a box wraps round the centre block, so
        // the shape can be an L, a C, or a rectangle with a hole in the middle
        // of it, and a rectangle would cut straight through the block. What is
        // drawn is what the box actually occupies - see `grid::Cells`.
        if let Some(renderer) = &self.renderer {
            let stroke = (1.5 * self.scale()).max(1.0);
            // Room for the stroke, which is centred on the path and so hangs
            // half its weight outside the corners.
            let margin = stroke;
            for index in 0..self.layout.bands().len() {
                let band = &self.layout.bands()[index];
                if band.center || band.count == 0 {
                    continue;
                }
                let color = self.section_edge(band.section);
                if color.a <= 0.0 {
                    continue;
                }
                let rings = self.layout.band_ring(index, self.scroll);
                let Some(bounds) = covering(&rings, margin) else {
                    continue;
                };
                let surface = match renderer.create_surface(bounds.w, bounds.h) {
                    Ok(surface) => surface,
                    Err(e) => {
                        log_warn!("could not create a ring surface: {e}");
                        continue;
                    }
                };
                // Surface local, so the sprite can be offset to the bounds and
                // the geometry never carries the panel's coordinates.
                let local: Vec<Vec<(f32, f32)>> = rings
                    .into_iter()
                    .map(|ring| {
                        ring.into_iter()
                            .map(|(x, y)| (x - bounds.x, y - bounds.y))
                            .collect()
                    })
                    .collect();
                if let Err(e) = renderer.draw_ring(&surface, &local, radius, stroke, color) {
                    log_warn!("could not draw a box ring: {e}");
                    continue;
                }
                let sprite = self.compositor.CreateSpriteVisual()?;
                sprite.SetSize(Vector2 { X: bounds.w, Y: bounds.h })?;
                sprite.SetOffset(Vector3 { X: bounds.x, Y: bounds.y, Z: 0.0 })?;
                sprite.SetBrush(&self.compositor.CreateSurfaceBrushWithSurface(&surface)?)?;
                children.InsertAtTop(&sprite)?;
                let rest = bounds.y + self.layout.band_scroll(index, self.scroll);
                keep.push(Scrolled { visual: sprite.cast()?, band: index, rest });
            }
        }
        self.scrolled.append(&mut keep);

        let Some((block, seams)) = self.layout.center_frame() else {
            return Ok(());
        };
        let edge = color_of(&self.config.theme.center_edge);
        if edge.A == 0 {
            return Ok(());
        }
        let block = block.shifted_by(self.scroll);
        // Out into the gutter the tiles already leave, so the frame surrounds
        // the block rather than cropping it.
        let out = (self.config.grid.gap * scale) / 2.0;
        let frame = GridRect {
            x: block.x - out,
            y: block.y - out,
            w: block.w + 2.0 * out,
            h: block.h + 2.0 * out,
        };
        let ring = self.outline(
            Vector2 { X: frame.w, Y: frame.h },
            radius * 1.5,
            edge,
            (2.0 * scale).max(1.0),
        )?;
        ring.SetOffset(Vector3 { X: frame.x, Y: frame.y, Z: 0.0 })?;
        children.InsertAtTop(&ring)?;
        // The centre is not in the tree, so it answers to no band. It scrolls
        // with the grid like everything else in content space.
        self.scrolled.push(Scrolled {
            visual: ring.cast()?,
            band: 0,
            rest: frame.y + self.scroll,
        });

        // One container with a line down it, not two containers side by side.
        // Which half is which is the block's only rule, and a seam is what says
        // it while both halves are still empty.
        for x in seams {
            let (line, _) =
                self.rounded_rect(Vector2 { X: hairline, Y: block.h }, 0.0, edge)?;
            line.SetOffset(Vector3 {
                X: (x - hairline / 2.0).round(),
                Y: block.y,
                Z: 0.0,
            })?;
            children.InsertAtTop(&line)?;
        }
        Ok(())
    }

    /// A tint behind a box, for the sections that asked for one in config.
    ///
    /// Under everything, header included: the point is to say where the box
    /// begins and ends, and a plate that stopped at the header would say it
    /// twice. `InsertAtBottom` rather than build order, so this stays
    /// independent of when the tiles go in.
    fn build_box_plates(&mut self, radius: f32) -> Result<()> {
        let plates: Vec<(usize, GridRect, Color)> = self
            .layout
            .bands()
            .iter()
            .enumerate()
            .filter_map(|(index, band)| {
                let color = self.sections.get(band.section)?.color.as_deref()?;
                let rect = band.rect.shifted_by(self.layout.band_scroll(index, self.scroll));
                Some((index, rect, color_of(color)))
            })
            .collect();
        if plates.is_empty() {
            return Ok(());
        }

        let children = self.content.Children()?;
        for (band, rect, color) in plates {
            let (face, _) =
                self.rounded_rect(Vector2 { X: rect.w, Y: rect.h }, radius * 1.5, color)?;
            face.SetOffset(Vector3 { X: rect.x, Y: rect.y, Z: 0.0 })?;
            children.InsertAtTop(&face)?;
            let rest = rect.y + self.layout.band_scroll(band, self.scroll);
            self.scrolled.push(Scrolled { visual: face.cast()?, band, rest });
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
        if !self.editing() {
            return Ok(());
        }

        let Some(renderer) = &self.renderer else { return Ok(()) };
        let children = self.content.Children()?;
        // The box's own cells, not `band.rect`. That rectangle is stretched to
        // tile the panel - a lane with nothing opposite it takes the whole
        // width - so a face drawn on it lit up half the panel for a box holding
        // one side of it.
        for index in 0..self.layout.bands().len() {
            let rings = self.layout.band_ring(index, self.scroll);
            let Some(bounds) = covering(&rings, 0.0) else { continue };
            let surface = match renderer.create_surface(bounds.w, bounds.h) {
                Ok(surface) => surface,
                Err(e) => {
                    log_warn!("could not create a box face: {e}");
                    continue;
                }
            };
            let local: Vec<Vec<(f32, f32)>> = rings
                .into_iter()
                .map(|ring| {
                    ring.into_iter().map(|(x, y)| (x - bounds.x, y - bounds.y)).collect()
                })
                .collect();
            if let Err(e) =
                renderer.draw_shape(&surface, &local, radius * 1.5, d2d_color_of(self.box_color(index)))
            {
                log_warn!("could not draw a box face: {e}");
                continue;
            }
            let sprite = self.compositor.CreateSpriteVisual()?;
            sprite.SetSize(Vector2 { X: bounds.w, Y: bounds.h })?;
            sprite.SetOffset(Vector3 { X: bounds.x, Y: bounds.y, Z: 0.0 })?;
            sprite.SetBrush(&self.compositor.CreateSurfaceBrushWithSurface(&surface)?)?;
            children.InsertAtTop(&sprite)?;
            self.box_faces.push((surface, local));
        }
        Ok(())
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
                    // The block is beside the square, so this one says what
                    // it holds now rather than naming the question again.
                    Some(_) if control == Control::CenterHolds => {
                        (control.glyph(), settings::center_holds_said(&self.config))
                    }
                    // Says what the click does, not what the block is. A square
                    // reading "off" while the block is already off is a square
                    // nobody can read.
                    Some(_) if control == Control::CenterOn => match self.config.center.on() {
                        true => (control.glyph(), "Center off"),
                        false => (control.glyph(), "Center on"),
                    },
                    Some(state) => control.wording(state),
                    None => (control.glyph(), control.label()),
                };
                let paint = OptionPaint {
                    width: rect.w,
                    height: rect.h,
                    glyph,
                    mark: match control {
                        // Drawn, like the move bar's own latch, and for the
                        // same reason the lanes are drawn: a circle from the
                        // icon set says "a circle", not "on".
                        Control::CenterOn => {
                            Some(Mark::Latch { on: self.config.center.on() })
                        }
                        _ => control.span().map(|(left, right)| Mark::Half {
                            left,
                            top: 0.0,
                            right,
                            bottom: 1.0,
                        }),
                    },
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

        let rect = self.layout.home_rect();
        if rect.w < 24.0 || rect.h < 24.0 {
            return Ok(());
        }

        let (label, glyph) = match self.mode.done() {
            Some(done) => (done, "\u{2713}"),
            None => ("BentoLaunch", "\u{25A6}"),
        };

        // Our own logo on our own button, off the same cache the tiles use.
        // `None` on a cold summon - it is queued like any other icon - so the
        // glyph stands in and `fill_home_icon` paints the logo over it.
        // A mode borrows the button to say "Done", which is not us.
        let icon = (!self.in_mode()).then(|| app_icon(self.icon_size())).flatten();

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
                        mark: None,
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
        self.home_awaiting_icon = icon.is_none() && !self.in_mode() && painted.is_some();
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
                            mark: None,
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
        // The question replaces the surface rather than sitting on it. Two
        // squares where nine were is a change nobody can miss, which is the
        // whole job a confirm has.
        let squares: &[Setting] =
            if self.asking_reset { &settings::CONFIRM_RESET } else { &SETTINGS };
        let placed = centred_grid(
            GridRect { x: 0.0, y: 0.0, w: panel.w, h: panel.h },
            squares.len(),
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

        for (setting, rect) in squares.iter().copied().zip(placed) {
            let root = self.compositor.CreateContainerVisual()?;
            root.SetSize(Vector2 { X: rect.w, Y: rect.h })?;
            root.SetOffset(Vector3 { X: rect.x, Y: rect.y, Z: 0.0 })?;

            // Greyed from the start, not only once the pointer has been near
            // it: a square that looks live until you hover it is one you have
            // already aimed at for nothing.
            let fill = match setting.applies(&self.config) {
                true => idle,
                false => veil(idle, 0.35),
            };
            let (face, brush) =
                self.rounded_rect(Vector2 { X: rect.w, Y: rect.h }, radius, fill)?;
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
                            mark: None,
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
        for (index, (setting, _, brush)) in self.settings_items.iter().enumerate() {
            let color = if !setting.applies(&self.config) {
                veil(idle, 0.35)
            } else if self.hover_setting == Some(index) {
                hot
            } else {
                idle
            };
            let _ = brush.SetColor(color);
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
        // A square that does not apply is not applied either. The surface greys
        // it out; this is the half that makes that true.
        if !setting.applies(&self.config) {
            return;
        }

        // The reset asks first, and the question takes the surface: the eight
        // squares go and two answers arrive. A message box is the usual answer
        // and it is the wrong one here - small buttons, handed the focus, aimed
        // at with a gaze pointer - but so was asking on the square itself,
        // which changed a word on a tile and nothing else.
        match setting {
            Setting::Reset => {
                self.asking_reset = true;
                let _ = self.rebuild_visuals();
                self.keep_focus();
                return;
            }
            Setting::ResetNo => {
                self.asking_reset = false;
                let _ = self.rebuild_visuals();
                self.keep_focus();
                return;
            }
            Setting::ResetYes => {
                self.asking_reset = false;
                if pins::reset_layout() {
                    self.reload_config();
                    log_info!("layout reset to defaults");
                } else {
                    log_warn!("could not reset the layout");
                    // The one place a message box is right: nothing happened,
                    // so there is no change to read off the panel, and a click
                    // with no visible result is the whole complaint.
                    self.say(
                        "Could not reset the layout",
                        "The config file could not be written. It may be open in an editor,                          or not valid TOML. The log says which.",
                    );
                }
                let _ = self.rebuild_visuals();
                self.keep_focus();
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
        if let Some(mode) = command.mode() {
            self.enter_mode(mode);
            return;
        }
        match command {
            // Handled above; every mode square leaves through `Command::mode`.
            Command::EditLayout | Command::Center | Command::CloseApps => return,
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
            Command::Settings => {
                self.settings_open = true;
                self.asking_reset = false;
            }
            Command::Close => {}
        }
        let _ = self.rebuild_visuals();
    }

    /// The app's own button was clicked: leave whatever mode is on, or open and
    /// close the big menu.
    fn press_home(&mut self) {
        if self.in_mode() {
            self.leave_mode();
            return;
        }
        // Backs out one surface at a time, the same order Escape unwinds in.
        // The reset question is a surface of its own, so it is the first thing
        // backing out undoes - and the way out of it that is not an answer.
        if self.asking_reset {
            self.asking_reset = false;
            let _ = self.rebuild_visuals();
            self.keep_focus();
            return;
        }
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

    /// The colour of one section's ring, and of the title riding it.
    ///
    /// A box says which one it is by the colour of the line round it, which is
    /// what lets the title shrink to a mark on that line instead of taking a
    /// row above it. Off the section's own `edge` when it names one, and
    /// otherwise off a palette dealt out in section order - so a panel nobody
    /// has configured still comes out with its boxes told apart.
    ///
    /// An empty palette falls back to `box_edge` for every box, which is the
    /// old one-colour panel and the way to turn this off.
    fn section_edge(&self, section: usize) -> D2D1_COLOR_F {
        let Some(section) = self.sections.get(section) else {
            return d2d_color(&self.config.theme.box_edge);
        };
        if let Some(edge) = section.edge.as_deref() {
            return d2d_color(edge);
        }
        let palette = &self.config.theme.section_edges;
        match palette.is_empty() {
            true => d2d_color(&self.config.theme.box_edge),
            // By its place in the config, not its place on the panel: an empty
            // section never reaches the grid, and a box that changed colour
            // because another one turned up is a box you cannot learn.
            false => d2d_color(&palette[section.slot % palette.len()]),
        }
    }

    /// Whether tiles draw their second line.
    ///
    /// Off by default because on an ordinary panel the title is what identifies
    /// a tile and the second line is noise on every one of them. A list of every
    /// bookmark there is, is the case that needs it: five videos saved out of
    /// one series have five near-identical titles, and the folder each is filed
    /// under is the only thing telling them apart. It costs no layout - both
    /// lines share the label strip the title already has to itself.
    fn show_detail(&self) -> bool {
        self.config.grid.show_detail || self.mode == Mode::AllBookmarks
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
        self.outline(size, radius, color, TARGET_STROKE)
    }

    /// The same, at whatever weight the caller wants. A box seam is a hairline
    /// and the ring round the window being moved is three pixels, and the
    /// difference between them is the whole of what they say.
    fn outline(
        &self,
        size: Vector2,
        radius: f32,
        color: Color,
        stroke: f32,
    ) -> Result<ShapeVisual> {
        let geometry = self.compositor.CreateRoundedRectangleGeometry()?;
        geometry.SetSize(Vector2 {
            X: (size.X - stroke).max(0.0),
            Y: (size.Y - stroke).max(0.0),
        })?;
        geometry.SetCornerRadius(Vector2 { X: radius, Y: radius })?;

        let shape: CompositionSpriteShape =
            self.compositor.CreateSpriteShapeWithGeometry(&geometry)?;
        shape.SetStrokeBrush(&self.compositor.CreateColorBrushWithColor(color)?)?;
        shape.SetStrokeThickness(stroke)?;
        // The stroke is centred on the path, so half of it would fall outside.
        shape.SetOffset(Vector2 { X: stroke / 2.0, Y: stroke / 2.0 })?;

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
        // The borders move with what they surround. Without this they stayed
        // where the last rebuild put them and the grid scrolled out of them.
        for piece in &self.scrolled {
            let Ok(at) = piece.visual.Offset() else { continue };
            let y = piece.rest - self.layout.band_scroll(piece.band, self.scroll);
            let _ = piece.visual.SetOffset(Vector3 { X: at.X, Y: y, Z: at.Z });
        }
    }

    /// Hover beats selection: a tile that did not light up under the pointer
    /// reads as dead.
    fn tile_color(&self, index: usize) -> Color {
        let theme = &self.config.theme;
        // Unavailable beats every other state, hover included: a tile that
        // lights up under the pointer is a tile promising a click, and this
        // mode has nothing to do with it. Its own fill, banding and all - the
        // dimming is the whole tile's, done with opacity, and a second colour
        // saying the same thing would only fight it.
        if self.items.get(index).is_some_and(|item| self.inert(item)) {
            return color_of(if self.alternating(index) { &theme.tile_alt } else { &theme.tile });
        }
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
        let Some(item) = self.items.get(index) else { return false };
        // The mode tile for the mode that is on. Same statement as the latch:
        // this is switched on, and clicking it switches it off.
        if let Target::Mode(mode) = item.target {
            return self.mode == mode;
        }
        // Center mode: what the centre is already holding. The warm fill
        // already means "this is switched on", and a tile that is a favorite is
        // exactly that - one more click takes it back out.
        if self.mode == Mode::Center && item.origin == Source::Center {
            return !matches!(item.target, Target::Slot);
        }
        item.target == Target::Stay && self.stay
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
        // Pointing at a page rings the sites square and pointing at an app
        // rings the apps one, so where a tile is going is answered before it
        // is clicked.
        if self.mode == Mode::Center {
            self.refresh_landing();
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
            // An empty square says what it is for by doing it: taking one is
            // how center mode is found without knowing the menu exists. The
            // square clicked is what the mode comes up aimed at, so the ring is
            // already round the square that was asked for.
            Target::Slot => {
                let half = self.slot_half(index);
                self.enter_mode(Mode::Center);
                self.filling = half;
                self.refresh_landing();
                return;
            }
            // The same square turns the mode on and off, so a mode tile is
            // never a one-way door.
            Target::Mode(mode) => {
                if self.mode == mode {
                    self.leave_mode();
                } else {
                    self.enter_mode(mode);
                }
                return;
            }
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
            // A tab or a link is not a window. bentolaunch cannot map a tab onto
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

    /// A tile the current mode cannot act on. Drawn dim rather than taken away:
    /// tiles are found by aiming at where they were last time, so the grid
    /// keeps its shape and says it is unavailable instead.
    fn inert(&self, item: &Item) -> bool {
        // The way out of the mode is never dim. Every mode is left by a square,
        // and a square drawn as unavailable is one nobody aims at.
        if matches!(item.target, Target::Mode(_)) {
            return false;
        }
        match self.mode {
            // An empty square is never dim in this mode: it is what the mode
            // is aimed at, and clicking one aims it at that half.
            Mode::Center => {
                !matches!(item.target, Target::Slot) && self.center_would(item).is_none()
            }
            // Nothing open behind it, so there is nothing to close.
            Mode::Close => self.window_for(item).is_none(),
            Mode::Grid | Mode::Layout | Mode::Move | Mode::AllApps | Mode::AllBookmarks => {
                matches!(item.target, Target::Arrange(_)) && self.moving().is_none()
            }
        }
    }

    /// The empty square in the block that each list's next favorite lands in,
    /// indexed by `pins::Half`.
    ///
    /// A pick is written to the end of its list, so it appears in the first
    /// empty square of the half drawing that list. That square is the answer to
    /// "where is this going", which is the one thing center mode never said.
    fn landing_slots(&self) -> [Option<usize>; 2] {
        let contents = self.config.center.contents;
        let mut out = [None, None];
        let mut base = 0;
        for section in &self.sections {
            if let Some(drawn) = section.center {
                let first = section
                    .items
                    .iter()
                    .position(|item| matches!(item.target, Target::Slot))
                    .map(|n| base + n);
                for (half, slot) in out.iter_mut().enumerate() {
                    if contents.holds(drawn, half) {
                        *slot = first;
                    }
                }
            }
            base += section.items.len();
        }
        out
    }

    /// Which list an empty square belongs to, for aiming the mode at it.
    ///
    /// One block draws both lists in one half, so a square there answers for
    /// the first of them. Nothing is lost by that: both lists land in the same
    /// square, so both rings would be drawn round it anyway.
    fn slot_half(&self, index: usize) -> Option<pins::Half> {
        let contents = self.config.center.contents;
        let mut base = 0;
        for section in &self.sections {
            if (base..base + section.items.len()).contains(&index)
                && let Some(drawn) = section.center
            {
                return (0..2)
                    .find(|&half| contents.holds(drawn, half))
                    .map(|half| match half {
                        0 => pins::Half::Apps,
                        _ => pins::Half::Sites,
                    });
            }
            base += section.items.len();
        }
        None
    }

    /// Which landing squares are ringed right now.
    ///
    /// Hover wins: pointing at a page rings the sites square and pointing at an
    /// app rings the apps one, so where a tile is going is answered before the
    /// click rather than after it. With nothing eligible under the pointer it
    /// falls back to the square that was clicked to get here - and to both,
    /// when the mode was entered from its own square and neither was named.
    fn landing_lit(&self) -> [bool; 2] {
        let mut lit = [false; 2];
        if let Some(item) = self.hover.and_then(|index| self.items.get(index))
            && item.origin != Source::Center
            && let Some((half, _)) = self.center_would(item)
        {
            lit[half.index()] = true;
            return lit;
        }
        match self.filling {
            Some(half) => lit[half.index()] = true,
            None => lit = [true, true],
        }
        lit
    }

    /// Show the ring on the landing squares the state calls for, and hide the
    /// rest. Cheap enough to run on every hover: it sets a flag on at most two
    /// visuals that are already built.
    fn refresh_landing(&self) {
        let landings = self.landing_slots();
        let lit = self.landing_lit();
        for (index, tile) in self.tiles.iter().enumerate() {
            let Some(ring) = &tile.landing else { continue };
            let on = (0..2).any(|half| lit[half] && landings[half] == Some(index));
            let _ = ring.SetIsVisible(on);
        }
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

    /// The corner mark saying what a click does to this tile *right now*.
    ///
    /// Only where a click destroys something. Center mode is one gesture
    /// both ways - the same click that put a tile in the block takes it back
    /// out - and a fill that says "this one is switched on" does not say which
    /// way the next click goes. The badge is the half the fill cannot say.
    fn badge(&self, item: &Item) -> Option<Badge> {
        if self.mode != Mode::Center || item.origin != Source::Center {
            return None;
        }
        // An empty slot has nothing to take out.
        if matches!(item.target, Target::Slot) {
            return None;
        }
        Some(Badge {
            mark: Mark::Minus,
            color: d2d_color(&self.config.theme.tile_target),
        })
    }

    /// The figure on an action tile, and what to call it now. `None` twice
    /// over for an ordinary tile, which has an icon and a fixed name.
    fn action_face(&self, item: &Item) -> (Option<Mark>, Option<String>) {
        match item.target {
            Target::Arrange(mv) => (Some(mark_of(mv)), None),
            Target::NewTab { .. } => (Some(Mark::Plus), None),
            Target::Slot => (Some(Mark::Slot), None),
            // A picture of what the mode does to the panel, not an ornament.
            // Move shows a window taking a side; center shows the middle of
            // the screen held; layout shows the bento being cut.
            Target::Mode(mode) => {
                let mark = match mode {
                    Mode::Move => Mark::Half { left: 0.0, top: 0.0, right: 0.5, bottom: 1.0 },
                    Mode::Center => {
                        Mark::Half { left: 0.28, top: 0.24, right: 0.72, bottom: 0.76 }
                    }
                    Mode::Close => Mark::Cross,
                    // Nine squares where the box under it shows a handful:
                    // the same tiles, and the rest of them. One figure for both
                    // squares, because they are one idea asked of two boxes.
                    Mode::AllApps | Mode::AllBookmarks => Mark::All,
                    Mode::Layout | Mode::Grid => Mark::Bento,
                };
                (Some(mark), None)
            }
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
        let show_detail = self.show_detail();
        let text = d2d_color(&self.config.theme.text);
        let colors = TextColors { title: text, detail: dim(text) };
        // One rule for what a click destroys, read here rather than restated:
        // the loop below cannot call back into `&self`.
        let badges: Vec<Option<Badge>> = self.items.iter().map(|item| self.badge(item)).collect();

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
                running: item
                    .running
                    .map(|_| d2d_color(&self.config.theme.tile_target)),
                badge: badges.get(index).copied().flatten(),
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
                    mark: None,
                    label: "BentoLaunch",
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
        if self.editing() {
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
        if self.editing() {
            return self.edit_key(vk);
        }
        // The two picking modes leave the grid alone, so the keys still work as
        // they always did. Escape is the exception: it has to back out of the
        // mode before it starts closing the panel.
        if self.in_mode() && vk == VK_ESCAPE.0 {
            self.leave_mode();
            return true;
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
                if self.asking_reset {
                    self.asking_reset = false;
                    let _ = self.rebuild_visuals();
                } else if self.settings_open {
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
    /// Whether the bento is being rearranged. The one mode that changes the
    /// shape of the panel rather than what a click on a tile does.
    fn editing(&self) -> bool {
        self.mode == Mode::Layout
    }

    /// Any mode at all. What holds the panel open, what puts "Done" on the
    /// corner button, and what Escape backs out of.
    fn in_mode(&self) -> bool {
        self.mode != Mode::Grid
    }

    fn enter_mode(&mut self, mode: Mode) {
        // A query and a mode are two different things to be in the middle of,
        // and the layout options would be laid out over a grid that is mostly
        // hidden. Filtering wins: it is the thing being typed right now.
        if !self.query.is_empty() || self.sections.is_empty() {
            return;
        }
        // Move mode is the stay latch plus the six squares it exists for.
        // Turning it on here rather than making the user click "Stay open"
        // first is the whole of what one button buys over seven.
        if mode == Mode::Move {
            self.stay = true;
            self.frame_target();
        }
        // Asked on the way in, every time. The archive is only worth carrying
        // while somebody is looking at it, and asking again is what puts a
        // bookmark saved a minute ago in the list.
        if mode == Mode::AllBookmarks {
            server::want_tree();
        }
        self.mode = mode;
        // Nothing picked yet. The options belong to a box, so they wait for one
        // to be clicked rather than guessing at the first.
        self.edit = None;
        // Nor any half of the block: entered from a mode square, both empty
        // squares are waiting, and the one path that does name a half - a
        // click on an empty square - says so after this returns.
        self.filling = None;
        self.set_hover(None);
        self.set_selected(None);
        self.reload();
        log_info!("{mode:?} mode: on, {} box(es)", self.layout.bands().len());
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

    /// A mode lost focus. Ask once more, shortly, whether it is still lost.
    ///
    /// Every mode that takes clicks hands focus away as part of doing its job -
    /// a config write, a `WM_CLOSE`, a shell dialog - and takes it straight
    /// back with `keep_focus`. That is indistinguishable, at the moment it
    /// happens, from a click on another window. One question a beat later tells
    /// them apart: by then the panel is either foreground again or it is not.
    fn ask_again_about_focus(&self) {
        // SAFETY: our own window, on its owning thread. A second SetTimer on
        // the same id replaces the pending one rather than stacking.
        unsafe {
            SetTimer(Some(self.hwnd), DEACTIVATED_TIMER, DEACTIVATED_MS, None);
        }
    }

    /// The answer to that question.
    fn settle_focus(&mut self) {
        // SAFETY: our own window, on its owning thread.
        unsafe {
            let _ = KillTimer(Some(self.hwnd), DEACTIVATED_TIMER);
        }
        if !self.visible || self.menu_open || self.arranging {
            return;
        }
        // SAFETY: a plain read of the foreground window.
        let front = unsafe { GetForegroundWindow() };
        if front == self.hwnd {
            return;
        }
        log_info!("{:?} mode: focus went elsewhere; dismissing", self.mode);
        self.hide(false);
    }

    /// Leaves the panel up. Every one of these is something you do on the way
    /// to using the panel, so finishing should hand back a working panel rather
    /// than dismissing it.
    fn leave_mode(&mut self) {
        log_info!("{:?} mode: off", self.mode);
        // The latch came on with the mode, so it goes off with it. Leaving it
        // on would be a panel that quietly no longer switches to what you click.
        if self.mode == Mode::Move && self.stay {
            self.toggle_stay();
        }
        self.mode = Mode::Grid;
        self.edit = None;
        self.filling = None;
        self.hover_box = None;
        self.reload();
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
        self.layout
            .bands()
            .iter()
            .filter(|band| !band.center)
            .map(|band| band.section)
            .collect()
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
            self.editing_center(),
            g.tile_width * scale,
            g.tile_height * scale,
            g.gap * scale,
        )
    }

    /// The block's own section, which is the one edit mode points at for
    /// either of its halves. They are two boxes on the grid and one thing to
    /// configure, so picking either lights both and answers as one.
    fn center_section(&self) -> Option<usize> {
        self.sections.iter().position(|s| s.center.is_some())
    }

    /// Whether the box being edited is the centre block.
    fn editing_center(&self) -> bool {
        self.edit
            .and_then(|section| self.sections.get(section))
            .is_some_and(|section| section.center.is_some())
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

        match control {
            Control::Done => self.leave_mode(),
            Control::Fewer => {
                self.edit_placement(|p| p.max_items = shown.saturating_sub(1).max(1));
            }
            Control::More => {
                self.edit_placement(|p| p.max_items = shown + 1);
            }
            // One write, and nothing else on the panel moves. A lane is a
            // property of this box, so putting it in one says nothing about
            // any other box - which is the whole of why these replaced the
            // four claim buttons.
            Control::Left | Control::Right | Control::FullWidth => {
                let Some(lane) = control.lane() else { return };
                self.edit_placement(|p| p.side = Some(lane.word().to_owned()));
            }
            // The block's shape and its lists, off the same tables the
            // settings squares step. One list of shapes, whichever surface is
            // asking, so the two cannot drift apart.
            Control::CenterNarrower
            | Control::CenterWider
            | Control::CenterShorter
            | Control::CenterTaller => {
                let (across, down) = match control {
                    Control::CenterNarrower => (-1, 0),
                    Control::CenterWider => (1, 0),
                    Control::CenterShorter => (0, -1),
                    _ => (0, 1),
                };
                if let Some(change) = settings::center_resize(&self.config, across, down) {
                    self.apply_to_center(change);
                }
            }
            Control::CenterHolds => {
                self.apply_to_center(settings::center_holds_next(&self.config));
            }
            Control::CenterOn => {
                self.apply_to_center(settings::center_toggle(&self.config));
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
        }
    }

    /// Write one of the block's own settings and stay pointed at the block.
    ///
    /// Its sections are rebuilt by the reload, and switching it off replaces
    /// its two halves with one placeholder - so the index edit mode was holding
    /// is not the block any more. Every other box is followed by its title; the
    /// block has none.
    fn apply_to_center(&mut self, change: pins::Change) {
        if pins::set(change) {
            self.reload_config();
            self.edit = self.center_section();
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
        let lane = self.lane_named(&title);
        // Its neighbours down its own lane, in the order they are listed. Up
        // and down walk this, which every box is in - they used to walk only
        // the boxes with no claimed side, so both arrows were dead on most of
        // the panel.
        let siblings: Vec<&str> = self
            .sections
            .iter()
            .filter(|s| self.lane_named(&s.title) == lane)
            .map(|s| s.title.as_str())
            .collect();
        let at_lane = siblings.iter().position(|name| *name == title).unwrap_or(0);
        let center = self.editing_center().then(|| {
            let f = &self.config.center;
            CenterState {
                columns: f.columns,
                rows: f.rows,
                most: settings::CENTER_MOST,
                on: f.on(),
            }
        });

        Some(BoxState {
            shown: self.sections.get(section).map_or(0, |s| s.items.len()),
            total: self.sections.get(section).map_or(0, |s| s.total),
            lane,
            boxes: self.layout.bands().len(),
            at_lane,
            lane_len: siblings.len(),
            center,
        })
    }

    fn allows(&self, control: Control) -> bool {
        self.edit_state()
            .is_some_and(|state| control.allowed(&state))
    }

    /// A click on the grid while a mode is on. Every one of these leaves the
    /// panel up: they are all things you do several of in a row.
    ///
    /// Except the two that are the way out. The squares that turn modes on are
    /// the squares that turn them off - a mode that swallowed its own tile
    /// would be a one-way door with Escape as the only key - and a click on the
    /// panel's own padding dismisses in a mode exactly as it does out of one.
    /// Both are what stops a mode reading as a panel that will not go away.
    fn mode_click(&mut self, x: f32, y: f32) {
        // Under the edit options' plate, which answers for its whole rectangle:
        // a mode square behind it is not a mode square you can see.
        let covered = self.options_plate.is_some_and(|plate| plate.contains(x, y));
        if !covered
            && let Some(tile) = self.layout.hit_test(x, y, self.scroll)
            && self
                .items
                .get(tile)
                .is_some_and(|item| matches!(item.target, Target::Mode(_)))
        {
            self.activate(tile);
            self.keep_focus();
            return;
        }
        match self.mode {
            // These leave the grid alone: a click on a tile means what it
            // always means. Move mode picks the window with it, and all-apps
            // launches with it, which is the whole of what it is for.
            Mode::Grid | Mode::Move | Mode::AllApps | Mode::AllBookmarks => {}
            Mode::Layout => self.edit_click(x, y),
            Mode::Center => {
                match self.layout.hit_test(x, y, self.scroll) {
                    Some(tile) => self.toggle_center(tile),
                    None => return self.hide(true),
                }
                self.keep_focus();
            }
            Mode::Close => {
                match self.layout.hit_test(x, y, self.scroll) {
                    Some(tile) => self.close_tile(tile),
                    None => return self.hide(true),
                }
                self.keep_focus();
            }
        }
    }

    /// What favoriting this tile would write down.
    ///
    /// A string the shell or the browser can be handed later, never a handle:
    /// a favorite outlives the window, the tab and the browser it came from,
    /// and a config file has nowhere to put an HWND.
    fn center_target(&self, item: &Item) -> Option<String> {
        if matches!(item.target, Target::Slot) {
            return None;
        }
        item.link
            .clone()
            .or_else(|| item.shell_target().map(str::to_owned))
            // A window stands for its app, exactly as "Pin this app" reads it.
            .or_else(|| {
                matches!(item.target, Target::Window(_))
                    .then(|| item.icon_source.clone())
                    .flatten()
            })
    }

    /// What favoriting this tile would write down, and which half it lands in.
    ///
    /// `None` when the block would not show it. A block set to apps only still
    /// keeps its site list, but writing to a list that is not on the panel is a
    /// click with no visible result - which reads as the panel ignoring you.
    fn center_would(&self, item: &Item) -> Option<(pins::Half, String)> {
        let half = pins::Half::of(item.kind);
        // Already in the block, so it is on screen and has to be removable
        // however the block is set. Only what would be *added* to a half that
        // is not drawn is refused.
        if item.origin != Source::Center
            && !self.config.center.contents.shows(half.index())
        {
            return None;
        }
        // Nor when that list has no empty square left. The write would be taken
        // and nothing would be drawn - the block shows `slots` of a list and
        // keeps the rest - which is the same click with no visible result a
        // half that is not drawn at all would give.
        //
        // A block that is not drawn is not that case. There is no square to
        // land on and there does not need to be: the write is what makes the
        // block appear. Refusing here is what made an off block a dead end - no
        // way in from the panel, and the way back on only in edit mode.
        //
        // Drawn, not `center.on()`: an empty block collapses whatever its
        // shape says, so "on" is not the same question as "there".
        let drawn = self.sections.iter().any(|section| section.center.is_some());
        if item.origin != Source::Center
            && drawn
            && self.landing_slots()[half.index()].is_none()
        {
            return None;
        }
        Some((half, self.center_target(item)?))
    }

    /// Put a tile into the centre block, or take it back out.
    ///
    /// One gesture both ways. The block lights up what it is already holding,
    /// so the same click that added something is the click that removes it -
    /// which is the whole of "manage center" without a second surface.
    fn toggle_center(&mut self, tile: usize) {
        let Some(item) = self.items.get(tile).cloned() else { return };
        // An empty square is not something to write down - it is where things
        // land. Clicking one aims the mode at that half, and clicking the
        // square it is already aimed at turns the mode off, which is the rule
        // every mode square follows: its own turns it off, another switches.
        if matches!(item.target, Target::Slot) {
            let half = self.slot_half(tile);
            if half.is_some() && half == self.filling {
                self.leave_mode();
            } else {
                self.filling = half;
                self.refresh_landing();
            }
            return;
        }
        let Some((half, target)) = self.center_would(&item) else {
            log_info!("center: nothing to write down for \"{}\"", item.title);
            return;
        };

        let changed = if item.origin == Source::Center {
            log_info!("center: removing {target}");
            pins::forget_in_center(&target)
        } else {
            log_info!("center: adding {target} to {}", half.key());
            pins::add_to_center(half, &target)
        };
        if !changed {
            return;
        }
        // Read straight back rather than waiting on the watcher, so the block
        // has changed by the time the finger is off the button.
        self.reload_config();
        let _ = self.rebuild_visuals();
    }

    /// Close the window behind a tile.
    ///
    /// `WM_CLOSE`, which is the same polite ask the taskbar's "Close window"
    /// makes: the app gets to prompt about unsaved work and gets to refuse.
    /// Nothing here ever terminates a process.
    fn close_tile(&mut self, tile: usize) {
        let Some(item) = self.items.get(tile).cloned() else { return };
        let Some(handle) = self.window_for(&item) else {
            log_info!("close: nothing open behind \"{}\"", item.title);
            return;
        };
        if handle.hwnd() == self.hwnd {
            log_warn!("close: refusing to close bentolaunch's own window");
            return;
        }
        if self.config.dry_run {
            log_dry!("would close \"{}\" ({:#x})", item.title, handle.raw());
            return;
        }
        log_info!("close: \"{}\" ({:#x})", item.title, handle.raw());
        // Posted, not sent: a window that puts up a "save your work?" dialog
        // would otherwise block this thread until it is answered, and this
        // thread is the one drawing the panel the dialog is in front of.
        //
        // SAFETY: a plain post to another window. The hooks bring the grid up
        // to date when the window actually goes.
        unsafe {
            let _ = PostMessageW(Some(handle.hwnd()), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
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
        let band = self.layout.bands().get(band);
        // Both halves of the block light together: one thing to configure.
        let section = match band.is_some_and(|band| band.center) && self.editing_center() {
            true => self.edit,
            false => band.map(|band| band.section),
        };
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
        let Some(renderer) = &self.renderer else { return };
        let radius = self.config.grid.corner_radius * self.scale() * 1.5;
        for (band, (surface, rings)) in self.box_faces.iter().enumerate() {
            let color = d2d_color_of(self.box_color(band));
            let _ = renderer.draw_shape(surface, rings, radius, color);
        }
    }

    fn set_hover_box(&mut self, x: f32, y: f32) {
        let next = self.box_at(x, y).map(|band| band.section);
        if next != self.hover_box {
            self.hover_box = next;
            self.refresh_boxes();
        }
    }

    /// Which box a panel-local point falls in, for editing.
    ///
    /// The centre counts, and is looked for first: its band sits inside the
    /// rectangle of whatever box wrapped around it, so the tree band would
    /// answer for a click that landed squarely on the block.
    fn box_at(&self, x: f32, y: f32) -> Option<&Band> {
        let content_y = y + self.scroll;
        let hit = |center: bool| {
            self.layout
                .bands()
                .iter()
                .find(move |band| band.center == center && band.rect.contains(x, content_y))
        };
        hit(true).or_else(|| hit(false))
    }

    /// Point edit mode at whichever box covers a panel-local point. Bands tile
    /// the panel, so anywhere inside it answers.
    fn pick_box(&mut self, x: f32, y: f32) {
        let Some(band) = self.box_at(x, y) else {
            return;
        };
        // Clicking the picked box again puts the options away. The overlay sits
        // over the middle of the panel, so there has to be a way to clear it
        // without leaving the mode.
        // Either half of the block means the block. They are two boxes on the
        // grid and one thing to configure.
        let section = match band.center {
            true => self.center_section().unwrap_or(band.section),
            false => band.section,
        };
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
                side: section.side.clone(),
                columns: section.columns,
                max_items: section.max_items,
            })
    }

    /// Which band across the panel a section sits in.
    fn lane_named(&self, title: &str) -> Lane {
        lane_of(self.config.sections.iter().find(|s| s.title == title))
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
                    self.leave_mode();
                    true
                }
                LEFT => self.edit_step(-1),
                RIGHT => self.edit_step(1),
                _ => false,
            };
        }

        match vk {
            ESCAPE | ENTER => {
                self.leave_mode();
                true
            }
            // Left and right pick the lane, up and down move down it. The
            // arrows say the same thing the squares do.
            LEFT => self.click_control(Control::Left),
            RIGHT => self.click_control(Control::Right),
            UP => self.click_control(Control::MoveUp),
            DOWN => self.click_control(Control::MoveDown),
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
        if title.is_empty() {
            return String::new();
        }
        let placement = self.placement_of(title);
        let shown = match placement.max_items {
            0 => String::from("all"),
            capped => format!("{capped}"),
        };
        // The lane exactly as the config spells it, so what the squares do and
        // what the file says are visibly the same thing.
        let sits = self.lane_named(title).word();

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

    /// Only pins bentolaunch owns can be removed. A taskbar entry belongs to the
    /// taskbar, and unpinning it there is Windows' business, not bentolaunch's
    /// (safety rule 3).
    fn removable(&self, tile: usize) -> bool {
        self.items.get(tile).is_some_and(|i| i.origin == Source::Manual)
    }

    /// Window tiles are MRU ordered by the foreground hook, so a saved order
    /// would fight the hook on every focus change. Pinned sections have an order
    /// that is bentolaunch's to keep.
    ///
    /// Never while filtering: writing back a subset's order would drop every
    /// pin the query hid.
    fn draggable(&self, tile: usize) -> bool {
        self.query.is_empty()
            && self.items.get(tile).is_some_and(|i| {
                matches!(i.origin, Source::Manual | Source::Taskbar | Source::Center)
                    // An empty slot is a place, not a tile. There is nothing to
                    // pick up and nothing to write down.
                    && !matches!(i.target, Target::Slot)
            })
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
            // Past the threshold on a tile bentolaunch cannot rearrange: nothing to
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
            Source::Manual | Source::Center => item.shell_target().map(str::to_owned),
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
            // Which half is being dragged in, so a drag inside the centre
            // rewrites that list and only that one.
            Source::Center => match section.center {
                Some(1) => pins::order_center(pins::Half::Sites, &keys),
                _ => pins::order_center(pins::Half::Apps, &keys),
            },
            // Ordered by the foreground hook and the browser, not by bentolaunch.
            Source::Windows | Source::Extra | Source::Running | Source::Tabs
            | Source::Bookmarks => false,
            // Alphabetical, as every all-apps list on Windows is, and the
            // browser's own order for the bookmarks. Dragging one to a new
            // place would be dragging it back on the next summon.
            Source::AllApps | Source::AllBookmarks => false,
            // A fixed set in a fixed order. Nowhere to write one down.
            Source::Moves | Source::Modes => false,
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
    /// is the thing a user most often wants to pin, and bentolaunch is already showing
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
            Some(menu::CMD_CENTER | menu::CMD_UNCENTER) => {
                if let Some(tile) = tile {
                    self.toggle_center(tile);
                }
            }
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
            Some(menu::CMD_EDIT_LAYOUT) => self.enter_mode(Mode::Layout),
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
                // already on screen and bentolaunch already knows its path.
                Target::Window(_) => {
                    if item.icon_source.is_some() {
                        // Named by where it goes, not by what it does to it.
                        // "Pin" was one word for the write the tray calls "Add
                        // app...", and neither said where the tile would turn
                        // up. The name of the app is deliberately not in here:
                        // what is available is the executable's stem, which
                        // reads as "Pin obs64".
                        entries.push(Some(menu::Entry::new(menu::CMD_PIN_APP, add_label())));
                    }
                }
                Target::Shell(_) => {
                    if self.removable(index) {
                        // The box it is actually in, which is not always the
                        // box a new one would go to.
                        let label = match self.section_of(index).map(|s| s.title.as_str()) {
                            Some(title) if !title.is_empty() => format!("Remove from {title}"),
                            _ => "Remove".to_string(),
                        };
                        entries.push(Some(menu::Entry::new(menu::CMD_UNPIN, label)));
                    }
                }
                // Bookmarking a tab arrives with the rest of Milestone 4.
                Target::Tab { .. } => {}
                // Fixed tiles. Nothing to pin, unpin or locate.
                Target::Arrange(_) | Target::Stay | Target::NewTab { .. } | Target::Slot
                | Target::Mode(_) => {}
            }
            // The centre block, as a second path to the mode. Same rule as
            // "Unpin": the gesture is a mode, and this is here for anyone who
            // reaches for a right-click first.
            if self.center_target(item).is_some() {
                entries.push(Some(if item.origin == Source::Center {
                    menu::Entry::new(menu::CMD_UNCENTER, "Remove from Center")
                } else {
                    menu::Entry::new(menu::CMD_CENTER, "Add to Center")
                }));
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
            // something that is not bentolaunch, and handing out a code in that
            // state would be handing it to whatever answered.
            self.say(
                "Cannot pair right now",
                &format!(
                    "Another process is using port {port}, so BentoLaunch's browser \
                     bridge is not listening.\n\n\
                     Close whatever is using it, or set a different browser.port in \
                     bentolaunch.toml, then try again."
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
                 Open the BentoLaunch extension's options page, choose \"Pair with \
                 BentoLaunch\", and type this code.\n\n\
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

    /// The only dialog bentolaunch has. Everything else it draws itself, but a
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

        store::reconfigure(&self.config.sections, &self.config.center);
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
            WM_TIMER if wparam.0 == DEACTIVATED_TIMER => {
                self.settle_focus();
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
                        .position(|(setting, rect, _)| {
                            rect.contains(hx, hy) && setting.applies(&self.config)
                        });
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
                if self.editing() {
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
            WM_MOUSELEAVE if self.editing() => {
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
                // A mode takes the click. Launching something from a layout
                // editor, or while picking center, would be a click nobody
                // meant.
                if self.mode.takes_clicks() {
                    self.handled_down = true;
                    self.mode_click(x, y);
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
                        // Travelled, and over something bentolaunch can rearrange.
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
                if handled || self.in_mode() || self.menu_open_big || self.settings_open {
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
            //
            // The three modes that take clicks off the grid do not dismiss on
            // the spot: each of them writes the config or closes a window, and
            // both of those hand focus somewhere else for a moment. A panel
            // that vanished on its own write would be one click of work per
            // summon. They ask again a moment later instead - see
            // `settle_focus` - because a mode that will not go away when you
            // click off it is the thing that reads as a frozen PC, and holding
            // the panel open through *every* loss was doing exactly that.
            //
            // Move mode is deliberately not among them. Nothing it does needs
            // the panel to survive losing focus - `arranging` already covers the
            // window it raises.
            WM_ACTIVATE if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE && !self.menu_open => {
                if self.arranging {
                    return Some(LRESULT(0));
                }
                if self.mode.takes_clicks() {
                    self.ask_again_about_focus();
                } else {
                    self.hide(false);
                }
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

/// Open `bentolaunch.toml` in whatever the user edits TOML with. Falls back to
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

pub const CLASS_NAME: PCWSTR = w!("bentolaunch_panel");

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
        // WS_EX_TOOLWINDOW: keeps bentolaunch out of alt-tab and the taskbar.
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOREDIRECTIONBITMAP,
            CLASS_NAME,
            w!("BentoLaunch"),
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

/// Which band across the panel a section asks for.
fn lane_of(section: Option<&crate::config::SectionConfig>) -> Lane {
    section
        .and_then(|s| s.side.as_deref())
        .and_then(Lane::parse)
        .unwrap_or_default()
}

/// The rectangle a set of rings needs to be drawn in, with room for the stroke
/// that hangs outside their corners. `None` when there is nothing to draw.
fn covering(rings: &[Vec<(f32, f32)>], margin: f32) -> Option<GridRect> {
    let (mut left, mut top) = (f32::MAX, f32::MAX);
    let (mut right, mut bottom) = (f32::MIN, f32::MIN);
    for (x, y) in rings.iter().flatten() {
        left = left.min(*x);
        top = top.min(*y);
        right = right.max(*x);
        bottom = bottom.max(*y);
    }
    (right > left && bottom > top).then_some(GridRect {
        x: left - margin,
        y: top - margin,
        w: right - left + 2.0 * margin,
        h: bottom - top + 2.0 * margin,
    })
}

/// A box's face while editing: the surface it is drawn on, and the shape to
/// redraw on it when the colour changes.
type BoxFace = (CompositionDrawingSurface, Vec<Vec<(f32, f32)>>);

/// A composition colour as Direct2D wants it. The theme is parsed once into
/// the composition form; this is the one place the other form is needed.
fn d2d_color_of(color: Color) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: color.R as f32 / 255.0,
        g: color.G as f32 / 255.0,
        b: color.B as f32 / 255.0,
        a: color.A as f32 / 255.0,
    }
}

/// The same colour at full strength. A ring is drawn faint enough to sit behind
/// the tiles; the words on it have to be read.
fn opaque(color: D2D1_COLOR_F) -> D2D1_COLOR_F {
    D2D1_COLOR_F { a: 1.0, ..color }
}
