# flick bridge

Sends open tabs to flick. Switches to one when flick asks.

Chromium only for now: Chrome, Edge, Brave, Vivaldi. Firefox needs a separate
build.

## Why an extension

No OS API exposes browser tabs. The alternatives are closed:

- UI Automation on the tab strip: titles but no URLs, and it turns on Chrome's
  accessibility mode.
- DevTools port 9222: needs Chrome launched with a flag.
- The profile's `Bookmarks` / `places.sqlite` files: locked while running, and
  writing near them breaks safety rule 3.

## Pairing

The socket stays shut until both halves are set, so this is a round trip.

1. In `flick.toml`, set `enabled = true` under `[browser]`. Restart flick.
   It generates `token` and writes it back to the same file.
2. Load this folder: `chrome://extensions`, Developer mode on, Load unpacked.
3. Open its options page. Paste the token in, Save. Copy the origin it shows.
4. Put that origin in `browser.allow`. Restart flick.

flick only listens once `allow` is non-empty, so nothing connects before step 4.

```
INFO  browser bridge listening on 127.0.0.1:8777
INFO  browser connected (connection 1)
INFO  browser connection 1: 37 tab(s)
```

Log lives at `%LOCALAPPDATA%\flick\flick.log`. A wrong origin says so:

```
WARN  browser connection refused: origin chrome-extension://abc... is not paired
```

## Add a tab section

```toml
[[sections]]
title = "Tabs"
source = "tabs"
```

Empty until the extension connects, and empty sections do not render.

## What it can see

`tabs` permission: the title and URL of every open tab, sent to flick over
loopback. Nothing leaves the machine. No host permissions, no content scripts,
no network access beyond `127.0.0.1`.

## Security

The socket is loopback-only and refuses a connection unless both hold:

- `Origin` is in `browser.allow`. Any web page can script a WebSocket to
  localhost, but a browser stamps its own origin on the handshake and page
  JavaScript cannot forge it. This is what stops a site enumerating your tabs.
- The token matches. A non-browser process can claim any origin it likes; it
  cannot guess a 48-hex-character secret from the OS CSPRNG.

Neither stops code already running as you with your files in reach. That code
has better targets than this socket.
