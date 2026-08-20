<img src="assets/bentopick-256.png" width="88" alt="">

# BentoPick (WIP)

A hotkey-triggered overlay in the center of your screen for all your running apps and browser tabs represented as big icons that are easily clickable. Type filtering also supported.

Example:
1) Press `` Alt+` `` to immediately show BentoPick - See running apps, taskbar pins, and browser tabs in a grid of tiles.
2) Click what you want to switch to, or type to filter the grid

## Install

Windows 11 only.

### 1. The app

1. Download [bentopick.exe](https://github.com/rokubop/bentopick/releases/latest/download/bentopick.exe).
2. Move it to `%LOCALAPPDATA%\Programs\bentopick\`.
3. Run it. Windows warns about an unrecognised app: **More info** -> **Run anyway**.
4. Press `` Alt+` ``.

Tray icon only, no window until you summon it. To update, copy a newer exe over
the old one.

### 2. Browser tabs (optional)

Only if you want tabs in the grid. Chromium browsers for now.

1. Download `bentopick-extension.zip` from the
   [same release](https://github.com/rokubop/bentopick/releases/latest) and unzip it.
2. Go to `chrome://extensions`, turn on Developer mode, click **Load unpacked**,
   pick the unzipped folder.
3. Right-click the BentoPick tray icon: **Browser > Pair a browser...**. It
   shows six digits.
4. Open the extension's options page, type the digits, click **Pair with
   BentoPick**.

Tabs appear in the grid straight away. To unpair: **Browser > Forget**.

### Start it at login

```powershell
$s = (New-Object -ComObject WScript.Shell).CreateShortcut(
    "$env:APPDATA\Microsoft\Windows\Start Menu\Programs\Startup\BentoPick.lnk")
$s.TargetPath = "$env:LOCALAPPDATA\Programs\bentopick\bentopick.exe"
$s.Save()
```

Delete that shortcut to undo it.

## Build from source

If instead of downloading the exe you want to build it yourself.

Needs [Rust](https://rustup.rs). From **PowerShell, not WSL**, the toolchain and
the window are Windows-native.

```powershell
git clone https://github.com/rokubop/bentopick
cd bentopick
cargo build --release
target\release\bentopick.exe
```

`cargo build` without `--release` keeps a console window so the log is visible.

Careful which copy you are running. The panel reads the config next to whichever
exe started, so an installed copy and `target\release\` have separate settings.

## Running

Starts silent, tray icon only. Only one runs at a time: launch it again, from a
taskbar pin or anywhere else, and the panel comes up instead of a second copy.

Right-click the tray icon:

| Item | Does |
|---|---|
| Show BentoPick | Same as the hotkey |
| Add app… | Browse installed apps, Store apps included, and pin one |
| Add folder… | Pin a folder |
| Add file or shortcut… | Pin a file or `.lnk` |
| Browser ▸ | Pair a browser for tabs, or forget one |
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
```

Pairing is not a config edit. Load the extension, then right-click the tray
icon: **Browser > Pair a browser...**. BentoPick shows six digits, you type them
into the extension's options page, and that is the whole setup - it switches the
bridge on for you if it was off. **Browser > Forget** undoes it.
`extension/README.md` has the details.

Tabs sit under the same header as your browser windows, right behind them,
since both answer the same question.

**Read this before turning it on.** It opens a port on your machine that only
your own computer can reach, and it installs an extension that can read the
title and URL of every tab you have open. Both are the feature working as
intended, and both are your call.

What guards that port:

- Nothing is admitted that you have not paired, and pairing takes a code shown
  by the app itself. Turning the bridge on grants nothing on its own.
- Websites cannot get in. Any page can try to open a connection to your own
  machine, but browsers stamp every connection with who is making it and pages
  cannot fake that stamp. Only a paired extension is let through.
- A separate secret per browser, from the OS random generator, kept in
  `%LOCALAPPDATA%\bentopick\peers.json`, which Windows restricts to your
  account. It never travels over the socket; each side proves it knows it.

And the guard that points the other way: **BentoPick proves itself to the
extension too**, before the extension sends a single tab title. Otherwise
anything that grabbed port 8777 first would be handed your open tabs by an
extension with no way to tell the difference. For the same reason, pairing is
refused outright when something else holds the port, and the tray says so
instead of failing quietly.

What it does not guard against: software already running under your own account.
That software can read the tokens, but it can also read your browser profile
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
long as messages flow. Native messaging would also have the browser spawn the
host, and BentoPick is a long-running GUI that would end up with a second copy
of itself - plus a registry key and a host manifest, which is footprint this app
does not want.

What a fixed port costs is that something else can take it, and an extension
cannot read a file to find out where BentoPick went. So the answer is not a
fallback port but a handshake: both ends prove they know the token, BentoPick
going first, and a bind failure is reported in the tray rather than retried
around.

## Known gaps

- Taskbar pin order is alphabetical until you arrange it. Windows keeps its own
  order in an undocumented registry blob, so dragging a tile writes an `order`
  list instead.
- Bookmarks are not built.
- Firefox needs its own extension build.
- Tab tiles cannot be rearranged, and neither can a filtered grid.
- Dragging moves a tile within its own section. Moving one between sections
  means editing `bentopick.toml`.
