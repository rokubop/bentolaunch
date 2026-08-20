// Dials bentopick, streams the tab list, switches tabs on request.
//
// Connects out rather than being connected to: an MV3 worker is killed when
// idle, and socket traffic keeps it alive. bentopick's 20s ping is what holds it.
//
// Whatever answers 127.0.0.1 is not automatically bentopick. So nothing is sent
// until the far end has proved it holds this browser's token: the tab list goes
// out after the handshake in `proveTheServer`, never before.

importScripts("bridge.js");

const RECONNECT_MIN_MS = 1000;
const RECONNECT_MAX_MS = 30000;
const TAB_DEBOUNCE_MS = 250;

const ICON_PX = 32;

let socket = null;
let backoff = RECONNECT_MIN_MS;
let debounce = null;
// Set once the far end has proved itself. Nothing is sent while it is false.
let proven = false;
let nonceClient = null;
// Decoded favicons by origin, and which of them this connection has sent.
const iconCache = new Map();
let iconsSent = new Set();

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
  // Not paired yet. Stay quiet rather than hammer a socket that will refuse us.
  if (!token) return;

  proven = false;
  nonceClient = randomHex(16);
  socket = new WebSocket(`ws://127.0.0.1:${port}/`);
  socket.onopen = () => {
    backoff = RECONNECT_MIN_MS;
    // A new bentopick process knows none of them.
    iconsSent = new Set();
    // Opens the exchange and says nothing else. The token stays here.
    raw({ type: "hello", v: BRIDGE_PROTOCOL, mode: "resume", nonce: nonceClient });
  };
  socket.onmessage = (event) => receive(event.data);
  socket.onclose = () => {
    socket = null;
    proven = false;
    retry();
  };
  socket.onerror = () => {};
}

function retry() {
  setTimeout(connect, backoff);
  backoff = Math.min(backoff * 2, RECONNECT_MAX_MS);
}

// Before the far end has proved itself, `raw` is the only way to write to the
// socket, and the only thing it carries is this browser's half of the proof.
function raw(message) {
  if (!live()) return;
  try {
    socket.send(JSON.stringify(message));
  } catch (e) {
    // onclose reconnects.
  }
}

function send(message) {
  if (!proven) return;
  raw(message);
}

// Favicons are per-site, so one bitmap serves every tab on the same origin.
function originOf(url) {
  try {
    return new URL(url).origin;
  } catch (e) {
    return null;
  }
}

// Decoded here rather than in bentopick: a service worker already has an image
// decoder, and shipping raw pixels keeps bentopick free of one.
async function decodeIcon(pageUrl) {
  const url = new URL(chrome.runtime.getURL("/_favicon/"));
  url.searchParams.set("pageUrl", pageUrl);
  url.searchParams.set("size", String(ICON_PX));

  const response = await fetch(url.toString());
  if (!response.ok) return null;
  const bitmap = await createImageBitmap(await response.blob());

  const canvas = new OffscreenCanvas(ICON_PX, ICON_PX);
  const ctx = canvas.getContext("2d", { willReadFrequently: true });
  ctx.clearRect(0, 0, ICON_PX, ICON_PX);
  ctx.drawImage(bitmap, 0, 0, ICON_PX, ICON_PX);
  bitmap.close();

  const { data } = ctx.getImageData(0, 0, ICON_PX, ICON_PX);
  let binary = "";
  for (let i = 0; i < data.length; i += 1) binary += String.fromCharCode(data[i]);
  return { w: ICON_PX, h: ICON_PX, rgba: btoa(binary) };
}

async function iconFor(pageUrl) {
  const origin = originOf(pageUrl);
  if (!origin) return null;
  if (!iconCache.has(origin)) {
    try {
      iconCache.set(origin, await decodeIcon(pageUrl));
    } catch (e) {
      iconCache.set(origin, null);
    }
  }
  return iconCache.get(origin) ? origin : null;
}

async function sendTabs() {
  if (!proven || !live()) return;
  const tabs = await chrome.tabs.query({});
  const keys = await Promise.all(tabs.map((tab) => iconFor(tab.url || "")));

  // Only what bentopick has not been sent on this connection. It keeps them.
  const icons = {};
  keys.forEach((key) => {
    if (key && !iconsSent.has(key)) {
      icons[key] = iconCache.get(key);
      iconsSent.add(key);
    }
  });

  send({
    type: "tabs",
    tabs: tabs.map((tab, i) => ({
      id: tab.id,
      windowId: tab.windowId,
      title: tab.title || "",
      url: tab.url || "",
      active: !!tab.active,
      icon: keys[i],
    })),
    icons,
  });
}

// Tab events arrive in bursts.
function scheduleTabs() {
  if (debounce) clearTimeout(debounce);
  debounce = setTimeout(() => {
    debounce = null;
    sendTabs();
  }, TAB_DEBOUNCE_MS);
}

// bentopick proves itself first, so a wrong answer here costs nothing: the
// socket closes with not one tab title having crossed it.
//
// The token is not cleared on a failure. Something else holding the port would
// otherwise be able to unpair this browser just by answering badly.
async function proveTheServer(message) {
  const { token } = await settings();
  const expected = await bridgeProof("resume-server", token, nonceClient, message.nonce);
  if (message.proof !== expected) {
    console.warn("bentopick: whatever answered the port could not prove itself; not sending tabs");
    if (socket) socket.close();
    return;
  }

  raw({
    type: "prove",
    proof: await bridgeProof("resume-client", token, nonceClient, message.nonce),
  });
  proven = true;
  sendTabs();
}

function receive(data) {
  let message;
  try {
    message = JSON.parse(data);
  } catch (e) {
    return;
  }

  if (message.type === "outdated") {
    // Not a pairing problem, and it must not look like one. The reconnect
    // backoff still applies, so this settles into one line every 30 seconds
    // rather than a stream.
    console.warn(
      `bentopick: BentoPick speaks bridge protocol ${message.protocol}, this extension speaks ` +
        `${BRIDGE_PROTOCOL}. Update ${outdatedSide(message.protocol)}.`,
    );
    return;
  }

  if (message.type === "challenge") {
    proveTheServer(message);
    return;
  }

  // Everything below acts on this browser, so none of it runs for a caller
  // that has not proved itself.
  if (!proven) return;

  if (message.type === "ping") {
    send({ type: "pong" });
    return;
  }

  if (message.type === "focus") {
    // The switch needs no foreground rights. Raising the window does, and
    // bentopick grants them with AllowSetForegroundWindow before asking.
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

// The worker still gets killed eventually. The alarm wakes it back up.
chrome.alarms.create("bentopick-reconnect", { periodInMinutes: 1 });
chrome.alarms.onAlarm.addListener(connect);
chrome.runtime.onStartup.addListener(connect);
chrome.runtime.onInstalled.addListener(connect);
// Pairing writes the token from the options page; this is what picks it up.
chrome.storage.onChanged.addListener(() => {
  if (socket) socket.close();
  connect();
});

connect();
