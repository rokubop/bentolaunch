// Dials bentolaunch, streams the tab list, switches tabs on request.
//
// Connects out rather than being connected to: an MV3 worker is killed when
// idle, and socket traffic keeps it alive. bentolaunch's 20s ping is what holds it.
//
// Whatever answers 127.0.0.1 is not automatically bentolaunch. So nothing is sent
// until the far end has proved it holds this browser's token: the tab list goes
// out after the handshake in `proveTheServer`, never before.

importScripts("bridge.js");

// Printed on every worker start, because Chrome will happily keep a registered
// service worker across a reload of an unpacked extension: the extension
// reloads, the script does not, and the new message type is dropped by old code
// that is still answering pings. If this line does not say what the manifest
// says, remove the extension and add it again.
console.log(`bentolaunch bridge ${chrome.runtime.getManifest().version}, protocol ${BRIDGE_PROTOCOL}`);

const RECONNECT_MIN_MS = 1000;
const RECONNECT_MAX_MS = 30000;
const TAB_DEBOUNCE_MS = 250;
const BOOKMARK_DEBOUNCE_MS = 500;

const ICON_PX = 32;

let socket = null;
let backoff = RECONNECT_MIN_MS;
let debounce = null;
let bookmarkDebounce = null;
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
    // Backoff is not reset here. The socket opening proves nothing: a refusal
    // for a bad token or a stale protocol arrives after it, and resetting on
    // open turns that into a reconnect once a second forever.
    //
    // A new bentolaunch process knows none of them.
    iconsSent = new Set();
    // Opens the exchange and says nothing else. The token stays here.
    raw({
      type: "hello",
      v: BRIDGE_PROTOCOL,
      ext: chrome.runtime.getManifest().version,
      mode: "resume",
      nonce: nonceClient,
    });
  };
  socket.onmessage = (event) => receive(event.data);
  socket.onclose = () => {
    socket = null;
    proven = false;
    retry();
  };
  socket.onerror = () => {};
}

// One pending retry at a time. `connect` is called by the close handler and by
// a one-minute alarm, and without this each becomes its own reconnect chain:
// two sockets a second, neither aware of the other.
let pendingRetry = null;

