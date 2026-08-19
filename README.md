<img src="assets/dashpick-256.png" width="88" alt="">

# DashPick

A hotkey-summoned grid of everything worth switching to: running windows,
browser tabs, taskbar pins, folders, apps, settings pages, links.

Press `` Alt+` ``. Type to narrow. Enter takes the top match, or click a tile.
Esc closes and puts you back where you were.

Windows 11 only. Never asks for admin, and writes nothing outside its own
config and `%LOCALAPPDATA%\dashpick`.

`DESIGN.md` for architecture and decisions, `STATUS.md` for what works and what
is still unverified.

Early days. It does what this README says, but plenty is listed as unverified in
`STATUS.md` and it has only ever run on one machine.

## Running

```powershell
cargo build
target\debug\dashpick.exe
```

Starts silent, tray icon only. Only one runs at a time: launch it again, from a
taskbar pin or anywhere else, and the panel comes up instead of a second copy.

Right-click the tray icon:

| Item | Does |
|---|---|
| Show DashPick | Same as the hotkey |
| Add app… | Browse installed apps, Store apps included, and pin one |
| Add folder… | Pin a folder |
| Add file or shortcut… | Pin a file or `.lnk` |
| Edit settings… | Open `dashpick.toml` in your editor |
| Exit | Quit |

Log: `%LOCALAPPDATA%\dashpick\dashpick.log`

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
| Right-click anywhere | Add app/folder/file, keep open, settings |
| Drag a file or folder in from Explorer | Pin it where you dropped it |

Click and drag never get confused: under the system's drag threshold is a click,
past it is a drag.

Only pinned tiles move. Running windows stay in most-recent order.

Every change goes straight into `dashpick.toml`. Nothing is remembered anywhere
else, and all of it can be undone by hand. Drops land in the section you dropped
on, or the first manual one. Taskbar order is saved as an `order` list, since
Windows does not expose its own.

### Keeping the panel open

Pushpin, top right. Also in the right-click menu.

The panel normally closes the moment it loses focus. A drag from Explorer takes
focus before there is anything to drop, so pin it open first, then go get the
file. Resets when the panel closes.

## Config

`dashpick.toml`, next to the exe. Written with defaults on first run.

**No restart needed.** DashPick watches the file and reloads on save, hotkey
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
  `%LOCALAPPDATA%\dashpick\bridge-token`, which Windows restricts to your account.

What it does not guard against: software already running under your own account.
That software can read the token, but it can also read your browser profile
directly, so this is not the interesting way in. `DESIGN.md` has the full
reasoning under "Who is allowed on the socket".

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
section_gap = 10.0
corner_radius = 8.0

[theme]
panel = "#F01A1A1E"      # #AARRGGBB or #RRGGBB
tile = "#FF2A2A32"
tile_hover = "#FF3C3C48"
text = "#FFE8E8EC"
header = "#FF9A9AA8"
tile_drag = "#FF4A4460"    # a tile being dragged, and the pushpin when it is on
tile_selected = "#FF4C5A78"  # the tile Enter would take
```

```toml
[grid]
search_height = 72.0     # the filter strip; its text is sized from this
```

## Known gaps

- Taskbar pin order is alphabetical until you arrange it. Windows keeps its own
  order in an undocumented registry blob, so dragging a tile writes an `order`
  list instead.
- Bookmarks are not built. Same extension, same channel.
- Firefox needs its own extension build.
- Tab tiles cannot be rearranged, and neither can a filtered grid.
- Dragging moves a tile within its own section. Moving one between sections
  means editing `dashpick.toml`.
- Window tiles show icons, not live previews.
