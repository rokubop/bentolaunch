//! The loopback WebSocket the extension dials into.
//!
//! The extension connects out, not the reverse: MV3 service workers die on
//! idle and socket traffic is what keeps one alive.
//!
//! One thread per connection, owning its socket. Commands from the UI arrive
//! on a channel it drains between reads, so nothing else writes to the stream.
//!
//! A connection is not a connection until it has proved itself. The handshake
//! only gets to look at headers, so admission finishes over the socket: nothing
//! is registered, and no tab list is accepted, before the exchange in `gate`
//! has run both ways.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::protocol::WebSocketConfig;
use tungstenite::{Message, WebSocket};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use crate::browser::crypto;
use crate::browser::gate::{self, Admission, Refusal};
use crate::browser::peers::{self, Peer};
use crate::browser::protocol::{Bookmark, Inbound, Outbound, PROTOCOL, Tab};
use crate::{log_info, log_warn};

/// Posted to the panel when the tab list changes.
pub const WM_TABS_CHANGED: u32 = WM_APP + 0x40;
/// Posted when a browser pairs, so the tray stops saying nothing is paired.
pub const WM_PAIRED: u32 = WM_APP + 0x41;

/// How long a read waits before the loop checks for outgoing commands.
const READ_TIMEOUT: Duration = Duration::from_millis(250);
/// Comfortably inside Chrome's ~30s service worker idle timeout.
const PING_EVERY: Duration = Duration::from_secs(20);

/// Live connections. One browser needs one; the cap is here so nothing local
/// can spend bentolaunch's threads and per-connection buffers by dialling in a loop.
const MAX_CONNECTIONS: usize = 8;
static LIVE: AtomicUsize = AtomicUsize::new(0);

/// More tabs than anyone has open. A list past this is a bug or an attempt.
const MAX_TABS: usize = 2_000;
/// The bar, not the whole tree. A bar this long is a bug or an attempt.
const MAX_BOOKMARKS: usize = 500;

/// How long a caller has to finish proving itself. Generous for a local round
/// trip, short enough that a connection holding a slot in silence is dropped.
const NEGOTIATION: Duration = Duration::from_secs(5);

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
    bookmarks: HashMap<u64, Vec<Bookmark>>,
    outbox: HashMap<u64, Sender<Outbound>>,
}

fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(State {
            tabs: HashMap::new(),
            bookmarks: HashMap::new(),
            outbox: HashMap::new(),
        })
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

/// Every paired browser's bar, in connection order. Two browsers connected at
/// once contribute both bars; duplicates across them are left alone, since the
/// same URL bookmarked in two browsers is still two places to go back to.
pub fn bookmarks() -> Vec<Bookmark> {
    let Ok(state) = state().lock() else {
        log_warn!("browser state is poisoned; reporting no bookmarks");
        return Vec::new();
    };
    let mut connections: Vec<&u64> = state.bookmarks.keys().collect();
    connections.sort();
    connections
        .into_iter()
        .flat_map(|connection| state.bookmarks[connection].iter().cloned())
        .collect()
}

/// Fire and forget. bentolaunch has already hidden by the time the switch lands.
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

/// Open a tab in one browser. Same fire and forget as `focus`.
pub fn new_tab(connection: u64) -> bool {
    let Ok(state) = state().lock() else {
        return false;
    };
    let Some(outbox) = state.outbox.get(&connection) else {
        log_warn!("browser connection {connection} has gone; cannot open a tab");
        return false;
    };
    outbox.send(Outbound::NewTab).is_ok()
}

/// Connections with tabs, lowest first, so the same browser answers every time
/// rather than the button moving between them.
pub fn connections() -> Vec<u64> {
    let Ok(state) = state().lock() else {
        return Vec::new();
    };
    let mut open: Vec<u64> = state.outbox.keys().copied().collect();
    open.sort_unstable();
    open
}

/// What the tray reports. The failure that matters is `PortTaken`: it used to
/// be one line in the log, and a silent bridge is exactly what something else
/// holding the port looks like.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Off,
    Listening,
    PortTaken,
}

static STATUS: AtomicU8 = AtomicU8::new(0);
static PORT: AtomicU32 = AtomicU32::new(0);
static STARTED: AtomicBool = AtomicBool::new(false);

fn set_status(status: Status, port: u16) {
    STATUS.store(status as u8, Ordering::Relaxed);
    PORT.store(port as u32, Ordering::Relaxed);
}

