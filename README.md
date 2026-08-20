<img src="assets/bentopick-256.png" width="88" alt="">

# BentoPick

A hotkey-summoned grid of everything worth switching to: running windows,
browser tabs, taskbar pins, folders, apps, settings pages, links.

Press `` Alt+` ``. Type to narrow. Enter takes the top match, or click a tile.
Esc closes and puts you back where you were.

Windows 11 only. Never asks for admin, and writes nothing outside its own
config and `%LOCALAPPDATA%\bentopick`.

### What it leaves on your machine

| Item | Reversal |
|---|---|
| Autostart shortcut (optional) | Delete it from the Startup folder |
| Browser extension, per profile | Uninstall it in the browser |

Nothing else changes. No installer, no registry keys, no elevation, ever.

Early days. It does what this README says, but it has only ever run on one
machine, and most of it has never been verified anywhere else.

## Running

```powershell
cargo build
target\debug\bentopick.exe
```

Starts silent, tray icon only. Only one runs at a time: launch it again, from a
taskbar pin or anywhere else, and the panel comes up instead of a second copy.

Right-click the tray icon:

| Item | Does |
|---|---|
| Show BentoPick | Same as the hotkey |
| Add app… | Browse installed apps, Store apps included, and pin one |
| Add folder… | Pin a folder |
| Add file or shortcut… | Pin a file or `.lnk` |
| Edit settings… | Open `bentopick.toml` in your editor |
| Exit | Quit |

Log: `%LOCALAPPDATA%\bentopick\bentopick.log`

## Finding

Just type. The grid narrows on every character, and a strip at the top shows
what you typed and how much survived it.

| Key | Does |
|---|---|
| Any letter | Narrow the grid |
| Arrows | Move the selection |
| Enter | Take the selected tile |
| Home / End | First tile, last tile |
| Esc | Clear the query, then close the panel |

Every word has to match, so typing more always narrows. Both the title and the
second line are searched, which is how `github` finds a tab whose title never
says so.

Filtering hides tiles, it never reorders them. Tile positions are what make the
grid learnable, and the panel keeps its width while you type so it cannot slide
sideways under you.

## Arranging

There is no edit mode. Same as the taskbar or the bookmarks bar:

| Do | Get |
|---|---|
| Click a tile | Switch to it, or launch it |
| Drag a pinned tile | Reorder it inside its section |
| Right-click a running window | **Pin this app** |
| Right-click a pin | **Unpin**, **Open file location** |
| Right-click anywhere | Add app/folder/file, settings |

Click and drag never get confused: under the system's drag threshold is a click,
past it is a drag.

Only pinned tiles move. Running windows stay in most-recent order.

Every change goes straight into `bentopick.toml`. Nothing is remembered anywhere
else, and all of it can be undone by hand. Taskbar order is saved as an `order`
list, since Windows does not expose its own.

The panel closes the moment it loses focus.

## Config

`bentopick.toml`, next to the exe. Written with defaults on first run.

**No restart needed.** BentoPick watches the file and reloads on save, hotkey
included. Pins added from the tray are written here, and hand-written comments
and formatting are preserved.

```toml
hotkey = "alt+`"     # ctrl, alt, shift, win + a key
dry_run = false      # true: log what a click would do, do nothing
```

### Sections

Order here is order on screen. Empty sections do not render.

Running things are listed before launchable ones, because switching to something
that exists beats starting something new. Out of the box that is three headers:
`Browsing` (browser windows and tabs), `Active` (every other window), and
`Launch` (taskbar pins and anything you pin yourself).

```toml
[[sections]]
title  = "Browsing"
source = "windows"
match  = ["chrome.exe", "msedge.exe", "firefox.exe"]

[[sections]]
title  = "Files"
source = "windows"
match  = ["explorer.exe"]

[[sections]]
title  = "Active"
source = "windows"   # no match: everything not claimed above

[[sections]]
title  = "Launch"
source = "taskbar"   # apps pinned to your Windows taskbar
order  = []          # pin names, in order; written by dragging a tile

[[sections]]
title  = "Places"
source = "manual"
items = [
    'R:\dev',
    { title = "Display", target = "ms-settings:display" },
    { title = "Wikipedia", target = "https://wikipedia.org" },
]
```

`match` lists process names, case-insensitive, and only applies to
`source = "windows"`. Sections claim windows in order and each window is claimed
once, so put filtered sections above the unfiltered catch-all. Keep exactly one
windows section without a `match`, or windows from an unlisted app have nowhere
to go.

Use `'single quotes'` for Windows paths. Inside `"double quotes"` TOML reads `\`
as an escape, so `"R:\dev"` is a parse error.

A manual `target` is anything the shell can open:

| Target | Example |
|---|---|
| Folder | `'R:\dev'` |
| App or file | `'C:\Windows\notepad.exe'` |
| Shortcut | `'C:\...\Thing.lnk'` |
| Store app | `'shell:AppsFolder\<AppUserModelID>'` |
| Settings page | `"ms-settings:display"` |
| Link | `"https://example.com"` |

Bare strings get their title from the path. Use the `{ title, target }` form to
choose one.

### Browser tabs

Off by default. Windows has no API for browser tabs, so this needs an extension:
`extension/`, Chromium only for now.

```toml
[[sections]]
title  = "Browsing"  # the default: windows first, then tabs
source = [
    { source = "windows", match = ["chrome.exe", "msedge.exe", "firefox.exe"] },
    "tabs",          # empty until the extension connects
]

