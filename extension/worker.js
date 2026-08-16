// Dials flick, streams the tab list, switches tabs on request.
//
// The extension connects out rather than flick connecting in: an MV3 service
// worker is killed when idle, and socket traffic is what keeps it alive.
// flick pings every 20s, which is what actually holds this worker open.

const RECONNECT_MIN_MS = 1000;
const RECONNECT_MAX_MS = 30000;
const TAB_DEBOUNCE_MS = 250;

let socket = null;
let backoff = RECONNECT_MIN_MS;
let debounce = null;

async function settings() {
  const stored = await chrome.storage.local.get(["port", "token"]);
  return { port: stored.port || 8777, token: stored.token || "" };
}

function live() {
  return socket && socket.readyState === WebSocket.OPEN;
}

async function connect() {
  if (socket && (socket.readyState === WebSocket.OPEN || socket.readyState === WebSocket.CONNECTING)) {
    return;
  }
  const { port, token } = await settings();
  // Unpaired. Stay quiet rather than hammering a socket that will refuse us.
  if (!token) return;

  socket = new WebSocket(`ws://127.0.0.1:${port}/${encodeURIComponent(token)}`);
  socket.onopen = () => {
    backoff = RECONNECT_MIN_MS;
    sendTabs();
  };
  socket.onmessage = (event) => receive(event.data);
  socket.onclose = () => {
    socket = null;
    retry();
  };
  // A refused handshake surfaces here; onclose follows and handles the retry.
  socket.onerror = () => {};
}

function retry() {
  setTimeout(connect, backoff);
  backoff = Math.min(backoff * 2, RECONNECT_MAX_MS);
}

function send(message) {
  if (!live()) return;
  try {
    socket.send(JSON.stringify(message));
  } catch (e) {
    // Nothing to do about it here; onclose will reconnect.
  }
}

async function sendTabs() {
  if (!live()) return;
  const tabs = await chrome.tabs.query({});
  send({
    type: "tabs",
    tabs: tabs.map((tab) => ({
      id: tab.id,
      windowId: tab.windowId,
      title: tab.title || "",
      url: tab.url || "",
      active: !!tab.active,
    })),
  });
}

// Tab events arrive in bursts, so coalesce them into one list.
function scheduleTabs() {
  if (debounce) clearTimeout(debounce);
  debounce = setTimeout(() => {
    debounce = null;
    sendTabs();
  }, TAB_DEBOUNCE_MS);
}

function receive(data) {
  let message;
  try {
    message = JSON.parse(data);
  } catch (e) {
    return;
  }

  if (message.type === "ping") {
    send({ type: "pong" });
    return;
  }

  if (message.type === "focus") {
    // The tab switch needs no foreground rights; raising the window does, and
    // flick hands those over with AllowSetForegroundWindow before asking.
    chrome.tabs.update(message.tabId, { active: true });
    chrome.windows.update(message.windowId, { focused: true });
  }
}

for (const event of [
  chrome.tabs.onCreated,
  chrome.tabs.onRemoved,
  chrome.tabs.onUpdated,
  chrome.tabs.onActivated,
  chrome.tabs.onMoved,
  chrome.tabs.onReplaced,
  chrome.windows.onRemoved,
]) {
  event.addListener(scheduleTabs);
}

// The worker still gets killed eventually. An alarm wakes it back up and the
// connection is re-established from scratch.
chrome.alarms.create("flick-reconnect", { periodInMinutes: 1 });
chrome.alarms.onAlarm.addListener(connect);
chrome.runtime.onStartup.addListener(connect);
chrome.runtime.onInstalled.addListener(connect);
chrome.storage.onChanged.addListener(() => {
  if (socket) socket.close();
  connect();
});

connect();