pub fn status() -> (Status, u16) {
    let status = match STATUS.load(Ordering::Relaxed) {
        1 => Status::Listening,
        2 => Status::PortTaken,
        _ => Status::Off,
    };
    (status, PORT.load(Ordering::Relaxed) as u16)
}

/// Bind the port, once. Safe to call again - pairing calls it after switching
/// the bridge on, and a second call while already listening does nothing.
///
/// Listening no longer waits for a pairing: an unpaired bridge has to be
/// reachable for pairing to be possible at all. Nothing is admitted without a
/// paired origin or an open pairing window, so an idle listener grants nothing.
pub fn start(hwnd: HWND, config: &crate::config::Browser) -> bool {
    if !config.enabled {
        set_status(Status::Off, config.port);
        return false;
    }
    if STARTED.load(Ordering::Relaxed) {
        return matches!(status().0, Status::Listening);
    }

    migrate_legacy(config);

    // Explicit loopback. The unspecified address would put the tab list on
    // every interface.
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, config.port));
    let listener = match TcpListener::bind(address) {
        Ok(listener) => listener,
        Err(e) => {
            // Deliberately not a fallback port. An extension cannot read a
            // file to find out where bentolaunch went, so a bridge that wanders
            // is a bridge nobody finds - and this failure is also what a
            // process squatting the port looks like, which is worth saying out
            // loud rather than retrying around.
            log_warn!(
                "could not listen on {address} ({e}); another process is using the port. \
                 The browser bridge is off until it is free."
            );
            set_status(Status::PortTaken, config.port);
            return false;
        }
    };

    STARTED.store(true, Ordering::Relaxed);
    set_status(Status::Listening, config.port);
    let hwnd = hwnd.0 as isize;
    std::thread::spawn(move || accept_loop(listener, hwnd));
    log_info!("browser bridge listening on {address}");
    true
}

/// Carry an older install's shared token onto the origins it served, then clear
/// both legacy keys so the config stops holding a secret.
fn migrate_legacy(config: &crate::config::Browser) {
    if config.allow.is_empty() {
        return;
    }
    let legacy = if config.token.is_empty() {
        gate::legacy_token().unwrap_or_default()
    } else {
        config.token.clone()
    };
    if peers::migrate_legacy(&config.allow, &legacy) > 0 {
        crate::pins::clear_browser_legacy();
    }
}

fn accept_loop(listener: TcpListener, hwnd: isize) {
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

        // Claim the slot in the same operation that checks for one: two
        // accepts landing together must not both see room for the last.
        let claimed = LIVE
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |live| {
                (live < MAX_CONNECTIONS).then_some(live + 1)
            })
            .is_ok();
        if !claimed {
            log_warn!("browser bridge at {MAX_CONNECTIONS} connections; dropping this one");
            continue;
        }

        let connection = NEXT.fetch_add(1, Ordering::Relaxed);
        std::thread::spawn(move || {
            if serve(stream, loopback, connection, hwnd) {
                disconnect(connection, hwnd);
            }
            LIVE.fetch_sub(1, Ordering::Relaxed);
        });
    }
}

/// Returns whether this connection ever became a real one, which is what
/// decides if there is any state to tear down.
///
/// The large `Err` clippy objects to is tungstenite's `ErrorResponse`, which is
/// the callback signature `accept_hdr_with_config` requires. Not ours to shrink.
#[allow(clippy::result_large_err)]
fn serve(stream: TcpStream, loopback: bool, connection: u64, hwnd: isize) -> bool {
    if stream.set_read_timeout(Some(READ_TIMEOUT)).is_err() {
        log_warn!("browser connection {connection}: could not set a read timeout");
        return false;
    }

    // RefCell because a failed handshake hands the callback back inside the
    // error, so a `&mut` capture would still be borrowed below.
    let refusal: RefCell<Option<Refusal>> = RefCell::new(None);
    let admitted: RefCell<Option<(Admission, String)>> = RefCell::new(None);
    let handshake = tungstenite::accept_hdr_with_config(
        stream,
        |request: &Request, response: Response| -> Result<Response, ErrorResponse> {
            let origin = request
                .headers()
                .get("Origin")
                .and_then(|value| value.to_str().ok());
            match gate::admit(loopback, origin) {
                Ok(admission) => {
                    *admitted.borrow_mut() =
                        Some((admission, origin.unwrap_or_default().trim().to_owned()));
                    Ok(response)
                }
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
                    log_warn!("browser connection refused: {origin} is not paired");
                    // Extensions only. A page origin here is a site trying its
                    // luck; do not tell anyone how to pair it.
                    if origin.starts_with("chrome-extension://")
                        || origin.starts_with("moz-extension://")
                    {
                        log_info!(
                            "to pair it, choose Browser > Pair a browser... from the tray icon"
                        );
                    }
                }
                Some(reason) => log_warn!("browser connection refused: {}", reason.reason()),
                None => log_info!("browser handshake failed: {e}"),
            }
            return false;
        }
    };

    let Some((admission, origin)) = admitted.borrow_mut().take() else {
        return false;
    };

    let peer = match negotiate(&mut socket, admission, &origin, connection, hwnd) {
        Some(peer) => peer,
        None => {
            let _ = socket.close(None);
            return false;
        }
    };

    let (sender, commands) = channel();
    if let Ok(mut state) = state().lock() {
        state.outbox.insert(connection, sender);
    }
    log_info!("{} connected (connection {connection})", peer.name);

    pump(&mut socket, &commands, connection, hwnd);
    let _ = socket.close(None);
    true
}