[browser]
enabled = true
port    = 8777
allow   = ["chrome-extension://<id from the options page>"]
```

`extension/README.md` has the pairing steps. Tabs sit under the same header as
your browser windows, right behind them, since both answer the same question.

**Read this before turning it on.** It opens a port on your machine that only
your own computer can reach, and it installs an extension that can read the
title and URL of every tab you have open. Both are the feature working as
intended, and both are your call.

What guards that port:

- It only opens if you set `enabled = true` *and* list an extension in `allow`.
- Websites cannot get in. Any page can try to open a connection to your own
  machine, but browsers stamp every connection with who is making it and pages
  cannot fake that stamp. Only the extension you listed is let through.
- A secret token, generated on first run, kept in
  `%LOCALAPPDATA%\bentopick\bridge-token`, which Windows restricts to your account.

What it does not guard against: software already running under your own account.
That software can read the token, but it can also read your browser profile
directly, so this is not the interesting way in. `src/browser/gate.rs` has the
full reasoning at the top of the file.

### Appearance

Tile size is fixed. It never changes with item count, which is what makes tile
positions learnable. The panel grows outward from center until it hits
`max_screen_fraction` of the monitor, then scrolls.

Defaults fit about 60 tiles on a 1080p monitor. Raise `tile_width` and
`tile_height` if you want fewer, larger ones.

```toml
[grid]
tile_width = 140.0
tile_height = 100.0
gap = 10.0
padding = 18.0
max_screen_fraction = 0.8
max_columns = 9          # hard column cap; 0 means whatever fits
label_height = 24.0
show_detail = false      # true: second line with process name or path
header_height = 22.0     # 0 hides section headers
header_gap    = 6.0      # between a section title and its first row
section_gap = 10.0
corner_radius = 8.0

[theme]
panel = "#F01A1A1E"      # #AARRGGBB or #RRGGBB
tile = "#FF2A2A32"
tile_hover = "#FF3C3C48"
text = "#FFE8E8EC"
header = "#FF9A9AA8"
tile_drag = "#FF4A4460"    # a tile being dragged
tile_selected = "#FF4C5A78"  # the tile Enter would take
```

```toml
[grid]
search_height = 72.0     # the filter strip; its text is sized from this
```

## Why it's built this way

Hand-written layout and hit-testing on Windows.UI.Composition, no GUI framework.
The Rust GUI landscape is still weak, with a
[2026 survey](https://alexzhang-5109.xlog.app/-yi--pan-dian-zai-WASM-shi-jie-zhong-yong-xian-de-ji-shi-ge-Rust-GUI?locale=en)
putting 94.4% of crates at not production ready: Xilem isn't there, egui is a
debug UI with limited styling, iced has doc gaps, Dioxus is WebView underneath.
A framework earns its keep on complex UI anyway, and this is a uniform grid of
identical tiles, so layout is a few hundred lines.

C#, WPF or WinUI 3 would have been faster to build, but idle RSS runs 50-60MB or
120MB against 15-20MB native, and this process is resident all day. Tauri and
Electron add a second resident process on top of that, and can't reach the shell
APIs that justify going native at all.

The hotkey is `RegisterHotKey`, never `WH_KEYBOARD_LL`. A low-level hook sits in
every keystroke on the machine, degrading input latency everywhere and
attracting security tooling. `RegisterHotKey` is process-scoped, released by the
OS even on a crash, and
[counts as the last input event](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setforegroundwindow),
which is what grants the foreground right to activate another window.

Tabs arrive over a loopback WebSocket rather than native messaging, the
documented transport. MV3 service workers die after ~30s idle, and there are
[reports of them dying anyway](https://github.com/GoogleChrome/developer.chrome.com/issues/2688)
at 5-6 minutes with `connectNative()`. Chrome 116+ keeps the worker alive as
long as messages flow.

## Known gaps

- Taskbar pin order is alphabetical until you arrange it. Windows keeps its own
  order in an undocumented registry blob, so dragging a tile writes an `order`
  list instead.
- Bookmarks are not built.
- Firefox needs its own extension build.
- Tab tiles cannot be rearranged, and neither can a filtered grid.
- Dragging moves a tile within its own section. Moving one between sections
  means editing `bentopick.toml`.
