# flick

A hotkey-summoned grid of everything worth switching to: running windows,
taskbar pins, folders, apps, settings pages, links.

Press `` Alt+` ``. Click a tile. Esc closes and puts you back where you were.

Windows 11 only. `DESIGN.md` for architecture and decisions, `STATUS.md` for
what works and what is next.

## Running

```powershell
cargo build
target\debug\flick.exe
```

Starts silent, tray icon only.

Right-click the tray icon:

| Item | Does |
|---|---|
| Show flick | Same as the hotkey |
| Add app… | Browse installed apps, Store apps included, and pin one |
| Add folder… | Pin a folder |
| Add file or shortcut… | Pin a file or `.lnk` |
| Edit settings… | Open `flick.toml` in your editor |
| Exit | Quit |

Log: `%LOCALAPPDATA%\flick\flick.log`

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

Every change goes straight into `flick.toml`. Nothing is remembered anywhere
else, and all of it can be undone by hand. Drops land in the section you dropped
on, or the first manual one. Taskbar order is saved as an `order` list, since
Windows does not expose its own.

### Keeping the panel open

Pushpin, top right. Also in the right-click menu.

The panel normally closes the moment it loses focus. A drag from Explorer takes
focus before there is anything to drop, so pin it open first, then go get the
file. Resets when the panel closes.

## Config

`flick.toml`, next to the exe. Written with defaults on first run.

**No restart needed.** flick watches the file and reloads on save, hotkey
included. Pins added from the tray are written here, and hand-written comments
and formatting are preserved.

```toml
hotkey = "alt+`"     # ctrl, alt, shift, win + a key
dry_run = false      # true: log what a click would do, do nothing
```

### Sections

Order here is order on screen. Empty sections do not render.

Running things are listed before launchable ones, because switching to something
that exists beats starting something new.

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
tile_drag = "#FF4A4460"   # a tile being dragged, and the pushpin when it is on
```

## Known gaps

- Taskbar pin order is alphabetical until you arrange it. Windows keeps its own
  order in an undocumented registry blob, so dragging a tile writes an `order`
  list instead.
- Browser tabs and bookmarks need a browser extension. Not built yet.
- Dragging moves a tile within its own section. Moving one between sections
  means editing `flick.toml`.
- Window tiles show icons, not live previews.