/// The half of admission the handshake could not do. Returns the peer only once
/// it has proved itself; everything else closes the socket having registered
/// nothing and learned nothing.
fn negotiate(
    socket: &mut WebSocket<TcpStream>,
    admission: Admission,
    origin: &str,
    connection: u64,
    hwnd: isize,
) -> Option<Peer> {
    let deadline = Instant::now() + NEGOTIATION;

    let Some(Inbound::Hello { v, mode, nonce, proof }) = read_frame(socket, deadline, connection)
    else {
        log_warn!("browser connection {connection}: no usable hello; closing");
        return None;
    };

    if v != PROTOCOL {
        // Named, not just refused. Once the exe and the extension are separate
        // downloads they drift, and "not paired" is the wrong thing to tell
        // someone whose only problem is a stale install.
        let behind = if v < PROTOCOL { "extension" } else { "BentoLaunch" };
        log_warn!(
            "browser connection {connection}: {origin} speaks bridge protocol {v}, \
             this build speaks {PROTOCOL}; the {behind} is out of date"
        );
        send(socket, &Outbound::Outdated { protocol: PROTOCOL }, connection);
        return None;
    }

    match mode.as_str() {
        "pair" => {
            pair(socket, origin, &nonce, &proof, connection, hwnd);
            // Either way this connection is finished: on success the extension
            // now has a token and reconnects to use it, which keeps the paired
            // path the only path that ever carries tabs.
            None
        }
        "resume" => {
            let Admission::Known(peer) = admission else {
                log_warn!("browser connection {connection}: {origin} is not paired");
                return None;
            };
            resume(socket, *peer, &nonce, connection)
        }
        other => {
            // Same version, unknown mode: a build mismatch this side cannot
            // name, so say what was said rather than guessing.
            log_warn!("browser connection {connection}: unknown hello mode {other:?}; closing");
            None
        }
    }
}

/// First contact. The client proves it knows the code the app is showing before
/// bentolaunch says anything, because six digits are guessable from a proof and
/// this side must not be an oracle for them.
fn pair(
    socket: &mut WebSocket<TcpStream>,
    origin: &str,
    nonce_c: &str,
    proof_c: &str,
    connection: u64,
    hwnd: isize,
) {
    let Some(code) = gate::pairing_code() else {
        log_warn!("browser connection {connection}: tried to pair with no window open");
        return;
    };

    let Some(expected) = gate::client_pair_proof(&code, nonce_c) else {
        return;
    };
    if !crypto::equal(&expected, proof_c) {
        // One attempt. A window that stays open after a wrong answer is a
        // window worth guessing at.
        gate::close_pairing();
        log_warn!(
            "browser connection {connection}: wrong pairing code from {origin}; \
             the pairing window is now closed"
        );
        return;
    }

    // The code has been used. Consume it here rather than after the reply,
    // so the window is never open for a moment longer than the single attempt
    // it grants - not across minting a token, a file write, or a socket write
    // that could block.
    gate::close_pairing();

    let (Some(token), Some(proof_s)) = (gate::generate_token(), gate::server_pair_proof(&code, nonce_c))
    else {
        log_warn!("could not generate a token for {origin}; not pairing");
        return;
    };

    let name = peers::name_for(origin);
    let peer = Peer {
        origin: origin.to_owned(),
        name: name.clone(),
        token: token.clone(),
        added: peers::today(),
    };
    if !peers::put(peer) {
        log_warn!("could not record the pairing for {origin}");
        return;
    }

    // The proof is what tells the extension this token came from the app that
    // showed the code, rather than from whatever answered the port.
    send(socket, &Outbound::Paired { token, proof: proof_s }, connection);
    log_info!("paired with {name} ({origin})");
    post(hwnd, WM_PAIRED);
}

