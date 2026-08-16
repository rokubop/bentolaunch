//! The loopback WebSocket the extension dials into.
//!
//! The extension connects out, not the reverse: MV3 service workers die on
//! idle and socket traffic is what keeps one alive.
//!
//! One thread per connection, owning its socket. Commands from the UI arrive
//! on a channel it drains between reads, so nothing else writes to the stream.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::protocol::WebSocketConfig;
use tungstenite::{Message, WebSocket};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use crate::browser::gate::{Policy, Refusal};
use crate::browser::protocol::{Inbound, Outbound, Tab};
use crate::{log_info, log_warn};

/// Posted to the panel when the tab list changes.
pub const WM_TABS_CHANGED: u32 = WM_APP + 0x40;

/// How long a read waits before the loop checks for outgoing commands.
const READ_TIMEOUT: Duration = Duration::from_millis(250);
/// Comfortably inside Chrome's ~30s service worker idle timeout.
const PING_EVERY: Duration = Duration::from_secs(20);

/// Live connections. One browser needs one; the cap is here so nothing local
/// can spend flick's threads and per-connection buffers by dialling in a loop.
const MAX_CONNECTIONS: usize = 8;
static LIVE: AtomicUsize = AtomicUsize::new(0);

/// More tabs than anyone has open. A list past this is a bug or an attempt.
const MAX_TABS: usize = 2_000;

/// tungstenite defaults to 64 MiB messages and a 128 KiB buffer per connection.
/// A tab list with favicons is tens of KiB, so these are still generous.
fn limits() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(4 << 20))
        .max_frame_size(Some(1 << 20))
        .read_buffer_size(16 * 1024)
}

/// Tabs carry their connection: focus goes back to the browser that owns the
/// tab, not whichever answered last.
#[derive(Debug, Clone)]
pub struct Owned {
    pub connection: u64,
    pub tab: Tab,
}

struct State {
    tabs: HashMap<u64, Vec<Tab>>,
    outbox: HashMap<u64, Sender<Outbound>>,
}

fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(State { tabs: HashMap::new(), outbox: HashMap::new() })
    })
}

pub fn tabs() -> Vec<Owned> {
    let Ok(state) = state().lock() else {
        log_warn!("browser state is poisoned; reporting no tabs");
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut connections: Vec<&u64> = state.tabs.keys().collect();
    connections.sort();
    for connection in connections {
        for tab in &state.tabs[connection] {
            out.push(Owned { connection: *connection, tab: tab.clone() });
        }
    }
    out
}

/// Fire and forget. flick has already hidden by the time the switch lands.
pub fn focus(connection: u64, tab_id: i64, window_id: i64) -> bool {
    let Ok(state) = state().lock() else {
        return false;
    };
    let Some(outbox) = state.outbox.get(&connection) else {
        log_warn!("browser connection {connection} has gone; cannot focus tab {tab_id}");
        return false;
    };
    outbox.send(Outbound::Focus { tab_id, window_id }).is_ok()
}

/// No-op unless enabled and paired. Nothing binds a port otherwise.
pub fn start(hwnd: HWND, config: &crate::config::Browser) {
    if !config.enabled {
        return;
    }

    // Mint on first use so pairing is copy-paste, not invent-your-own-secret.
    let mut token = config.token.clone();
    if token.is_empty()
        && let Some(fresh) = crate::browser::gate::generate_token()
        && let Some(written) = crate::pins::set_browser_token(&fresh)
    {
        log_info!("generated a browser bridge token; it is in flick.toml under [browser]");
        token = written;
    }

    let Some(policy) = Policy::new(&config.allow, &token) else {
        log_warn!(
            "browser bridge is enabled but not paired; not listening. \
             Add the origin from the extension's options page to browser.allow"
        );
        return;
    };

    // Explicit loopback. The unspecified address would put the tab list on
    // every interface.
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, config.port));
    let listener = match TcpListener::bind(address) {
        Ok(listener) => listener,
        Err(e) => {
            log_warn!("could not listen on {address} ({e}); browser bridge is off");
            return;
        }
    };

    let hwnd = hwnd.0 as isize;
    std::thread::spawn(move || accept_loop(listener, policy, hwnd));
    log_info!("browser bridge listening on {address}");
}

