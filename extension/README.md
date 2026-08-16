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

Three steps, because the socket refuses everything until both halves are set.

1. In `flick.toml`, set `enabled = true` under `[browser]`. Restart flick.
   It generates `token` and writes it back to the same file.
2. Load this folder: `chrome://extensions`, Developer mode on, Load unpacked.
   Open its options page, paste the token, Save.
3. The extension connects and is refused. flick logs the origin it saw:

   ```
   WARN  browser connection refused: origin chrome-extension://abc... is not paired
   INFO  to pair it, add "chrome-extension://abc..." to browser.allow in flick.toml
   ```

   Paste that into `browser.allow`. Restart flick.

Log lives at `%LOCALAPPDATA%\flick\flick.log`.

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