/// Every reconnect after pairing. The server proves first so the extension can
/// hang up on an impostor before it has sent a single tab title; the token
/// itself never travels.
fn resume(
    socket: &mut WebSocket<TcpStream>,
    peer: Peer,
    nonce_c: &str,
    connection: u64,
) -> Option<Peer> {
    let nonce_s = crypto::random_hex(16)?;
    let proof_s = gate::server_resume_proof(&peer.token, nonce_c, &nonce_s)?;
    if !send(
        socket,
        &Outbound::Challenge { nonce: nonce_s.clone(), proof: proof_s },
        connection,
    ) {
        return None;
    }

    let deadline = Instant::now() + NEGOTIATION;
    let Some(Inbound::Prove { proof }) = read_frame(socket, deadline, connection) else {
        log_warn!("browser connection {connection}: no proof from {}; closing", peer.origin);
        return None;
    };

    let expected = gate::client_resume_proof(&peer.token, nonce_c, &nonce_s)?;
    if !crypto::equal(&expected, &proof) {
        log_warn!(
            "browser connection {connection}: {} could not prove its token; closing",
            peer.origin
        );
        return None;
    }
    Some(peer)
}

/// One frame, or nothing. Read timeouts are the normal case here - they are how
/// the deadline is enforced.
fn read_frame(
    socket: &mut WebSocket<TcpStream>,
    deadline: Instant,
    connection: u64,
) -> Option<Inbound> {
    while Instant::now() < deadline {
        match socket.read() {
            Ok(Message::Text(text)) => match serde_json::from_str(&text) {
                Ok(message) => return Some(message),
                Err(e) => {
                    log_warn!("browser connection {connection} sent something unreadable: {e}");
                    return None;
                }
            },
            // Nothing else is part of this protocol before admission.
            Ok(Message::Close(_)) => return None,
            Ok(_) => continue,
            Err(tungstenite::Error::Io(e))
                if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(_) => return None,
        }
    }
    None
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
            // Checked before the icons are decoded: a list this size is a bug
            // or an attempt, and neither deserves the work.
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
        Inbound::Bookmarks { bookmarks, icons } => {
            log_info!(
                "browser connection {connection}: {} bookmark(s), {} new icon(s)",
                bookmarks.len(),
                icons.len()
            );
            if bookmarks.len() > MAX_BOOKMARKS {
                log_warn!(
                    "browser connection {connection} sent {} bookmarks; ignoring the list",
                    bookmarks.len()
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
                state.bookmarks.insert(connection, bookmarks);
            }
            true
        }
        Inbound::Pong => false,
        // Admission is over. Repeating it mid-stream is not a thing this
        // protocol does, and re-running it would be a way to change identity
        // on a connection that already has one.
        Inbound::Hello { .. } | Inbound::Prove { .. } => {
            log_warn!("browser connection {connection} tried to negotiate again; ignoring");
            false
        }
    }
}

fn disconnect(connection: u64, hwnd: isize) {
    if let Ok(mut state) = state().lock() {
        state.tabs.remove(&connection);
        state.bookmarks.remove(&connection);
        state.outbox.remove(&connection);
    }
    log_info!("browser disconnected (connection {connection})");
    post(hwnd, WM_TABS_CHANGED);
}