fn accept_loop(listener: TcpListener, policy: Policy, hwnd: isize) {
    static NEXT: AtomicU64 = AtomicU64::new(1);

    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => stream,
            Err(e) => {
                log_warn!("browser bridge accept failed: {e}");
                continue;
            }
        };
        let loopback = stream
            .peer_addr()
            .map(|peer| peer.ip().is_loopback())
            .unwrap_or(false);

        if LIVE.load(Ordering::Relaxed) >= MAX_CONNECTIONS {
            log_warn!("browser bridge at {MAX_CONNECTIONS} connections; dropping this one");
            continue;
        }

        let connection = NEXT.fetch_add(1, Ordering::Relaxed);
        let policy = policy.clone();
        LIVE.fetch_add(1, Ordering::Relaxed);
        std::thread::spawn(move || {
            serve(stream, loopback, &policy, connection, hwnd);
            disconnect(connection, hwnd);
            LIVE.fetch_sub(1, Ordering::Relaxed);
        });
    }
}

fn serve(stream: TcpStream, loopback: bool, policy: &Policy, connection: u64, hwnd: isize) {
    if stream.set_read_timeout(Some(READ_TIMEOUT)).is_err() {
        log_warn!("browser connection {connection}: could not set a read timeout");
        return;
    }

    // RefCell because a failed handshake hands the callback back inside the
    // error, so a `&mut` capture would still be borrowed below.
    let refusal: RefCell<Option<Refusal>> = RefCell::new(None);
    let handshake = tungstenite::accept_hdr_with_config(
        stream,
        |request: &Request, response: Response| -> Result<Response, ErrorResponse> {
            let origin = request
                .headers()
                .get("Origin")
                .and_then(|value| value.to_str().ok());
            match policy.admit(loopback, origin, request.uri().path()) {
                Ok(()) => Ok(response),
                Err(reason) => {
                    // The caller is told only "no". Which gate it failed is a
                    // hint worth withholding.
                    *refusal.borrow_mut() = Some(reason);
                    Err(ErrorResponse::new(None))
                }
            }
        },
        Some(limits()),
    );

    let mut socket = match handshake {
        Ok(socket) => socket,
        Err(e) => {
            let refused = refusal.borrow_mut().take();
            match refused {
                Some(Refusal::UnknownOrigin(origin)) => {
                    log_warn!("browser connection refused: origin {origin} is not paired");
                    // Extensions only. A page origin here is a site trying its
                    // luck; do not tell anyone how to allowlist it.
                    if origin.starts_with("chrome-extension://")
                        || origin.starts_with("moz-extension://")
                    {
                        log_info!("to pair it, add \"{origin}\" to browser.allow in flick.toml");
                    }
                }
                Some(reason) => log_warn!("browser connection refused: {}", reason.reason()),
                None => log_info!("browser handshake failed: {e}"),
            }
            return;
        }
    };

    let (sender, commands) = channel();
    if let Ok(mut state) = state().lock() {
        state.outbox.insert(connection, sender);
    }
    log_info!("browser connected (connection {connection})");

    pump(&mut socket, &commands, connection, hwnd);
    let _ = socket.close(None);
}