function retry() {
  if (pendingRetry) return;
  pendingRetry = setTimeout(() => {
    pendingRetry = null;
    connect();
  }, backoff);
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

// Decoded here rather than in bentolaunch: a service worker already has an image
// decoder, and shipping raw pixels keeps bentolaunch free of one.
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

  // Only what bentolaunch has not been sent on this connection. It keeps them.
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

// Only the bookmarks bar, not the whole tree. The bar is the row someone
// already curated; "Other bookmarks" is an archive of thousands and would bury
// the panel it is pasted into.
//
// Chrome numbers the bar "1". Anything else falls back to the first root folder
// that has children, which is where Firefox's toolbar lands.
async function barFolder() {
  const roots = await chrome.bookmarks.getTree();
  const children = (roots[0] && roots[0].children) || [];
  return children.find((node) => node.id === "1") || children.find((node) => node.children);
}

// One level deep. A folder on the bar stays a folder: bentolaunch has no way to
// open one, and flattening it would spill a nested archive onto the panel.
async function sendBookmarks() {
  if (!proven || !live()) return;
  const bar = await barFolder();
  const entries = ((bar && bar.children) || []).filter((node) => node.url);
  const keys = await Promise.all(entries.map((node) => iconFor(node.url)));

  const icons = {};
  keys.forEach((key) => {
    if (key && !iconsSent.has(key)) {
      icons[key] = iconCache.get(key);
      iconsSent.add(key);
    }
  });

  send({
    type: "bookmarks",
    bookmarks: entries.map((node, i) => ({
      id: node.id,
      title: node.title || "",
      url: node.url,
      icon: keys[i],
    })),
    icons,
  });
}

function scheduleBookmarks() {
  if (bookmarkDebounce) clearTimeout(bookmarkDebounce);
  bookmarkDebounce = setTimeout(() => {
    bookmarkDebounce = null;
    sendBookmarks();
  }, BOOKMARK_DEBOUNCE_MS);
}

// The whole tree, flattened, each entry carrying the folder path it is filed
// under. Only ever in answer to a request: the bar is what the panel shows all
// the time, and an archive of thousands is not worth sending until somebody
// opens it.
//
// No icons. Fetching a favicon for every distinct site in an archive is minutes
// of work and megabytes of socket; BentoLaunch files favicons by origin, so
// anything sharing a site with an open tab or a bar entry already has one there.
const MAX_TREE = 5000;

async function sendTree() {
  if (!proven || !live()) return;
  const roots = await chrome.bookmarks.getTree();
  const bar = await barFolder();
  const out = [];

  const walk = (node, path) => {
    for (const child of node.children || []) {
      if (child.url) {
        out.push({ id: child.id, title: child.title || "", url: child.url, folder: path });
      } else {
        walk(child, path ? `${path} / ${child.title}` : child.title || "");
      }
    }
  };

  // The bar is where most bookmarks are, and naming it on nine tiles in ten
  // spends the line saying "the usual place". It starts at nothing; every other
  // root keeps its name, because "Other bookmarks" is the part worth knowing.
  for (const root of (roots[0] && roots[0].children) || []) {
    walk(root, bar && root.id === bar.id ? "" : root.title || "");
  }

  if (out.length > MAX_TREE) {
    console.warn(`bentolaunch: ${out.length} bookmarks; sending the first ${MAX_TREE}`);
    out.length = MAX_TREE;
  }
  send({ type: "tree", bookmarks: out });
}

// Tab events arrive in bursts.
function scheduleTabs() {
  if (debounce) clearTimeout(debounce);
  debounce = setTimeout(() => {
    debounce = null;
    sendTabs();
  }, TAB_DEBOUNCE_MS);
}

// bentolaunch proves itself first, so a wrong answer here costs nothing: the
// socket closes with not one tab title having crossed it.
//
// The token is not cleared on a failure. Something else holding the port would
// otherwise be able to unpair this browser just by answering badly.
async function proveTheServer(message) {
  const { token } = await settings();
  const expected = await bridgeProof("resume-server", token, nonceClient, message.nonce);
  if (message.proof !== expected) {
    console.warn("bentolaunch: whatever answered the port could not prove itself; not sending tabs");
    if (socket) socket.close();
    return;
  }

  raw({
    type: "prove",
    proof: await bridgeProof("resume-client", token, nonceClient, message.nonce),
  });
  proven = true;
  backoff = RECONNECT_MIN_MS;
  sendTabs();
  sendBookmarks();
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
      `bentolaunch: BentoLaunch speaks bridge protocol ${message.protocol}, this extension speaks ` +
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
    // bentolaunch grants them with AllowSetForegroundWindow before asking.
    chrome.tabs.update(message.tabId, { active: true });
    chrome.windows.update(message.windowId, { focused: true });
    return;
  }

  if (message.type === "wanttree") {
    sendTree();
    return;
  }

  if (message.type === "newtab") {
    // No window id: the last focused one is where you were, which is where a
    // new tab belongs. Raised the same way a focus is.
    chrome.tabs.create({}, (tab) => {
      if (tab) chrome.windows.update(tab.windowId, { focused: true });
    });
  }
}

for (const event of [
  chrome.bookmarks.onCreated,
  chrome.bookmarks.onRemoved,
  chrome.bookmarks.onChanged,
  chrome.bookmarks.onMoved,
]) {
  event.addListener(scheduleBookmarks);
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
chrome.alarms.create("bentolaunch-reconnect", { periodInMinutes: 1 });
chrome.alarms.onAlarm.addListener(connect);
chrome.runtime.onStartup.addListener(connect);
chrome.runtime.onInstalled.addListener(connect);
// Pairing writes the token from the options page; this is what picks it up.
chrome.storage.onChanged.addListener(() => {
  if (socket) socket.close();
  connect();
});

connect();
