# BentoLaunch config

`bentolaunch.toml`, next to the exe. Written with defaults on first run, and
missing keys take theirs, so a file with one line in it is a valid config.

**No restart needed.** BentoLaunch watches the file and reloads on save, hotkey
included. Pins added from the tray are written here, and hand-written comments
and formatting are preserved.

```toml
hotkey = "alt+`"     # ctrl, alt, shift, win + a key
dry_run = false      # true: log what a click would do, do nothing
```

## Sections

Order here is order on screen. Empty sections do not render.

Out of the box the panel is split down the middle: **`Browsing` down the whole
right side, everything else down the left**, with the modes bar across the
bottom. Two halves and one question each - what is open on the web, and what is
on this machine - and a panel split that way is answered by looking at one half
of it, which no stack of full-width rows manages. It also gives the center block
a half to sit in on either side of it.

The left half is `Active` (every window) over `Apps` (taskbar pins and anything
you pin yourself). Running things above launchable ones, because switching to
something that exists beats starting something new.

`Apps` mirrors your taskbar:

- same pins, same left-to-right order
- a line under the icon when the app is open
- clicking an open one switches to it, never starts a second copy
- open but unpinned apps follow the pins

So a pin sits where you already know it, running or not.

`Apps` lists apps. `Browsing` and `Active` list windows and tabs, which is a
different question.

```toml
[[sections]]
title  = "Browsing"
source = "windows"
match  = ["chrome.exe", "msedge.exe", "firefox.exe"]
side   = "right"     # the whole right side; everything else fills the left

[[sections]]
title  = "Files"
source = "windows"
match  = ["explorer.exe"]

[[sections]]
title  = "Active"
source = "extra"     # windows the Apps row cannot reach; see below

[[sections]]
title  = "Apps"
source = ["taskbar", "running"]  # your taskbar pins, then anything else open
order  = []          # pin names, in order; written by dragging a tile.
                     # Empty means the taskbar's own order is mirrored.

[[sections]]
title  = "Places"
source = "manual"
items = [
    'R:\dev',
    { title = "Display", target = "ms-settings:display" },
    { title = "Wikipedia", target = "https://wikipedia.org" },
]

# Empty until move mode brings the six out, so it costs a row only while it is
# being used. Takes the modes bar's row while it is out: the foot of the
# panel is one row, and the two take turns.
[[sections]]
title  = ""
source = "move"
side   = "full"

[[sections]]
title  = ""
source = "modes"
side   = "full"
```

Two keys place a section's box. Both optional, both set by **Edit layout**:

```toml
[[sections]]
title     = "Launch"
source    = "taskbar"
side      = "left"      # which lane: "left", "right" or "full"
max_items = 12          # most tiles to show; omit for all of them
```

A box picks a lane, and that is the whole of its x axis:

| `side` | Where the box goes |
|---|---|
| `"left"` | The left lane, top to bottom |
| `"right"` | The right lane |
| `"full"` | Edge to edge |

Height is not a choice - a box is as tall as what it holds - so the only other
thing to say is where it comes in its lane, which is the order it is listed in
the file. A box that says nothing takes the default lane.

A lane is a property of one box, never a relationship with another. "Left" is
still the left half when nothing is on the right, so a box does not change shape
because a browser disconnected. Where the seam down the panel sits is
`grid.split`: one number for the whole panel, hand-edited.

`max_items` is what a tabs or bookmarks box wants: both lists are as long as the
browser makes them, and a box that grows without limit pushes the rest off
screen.

`match` lists process names, case-insensitive, and only applies to `windows` and
`extra`. Sections claim windows in order and each window is claimed once, so put
filtered sections above the unfiltered catch-all. Keep exactly one windows
section without a `match`, or windows from an unlisted app have nowhere to go.

`extra` is `windows` minus the redundant half: only apps with **more than one**
window open.

One window is already in `Apps` under its own name, so repeating it by title
says nothing. Four windows is the opposite: the `Apps` tile reaches only the
most recent, and the titles pick the rest.

So an `extra` section stays empty until it has something to add. It needs apps
coming from `taskbar` and `running`. Alone it leaves single-window apps
unreachable.

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

## Browser tabs

Off by default. Windows has no API for browser tabs, so this needs an extension:
`extension/`, Chromium only for now.

```toml
[[sections]]
title  = "Browsing"  # the default: extra windows first, then tabs
source = [
    # Only once a browser has a second window: one is reached from Apps.
    { source = "extra", match = ["chrome.exe", "msedge.exe", "firefox.exe"] },
    "tabs",          # empty until the extension connects
]