fn pump(
    socket: &mut WebSocket<TcpStream>,
    commands: &Receiver<Outbound>,
    connection: u64,
    hwnd: isize,
) {
    let mut last_ping = Instant::now();

    loop {
        match socket.read() {
            Ok(Message::Text(text)) => {
                if handle(&text, connection) {
                    post(hwnd, WM_TABS_CHANGED);
                }
            }
            Ok(Message::Close(_)) => break,
            // Binary and continuation frames are not part of this protocol.
            Ok(_) => {}
            Err(tungstenite::Error::Io(e))
                if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => break,
            Err(e) => {
                log_warn!("browser connection {connection} read failed: {e}");
                break;
            }
        }

        loop {
            match commands.try_recv() {
                Ok(command) => {
                    if !send(socket, &command, connection) {
                        return;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }

        if last_ping.elapsed() >= PING_EVERY {
            last_ping = Instant::now();
            if !send(socket, &Outbound::Ping, connection) {
                return;
            }
        }
    }
}

fn send(socket: &mut WebSocket<TcpStream>, message: &Outbound, connection: u64) -> bool {
    let Ok(text) = serde_json::to_string(message) else {
        return true;
    };
    match socket.send(Message::Text(text.into())) {
        Ok(()) => true,
        Err(e) => {
            log_warn!("browser connection {connection} write failed: {e}");
            false
        }
    }
}

/// Returns whether the tab list changed.
fn handle(text: &str, connection: u64) -> bool {
    let message: Inbound = match serde_json::from_str(text) {
        Ok(message) => message,
        Err(e) => {
            log_warn!("browser connection {connection} sent something unreadable: {e}");
            return false;
        }
    };

    match message {
        Inbound::Tabs { tabs, icons } => {
            log_info!(
                "browser connection {connection}: {} tab(s), {} new icon(s)",
                tabs.len(),
                icons.len()
            );
            if tabs.len() > MAX_TABS {
                log_warn!(
                    "browser connection {connection} sent {} tabs; ignoring the list",
                    tabs.len()
                );
                return false;
            }
            for (key, icon) in &icons {
                match icon.to_pixels() {
                    Some(pixels) => crate::shell::icons::put_favicon(key, pixels),
                    None => log_warn!("browser sent an unusable favicon for {key}"),
                }
            }
            if let Ok(mut state) = state().lock() {
                state.tabs.insert(connection, tabs);
            }
            true
        }
        Inbound::Pong => false,
    }
}

fn disconnect(connection: u64, hwnd: isize) {
    if let Ok(mut state) = state().lock() {
        state.tabs.remove(&connection);
        state.outbox.remove(&connection);
    }
    log_info!("browser disconnected (connection {connection})");
    post(hwnd, WM_TABS_CHANGED);
}

fn post(hwnd: isize, message: u32) {
    let hwnd = HWND(hwnd as *mut core::ffi::c_void);
    // SAFETY: posting to a window owned by the UI thread. A destroyed window
    // makes this fail harmlessly, which is why nothing checks the result.
    unsafe {
        let _ = PostMessageW(Some(hwnd), message, WPARAM(0), LPARAM(0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tungstenite::client::IntoClientRequest;

    const ORIGIN: &str = "chrome-extension://abcdefghijklmnopabcdefghijklmnop";
    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    /// Proves the gate is wired into the handshake, not just correct alone.
    fn serving() -> u16 {
        let policy = Policy::new(&[ORIGIN.to_string()], TOKEN).unwrap();
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || accept_loop(listener, policy, 0));
        port
    }

    fn dial(port: u16, origin: Option<&str>, token: &str) -> bool {
        let mut request = format!("ws://127.0.0.1:{port}/{token}")
            .into_client_request()
            .unwrap();
        if let Some(origin) = origin {
            request
                .headers_mut()
                .insert("Origin", origin.parse().unwrap());
        }
        tungstenite::connect(request).is_ok()
    }

    #[test]
    fn the_paired_extension_completes_a_handshake() {
        let port = serving();
        assert!(dial(port, Some(ORIGIN), TOKEN));
    }

    #[test]
    fn a_page_origin_never_gets_past_the_handshake() {
        let port = serving();
        assert!(!dial(port, Some("https://evil.example"), TOKEN));
        assert!(!dial(port, None, TOKEN));
    }

    #[test]
    fn the_right_origin_without_the_token_is_still_refused() {
        let port = serving();
        assert!(!dial(port, Some(ORIGIN), "guessed"));
        assert!(!dial(port, Some(ORIGIN), ""));
    }

    #[test]
    fn the_connection_cap_holds_and_recovers() {
        let port = serving();
        // Hold the cap open, then prove the next one is turned away.
        let held: Vec<_> = (0..MAX_CONNECTIONS)
            .filter_map(|_| {
                let mut request = format!("ws://127.0.0.1:{port}/{TOKEN}")
                    .into_client_request()
                    .unwrap();
                request.headers_mut().insert("Origin", ORIGIN.parse().unwrap());
                tungstenite::connect(request).ok()
            })
            .collect();
        assert_eq!(held.len(), MAX_CONNECTIONS, "the cap must admit this many");
        assert!(!dial(port, Some(ORIGIN), TOKEN), "one past the cap must be refused");

        // Closing them frees the slots again.
        drop(held);
        let mut recovered = false;
        for _ in 0..40 {
            if dial(port, Some(ORIGIN), TOKEN) {
                recovered = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(recovered, "slots must come back when connections close");
    }

    #[test]
    fn a_refused_connection_leaves_no_state_behind() {
        let port = serving();
        let before = tabs().len();
        assert!(!dial(port, Some("https://evil.example"), TOKEN));
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(tabs().len(), before, "a refused caller must register nothing");
    }
}
