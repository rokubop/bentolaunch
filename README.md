# flick

A hotkey-summoned grid of everything worth switching to: running windows,
taskbar pins, folders, apps, settings pages, links.

Press `` Alt+` ``. Click a tile. Esc closes and puts you back where you were.

Windows 11 only. See `DESIGN.md` for architecture.

## Running

```powershell
cargo build
target\debug\flick.exe
```

Starts silent, tray icon only. Right-click the tray icon to exit.

Log: `%LOCALAPPDATA%\flick\flick.log`

## Config

`flick.toml`, next to the exe. Written with defaults on first run. Edit and
restart.

```toml
hotkey = "alt+`"     # ctrl, alt, shift, win + a key
dry_run = false      # true: log what a click would do, do nothing
```

### Sections

Order here is order on screen. Empty sections do not render.

```toml
[[sections]]
title = "Pinned"
source = "taskbar"   # apps pinned to your Windows taskbar

[[sections]]
title = "Windows"
source = "windows"   # every open window, most recent first

[[sections]]
title = "Places"
source = "manual"
items = [
    'R:\dev',
    { title = "Display", target = "ms-settings:display" },
    { title = "Wikipedia", target = "https://wikipedia.org" },
]
```

Use `'single quotes'` for Windows paths. TOML processes `\` as an escape inside
`"double quotes"`, so `"R:\dev"` is a parse error.

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

Tile size is fixed and never changes with item count, which is what makes tile
positions learnable. The panel grows outward from the center until it hits
`max_screen_fraction` of the monitor, then scrolls.

```toml
[grid]
tile_width = 220.0
tile_height = 150.0
gap = 14.0
padding = 24.0
max_screen_fraction = 0.8
label_height = 30.0
header_height = 28.0
section_gap = 14.0
corner_radius = 10.0

[theme]
panel = "#F01A1A1E"      # #AARRGGBB or #RRGGBB
tile = "#FF2A2A32"
tile_hover = "#FF3C3C48"
text = "#FFE8E8EC"
header = "#FF9A9AA8"
```

## Known gaps

- Taskbar pin **order** is not the taskbar's; entries are sorted by name. The
  real order is an undocumented registry blob. Use a manual section for exact
  control.
- Browser tabs and bookmarks need a browser extension. Not built yet.
- Pinning is by hand in `flick.toml`. Drag-and-drop is not built yet.
- Window tiles show icons, not live previews.
