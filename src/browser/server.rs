//! The loopback WebSocket the extension dials into.
//!
//! flick listens; the extension connects out. That direction is not a
//! preference — MV3 service workers die on idle, and an open socket carrying
//! traffic is what keeps one alive (DESIGN.md, "Browser tabs").
//!
//! One thread accepts, one thread per connection after that. A connection
//! thread owns its socket outright: commands from the UI reach it through a
//! channel it drains between reads, so nothing else ever writes to the stream.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use tungstenite::handshake::server::{ErrorResponse, Request, Response};
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

/// Which connection a tab came from, so a focus command goes back to the
/// browser that owns it rather than to whichever one answered last.
#[derive(Debug, Clone)]
pub struct Owned {
    pub connection: u64,
    pub tab: Tab,
}

struct State {
    /// Tabs per live connection. Two browsers can be connected at once.
    tabs: HashMap<u64, Vec<Tab>>,
    outbox: HashMap<u64, Sender<Outbound>>,
}

fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(State { tabs: HashMap::new(), outbox: HashMap::new() })
    })
}

/// Every tab from every connected browser, connection order.
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

/// Ask the browser that owns this tab to switch to it.
///
/// Fire and forget: the switch happens in the browser, and flick has already
/// hidden itself by the time it lands.
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

/// Start listening, if config says to and pairing is complete.
///
/// Silent no-op when the bridge is off, which is the default. Nothing binds a
/// port until the user has both enabled it and paired an extension.
pub fn start(hwnd: HWND, config: &crate::config::Browser) {
    if !config.enabled {
        return;
    }

    // First run with the bridge on: mint a token and write it back, so pairing
    // is copy-paste rather than "invent your own secret".
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
             Needs browser.token and at least one browser.allow origin"
        );
        return;
    };

    // Loopback explicitly. Binding the unspecified address would put the tab
    // list on every interface on the machine.
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

        let connection = NEXT.fetch_add(1, Ordering::Relaxed);
        let policy = policy.clone();
        std::thread::spawn(move || {
            serve(stream, loopback, &policy, connection, hwnd);
            disconnect(connection, hwnd);
        });
    }
}

/// Run one connection from handshake to close.
fn serve(stream: TcpStream, loopback: bool, policy: &Policy, connection: u64, hwnd: isize) {
    if stream.set_read_timeout(Some(READ_TIMEOUT)).is_err() {
        log_warn!("browser connection {connection}: could not set a read timeout");
        return;
    }

    // Interior mutability because a failed handshake hands the callback back
    // inside the error, so a plain `&mut` capture would still be borrowed here.
    let refusal: RefCell<Option<Refusal>> = RefCell::new(None);
    let handshake = tungstenite::accept_hdr(
        stream,
        |request: &Request, response: Response| -> Result<Response, ErrorResponse> {
            let origin = request
                .headers()
                .get("Origin")
                .and_then(|value| value.to_str().ok());
            match policy.admit(loopback, origin, request.uri().path()) {
                Ok(()) => Ok(response),
                Err(reason) => {
                    // The caller is told nothing but "no". Which gate it failed
                    // is a hint worth withholding.
                    *refusal.borrow_mut() = Some(reason);
                    Err(ErrorResponse::new(None))
                }
            }
        },
    );

    let mut socket = match handshake {
        Ok(socket) => socket,
        Err(e) => {
            let refused = refusal.borrow_mut().take();
            match refused {
                Some(Refusal::UnknownOrigin(origin)) => {
                    log_warn!("browser connection refused: origin {origin} is not paired");
                    // Offered only for an extension. A page origin reaching
                    // this socket is a site trying its luck, and telling anyone
                    // how to allowlist it would be advice worth not giving.
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

/// Read until the socket closes, draining outgoing commands between reads.
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
            // Ping/pong at the protocol level is handled by tungstenite on the
            // next write; binary and continuation frames are not part of this
            // protocol and are ignored rather than treated as an error.
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
        Inbound::Tabs { tabs } => {
            log_info!("browser connection {connection}: {} tab(s)", tabs.len());
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

    /// A real listener on a real loopback port, served by the real accept loop.
    /// The gate is unit-tested on its own; this is here to prove it is actually
    /// wired into the handshake.
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
        // Right token, wrong origin: this is the drive-by case.
        assert!(!dial(port, Some("https://evil.example"), TOKEN));
        // And no origin at all, which is what a non-browser sends.
        assert!(!dial(port, None, TOKEN));
    }

    #[test]
    fn the_right_origin_without_the_token_is_still_refused() {
        let port = serving();
        assert!(!dial(port, Some(ORIGIN), "guessed"));
        assert!(!dial(port, Some(ORIGIN), ""));
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