fn post(hwnd: isize, message: u32) {
    // A null window is not "no window" to PostMessageW - it is every top-level
    // window on the desktop. Tests run with no panel, and broadcasting a
    // private WM_APP message to every app on the machine is not a thing to do.
    if hwnd == 0 {
        return;
    }
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
    use tungstenite::client::{IntoClientRequest, client_with_config};

    const ORIGIN: &str = "chrome-extension://abcdefghijklmnopabcdefghijklmnop";

    use gate::test_turn as exclusive;

    /// Proves the gate is wired into the handshake and the exchange after it,
    /// not just correct on its own.
    fn serving() -> u16 {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || accept_loop(listener, 0));
        port
    }

    fn dial(port: u16, origin: Option<&str>) -> Option<WebSocket<TcpStream>> {
        let mut request = format!("ws://127.0.0.1:{port}/").into_client_request().unwrap();
        if let Some(origin) = origin {
            request.headers_mut().insert("Origin", origin.parse().unwrap());
        }
        let stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
        stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
        client_with_config(request, stream, Some(limits()))
            .ok()
            .map(|(socket, _)| socket)
    }

    fn say(socket: &mut WebSocket<TcpStream>, json: String) {
        socket.send(Message::Text(json.into())).unwrap();
    }

    fn hear(socket: &mut WebSocket<TcpStream>) -> Option<serde_json::Value> {
        loop {
            match socket.read().ok()? {
                Message::Text(text) => return serde_json::from_str(&text).ok(),
                Message::Close(_) => return None,
                _ => continue,
            }
        }
    }

    /// Pairing, end to end, the way the extension does it. Leaves the peer
    /// store holding this origin, which the tests below then rely on.
    fn pair_for_tests(port: u16) -> String {
        let code = gate::open_pairing().unwrap();
        let mut socket = dial(port, Some(ORIGIN)).expect("a pairing window admits an unknown origin");
        let nonce = crypto::random_hex(16).unwrap();
        let proof = gate::client_pair_proof(&code, &nonce).unwrap();
        say(
            &mut socket,
            format!(r#"{{"type":"hello","v":{PROTOCOL},"mode":"pair","nonce":"{nonce}","proof":"{proof}"}}"#),
        );
        let reply = hear(&mut socket).expect("pairing must answer");
        assert_eq!(reply["type"], "paired");
        assert_eq!(
            reply["proof"],
            gate::server_pair_proof(&code, &nonce).unwrap(),
            "the app must prove the token came from the app that showed the code"
        );
        assert!(!gate::pairing_open(), "success closes the window");
        reply["token"].as_str().unwrap().to_owned()
    }

    /// Runs the resume exchange and returns the socket once it is admitted.
    fn resume_for_tests(port: u16, token: &str) -> Option<WebSocket<TcpStream>> {
        let mut socket = dial(port, Some(ORIGIN))?;
        let nonce_c = crypto::random_hex(16).unwrap();
        say(
            &mut socket,
            format!(r#"{{"type":"hello","v":{PROTOCOL},"mode":"resume","nonce":"{nonce_c}"}}"#),
        );
        let challenge = hear(&mut socket)?;
        assert_eq!(challenge["type"], "challenge");
        let nonce_s = challenge["nonce"].as_str().unwrap();
        assert_eq!(
            challenge["proof"],
            gate::server_resume_proof(token, &nonce_c, nonce_s).unwrap(),
            "the server must prove itself before the extension sends anything"
        );
        let proof = gate::client_resume_proof(token, &nonce_c, nonce_s).unwrap();
        say(&mut socket, format!(r#"{{"type":"prove","proof":"{proof}"}}"#));
        Some(socket)
    }

    #[test]
    fn a_browser_pairs_then_resumes_and_its_tabs_arrive() {
        let _turn = exclusive();
        let port = serving();
        let token = pair_for_tests(port);
        let mut socket = resume_for_tests(port, &token).expect("a paired browser gets in");

        say(
            &mut socket,
            r#"{"type":"tabs","tabs":[{"id":1,"windowId":1,"title":"Docs","url":"https://d.test/"}]}"#
                .to_string(),
        );
        let mut seen = false;
        for _ in 0..40 {
            if tabs().iter().any(|owned| owned.tab.title == "Docs") {
                seen = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(seen, "a proven connection's tabs must land");
        drop(socket);
        gate::close_pairing();
    }

    #[test]
    fn a_mismatched_version_is_named_rather_than_refused() {
        let _turn = exclusive();
        let port = serving();
        let _token = pair_for_tests(port);

        let mut socket = dial(port, Some(ORIGIN)).expect("a paired origin passes the handshake");
        say(
            &mut socket,
            r#"{"type":"hello","v":0,"mode":"resume","nonce":"aa"}"#.to_string(),
        );
        let reply = hear(&mut socket).expect("a version gap must be explained, not ignored");
        assert_eq!(reply["type"], "outdated");
        assert_eq!(reply["protocol"], PROTOCOL);

        // Explained, and still admitted to nothing.
        assert!(hear(&mut socket).is_none(), "the connection must end there");
        gate::close_pairing();
    }

    #[test]
    fn a_page_origin_never_gets_past_the_handshake() {
        let _turn = exclusive();
        let port = serving();
        assert!(dial(port, Some("https://evil.example")).is_none());
        assert!(dial(port, None).is_none());
    }

    #[test]
    fn the_wrong_code_pairs_nothing_and_burns_the_window() {
        let _turn = exclusive();
        let port = serving();
        gate::open_pairing().unwrap();
        let origin = "chrome-extension://wrongcodewrongcodewrongcode";
        let mut socket = dial(port, Some(origin)).expect("the window admits it");
        let nonce = crypto::random_hex(16).unwrap();
        let proof = gate::client_pair_proof("000000", &nonce).unwrap();
        say(
            &mut socket,
            format!(r#"{{"type":"hello","v":{PROTOCOL},"mode":"pair","nonce":"{nonce}","proof":"{proof}"}}"#),
        );

        assert!(hear(&mut socket).is_none(), "a wrong code is told nothing");
        assert!(!gate::pairing_open(), "one wrong answer closes the window");
        assert!(peers::find(origin).is_none(), "nothing may be recorded");
    }

    #[test]
    fn a_paired_browser_that_cannot_prove_its_token_is_dropped() {
        let _turn = exclusive();
        let port = serving();
        let _token = pair_for_tests(port);

        let mut socket = dial(port, Some(ORIGIN)).expect("a paired origin passes the handshake");
        let nonce_c = crypto::random_hex(16).unwrap();
        say(
            &mut socket,
            format!(r#"{{"type":"hello","v":{PROTOCOL},"mode":"resume","nonce":"{nonce_c}"}}"#),
        );
        assert_eq!(hear(&mut socket).unwrap()["type"], "challenge");
        say(&mut socket, r#"{"type":"prove","proof":"nope"}"#.to_string());

        // Nothing it says afterwards can register anything.
        let before = tabs().len();
        let _ = socket.send(Message::Text(
            r#"{"type":"tabs","tabs":[{"id":9,"windowId":9,"title":"Sneaked"}]}"#.into(),
        ));
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(tabs().len(), before, "an unproven caller must register nothing");
        assert!(!tabs().iter().any(|o| o.tab.title == "Sneaked"));
        gate::close_pairing();
    }

    #[test]
    fn silence_after_the_handshake_does_not_hold_a_slot_forever() {
        let _turn = exclusive();
        let port = serving();
        let _token = pair_for_tests(port);

        let mut socket = dial(port, Some(ORIGIN)).expect("the handshake passes");
        // Says nothing at all. The negotiation deadline is what ends it.
        //
        // Asserted by watching this socket close rather than by watching the
        // live-connection counter: that counter is one per process and other
        // tests are still draining into it, which is a race, not a signal.
        let _ = socket
            .get_ref()
            .set_read_timeout(Some(Duration::from_millis(100)));

        let deadline = Instant::now() + NEGOTIATION + Duration::from_secs(5);
        let mut dropped = false;
        while Instant::now() < deadline {
            match socket.read() {
                Ok(Message::Close(_)) => {
                    dropped = true;
                    break;
                }
                Ok(_) => panic!("an unproven caller must not be sent anything"),
                Err(tungstenite::Error::Io(e))
                    if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                // Any other error is the connection ending, which is the point.
                Err(_) => {
                    dropped = true;
                    break;
                }
            }
        }
        assert!(dropped, "a caller that never proves itself must be dropped");
        gate::close_pairing();
    }

    #[test]
    fn the_connection_cap_holds_and_recovers() {
        let _turn = exclusive();
        let port = serving();
        let token = pair_for_tests(port);

        let held: Vec<_> = (0..MAX_CONNECTIONS)
            .filter_map(|_| resume_for_tests(port, &token))
            .collect();
        assert_eq!(held.len(), MAX_CONNECTIONS, "the cap must admit this many");
        assert!(
            dial(port, Some(ORIGIN)).is_none_or(|mut s| hear(&mut s).is_none()),
            "one past the cap must get nowhere"
        );

        drop(held);
        let mut recovered = false;
        for _ in 0..40 {
            if resume_for_tests(port, &token).is_some() {
                recovered = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(recovered, "slots must come back when connections close");
        gate::close_pairing();
    }
}