[browser]
enabled = true
port    = 8777
```

Pairing is not a config edit. Load the extension, then right-click the tray
icon: **Browser > Pair a browser...**. BentoLaunch shows six digits, you type them
into the extension's options page, and that is the whole setup - it switches the
bridge on for you if it was off. **Browser > Forget** undoes it.
`extension/README.md` has the details.

Tabs sit under the same header as your browser windows, right behind them,
since both answer the same question.

## The center block

Its own table, not a section: nothing `at` can say puts a box in the middle
without cutting the panel in half to get there.

```toml
[center]
rows     = 0       # 0 turns the block off, and is what it ships as;
                   # an empty block draws nothing whatever this says
columns  = 3       # tiles across in each half; rows * columns is a half's slots
contents = "split" # "split", "one", "apps", "sites" - see The center
color    = "#38FFC24B"
apps  = [
    'C:\Program Files\Some\App.exe',
    { title = "Dev", target = 'R:\dev' },
]
sites = [
    "https://wikipedia.org",
    { title = "Docs", target = "https://docs.example" },
]
```

Both lists take the same entries a manual section's `items` does: a path, a
`.lnk`, `shell:AppsFolder\<AppUserModelID>`, or a URI. **Edit center** writes
them; hand-editing works the same as everywhere else here.

A site wears its own favicon when a paired browser has sent one for that site,
and the shell's icon for the URL otherwise - which is the default browser's
logo, the same for every site. Four identical logos in the middle of the screen
is the block failing at the only thing it is for, so the favicon is asked for
every time the grid is built: connect a browser and they arrive on the next
summon.

`rows = 0` is how the block is turned off. How much center you want and whether
you want any are the same question, so one settings square answers it.

Turning `split` off gives one box, `columns` wide, holding the apps and then the
sites. It is narrower, not the same block merged - the width is what `columns`
says either way.

`rows` and `columns` are capped at four each. The block is held in the middle
and everything wraps around it, so one bigger than the panel would leave the
grid nowhere to wrap to.

The block wins over `max_columns`: that cap is a preference about how long a row
stays scannable, and a block hanging off the edge of the panel is a click that
lands on nothing. It still yields to what fits the screen. The panel may also
come out one column wider than it needed, to keep the spare columns even so the
block lands exactly on centre.

## Bookmarks

Same extension, same switch, no extra setup. Add a `bookmarks` source:

```toml
[[sections]]
title     = "Bookmarks"
source    = "bookmarks"
max_items = 12
```

The box is the **bookmarks bar only**, one level deep. Not "Other bookmarks",
which is an archive of thousands and would bury the panel it was pasted into,
and not the folders sitting on the bar, since there is nothing BentoLaunch could
do with one.

The rest of them are one square away: the box's last tile is **All bookmarks**,
which fills the panel with the whole tree. See *All of them*.

A bookmark tile is a URL handed to the shell, so it opens in your default
browser and works whether or not the browser that sent it is still running.

Read-only, and it stays that way: nothing is ever written to a browser profile.
"Bookmark this tab" is not built - to add one, use the browser.

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
  `%LOCALAPPDATA%\bentolaunch\peers.json`, which Windows restricts to your
  account. It never travels over the socket; each side proves it knows it.

And the guard that points the other way: **BentoLaunch proves itself to the
extension too**, before the extension sends a single tab title. Otherwise
anything that grabbed port 8777 first would be handed your open tabs by an
extension with no way to tell the difference. For the same reason, pairing is
refused outright when something else holds the port, and the tray says so
instead of failing quietly.

What it does not guard against: software already running under your own account.
That software can read the tokens, but it can also read your browser profile
directly, so this is not the interesting way in. `src/browser/gate.rs` has the
full reasoning at the top of the file.

## Appearance

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
box_edge = "#14FFFFFF"       # the seams between boxes; "#00000000" turns them off
center_edge = "#66FFC24B"    # the frame round the center, and the seam in it
```

Boxes tile the panel with no gaps, so `box_edge` lines meet and read as the
seams of the bento rather than as a border round each box. `center_edge` is
distinctly stronger, because the block is the one thing on the panel that is in
front of the layout rather than part of it.

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
host, and BentoLaunch is a long-running GUI that would end up with a second copy
of itself - plus a registry key and a host manifest, which is footprint this app
does not want.

What a fixed port costs is that something else can take it, and an extension
cannot read a file to find out where BentoLaunch went. So the answer is not a
fallback port but a handshake: both ends prove they know the token, BentoLaunch
going first, and a bind failure is reported in the tray rather than retried
around.

## Known gaps

- Unpinned running apps are named by their executable: `WindowsTerminal`, not
  `Windows Terminal`. Pins are named by their shortcut and read properly.
- A Store app pinned to the taskbar matches no window, so it never shows as
  running. It pins by AppUserModelID with no target path.
- Bookmarks are read-only, Chromium only, and the bookmarks bar only.
- A box cannot be dragged to another row. Edit layout moves it a place at a
  time, which is the same thing in more keystrokes.
- Firefox needs its own extension build.
- Tab tiles cannot be rearranged, and neither can a filtered grid.
- Dragging moves a tile within its own section. Moving one between sections
  means editing `bentolaunch.toml`.
