//! flick — a hotkey-summoned grid of everything worth switching to.
//!
//! Milestone 1 is a **dry run**: it enumerates and renders the real system, but
//! every activation is a no-op that logs what it would have done. See DESIGN.md.

mod browser;
mod config;
mod log;
mod model;
mod pins;
mod safety;
mod shell;
mod ui;
mod watch;

use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, MSG, TranslateMessage,
};

use crate::config::Config;
use crate::model::store;
use crate::shell::icons;
use crate::ui::panel::Panel;
use crate::ui::tray;

fn main() {
    log::init();
    // Installed before anything can panic, and before a window exists to strand.
    safety::install_panic_hook();
    safety::start_watchdog();

    let config = Config::load();
    let sections = config.sections.clone();
    let bridge = config.browser.clone();
    log_info!(
        "flick starting — dry_run={}, hotkey={}",
        config.dry_run,
        config.hotkey
    );
    if config.dry_run {
        log_info!("DRY RUN: clicks will be logged, not acted on");
    }

    let mut panel = match Panel::create(config) {
        Ok(panel) => panel,
        Err(e) => {
            log_error!("could not create the panel window: {e}");
            return;
        }
    };
    let hwnd = panel.hwnd();

    // Enumerate once, then stay current from hooks. Never scan on the hotkey.
    store::init(hwnd, &sections);
    store::install_hooks(hwnd);
    icons::start(hwnd);
    watch::start(hwnd);
    tray::install(hwnd);
    browser::server::start(hwnd, &bridge);

    log_info!("ready — press the hotkey to summon, or use the tray icon");
    pump();

    // Ordered teardown: stop the event feed, drop the tray, then the window.
    store::uninstall_hooks();
    tray::remove(hwnd);
    panel.hide(false);
    drop(panel);
    log_info!("flick exited cleanly");
}

fn pump() {
    let mut msg = MSG::default();
    loop {
        // SAFETY: msg is a live stack local; GetMessageW blocks until a message
        // arrives for this thread.
        let result = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        match result.0 {
            0 => break,
            -1 => {
                log_error!("GetMessageW failed; shutting down rather than spinning");
                break;
            }
            _ => {
                safety::beat();
                // SAFETY: both operate on the message just retrieved.
                unsafe {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
    }
}
