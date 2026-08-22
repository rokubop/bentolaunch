# BentoPick bridge

Sends open tabs and the bookmarks bar to BentoPick. Switches to a tab when
BentoPick asks.

Chromium only for now: Chrome, Edge, Brave, Vivaldi. Firefox needs a separate
build.

## Why an extension

No OS API exposes browser tabs. The alternatives are closed:

- UI Automation on the tab strip: titles but no URLs, and it turns on Chrome's
  accessibility mode.
- DevTools port 9222: needs Chrome launched with a flag.
- The profile's `Bookmarks` / `places.sqlite` files: locked while running, and
  writing near them breaks safety rule 3. Bookmarks come through this extension
  for the same reason tabs do.

## What it sends

- Every open tab: title, URL, favicon.
- The **bookmarks bar**, one level deep. Not "Other bookmarks", and not the
  folders on the bar. Read-only - nothing is ever written back.

## Pairing

Two clicks and six digits. Nothing is copied out of a config file, and no
restart is involved.

1. Load this folder: `chrome://extensions`, Developer mode on, Load unpacked.
2. Right-click the BentoPick tray icon: **Browser > Pair a browser...**. It
   turns the bridge on if it was off and shows a six-digit code.
3. Open this extension's options page, type the code, **Pair with BentoPick**.

The code is good for one attempt and lives only as long as that dialog is on
screen. A wrong code closes the window at BentoPick's end, so a second try means
asking it for a new code.

```
INFO  pairing window open
INFO  paired with Chrome (chrome-extension://abc...)
INFO  Chrome connected (connection 2)
INFO  browser connection 2: 37 tab(s)
```

Log lives at `%LOCALAPPDATA%\bentopick\bentopick.log`. An unpaired browser says
so, and says where the button is:

```
WARN  browser connection refused: chrome-extension://abc... is not paired
INFO  to pair it, choose Browser > Pair a browser... from the tray icon
```

**Unpairing** is **Browser > Forget** in the same menu. That drops the token at
BentoPick's end, which is the end that matters; the button on the options page
only clears this side.

The exe and this extension are separate downloads, so they can drift. Each
`hello` names the bridge protocol it speaks, and a mismatch says which half is
behind - in the log, and on the options page while pairing - rather than looking
like a pairing failure:

```
WARN  browser connection 3: chrome-extension://abc... speaks bridge protocol 1,
      this build speaks 2; the extension is out of date
```

Both halves ship from the same GitHub release, so taking the exe and the
extension from one download keeps them in step.

Upgrading from a build that used `browser.allow` and a shared token: the
origins in `allow` are carried into the peer store on first run and both legacy
keys are blanked. You do not re-pair, but you do have to reload the extension,
since the old one cannot speak the current handshake.

## Add a tab section

```toml
[[sections]]
title = "Tabs"
source = "tabs"
```

Empty until the extension connects, and empty sections do not render.

## What it can see

`tabs`: the title and URL of every open tab.
`favicon`: site icons, from Chrome's own cache.

Both go to BentoPick over loopback. Nothing leaves the machine. No host permissions,
no content scripts, no network access beyond `127.0.0.1` and `_favicon`.

Favicons are decoded here, not in BentoPick: a service worker already has an image
decoder, so BentoPick needs none. One bitmap per origin, sent once per connection.

## Security

Loopback only, and a caller gets nothing until two separate things hold.

- **`Origin` is a paired browser.** Any web page can script a WebSocket to
  localhost, but a browser stamps its own origin on the handshake and page
  JavaScript cannot forge it. This is what stops a site enumerating your tabs.
- **It proves it knows that browser's token.** A non-browser process can claim
  any origin it likes; it cannot guess 48 hex characters from the OS CSPRNG.
  The token itself never travels - both ends hash it against fresh nonces - so
  it is not in a URL, a devtools panel or a `chrome://net-export` capture.

That proof runs in **both directions**, and the second direction is not about
BentoPick's safety at all. Whatever holds port 8777 is what this extension would
otherwise believe BentoPick to be. Something that grabbed the port first would
be handed the token and every tab title and URL, continuously. So BentoPick
proves itself first on every reconnect, and this extension sends nothing at all
until that checks out - see `proveTheServer` in `worker.js`.

The same reasoning is why pairing is refused when the port is taken: if
BentoPick is not the one listening, there is no safe way to hand out a code, so
the tray says the port is in use instead.

One token per browser, in `%LOCALAPPDATA%\bentopick\peers.json`, which Windows
restricts to your account. Forgetting Chrome does not unpair Firefox. Anything
running as you can read that file - it stops other accounts and blind attempts,
not code running as you. The origin check is what stops the threat that actually
exists.

Caps past the gate: 4 MiB messages, 8 live connections, 2000 tabs, and five
seconds to finish proving yourself before the connection is dropped.
