# flick — design decisions

A centered, hotkey-summoned grid of everything worth switching to on this PC:
running windows, pinned apps, pinned Explorer folders, browser tabs, pinned
bookmarks. Uniform, configurable tile size. Click to launch or focus.

This document records decisions already made and *why*, including the options
that were rejected. It is the source of truth for the architecture. Research was
done August 2026; sources are linked inline.

---

## Target environment

- **Windows 11 only.** Confirmed dev machine: Win11 Pro, build 22631 (23H2).
  Win11-only is a deliberate choice — borderless screen capture does not exist
  on Win10, and the fallback path isn't worth carrying.
- Rust `stable-x86_64-pc-windows-msvc` (1.89.0), toolchain on the Windows side.
- Repo lives at `R:\dev\flick`. Develop from **PowerShell**, not WSL — the
  toolchain, the GUI process, and the packaging commands are all Windows-native.

---

## Stack

**Rust + the `windows` crate + Windows.UI.Composition + Windows.Graphics.Capture.**

No GUI framework. This is deliberate:

- The Rust GUI landscape was surveyed and is still weak. Xilem is explicitly not
  production-ready; egui is characterized even by advocates as a debug UI with
  limited custom styling; iced has documentation gaps; Dioxus is WebView
  underneath. A [2026 survey](https://alexzhang-5109.xlog.app/-yi--pan-dian-zai-WASM-shi-jie-zhong-yong-xian-de-ji-shi-ge-Rust-GUI?locale=en)
  found 94.4% of Rust GUI crates not production-ready.
- A framework only earns its keep when the UI is complex. This UI is a **uniform
  grid of identical tiles** — layout and hit-testing are a few hundred lines.
- Critically, a framework would *fight* the one hard requirement: getting live
  D3D11 capture textures into tiles.

The layers, and why they fit together:

| Layer | Choice | Why |
|---|---|---|
| Visual tree | **Windows.UI.Composition** (WinRT Visual layer) | Microsoft recommends it over raw DirectComposition. Free GPU compositing, rounded-corner clips, drop shadows, backdrop blur, implicit spring animations. |
| Window previews | **Windows.Graphics.Capture** | Returns `IDirect3DSurface`, which drops straight into a `CompositionSurfaceBrush`. No copy, no interop boundary, no airspace problem. |
| Text | **DirectWrite** | Native, fast. |
| Layout / hit-testing | Hand-written | Uniform grid. Small. |

Unpackaged Win32 needs `CreateDispatcherQueueController` and
`ICompositorDesktopInterop::CreateDesktopWindowTarget` to host a composition
tree. Known shape, not a blocker.

---

## Rejected alternatives (do not revisit without new information)

- **`DwmRegisterThumbnail`** — rejected. Per [Microsoft's guidance on exactly this
  scenario](https://learn.microsoft.com/en-us/answers/questions/5966957/recommended-api-for-live-window-previews-with-cust)
  (a Win32 picker with live previews and custom overlays): DWM thumbnails render
  *above* all your content, so labels, badges, and rounded corners would be
  occluded, and the destination must be a top-level HWND. No Z-order control.
- **`PrintWindow`** — rejected. Slow, synchronous, ~1fps, and returns blank
  bitmaps for Chromium and other GPU-composited apps.
- **C# / WPF / WinUI 3** — viable runner-up, faster to build, but compositing
  D3D11 capture textures into WPF means `D3DImage`/airspace pain, and idle RSS is
  ~50–60MB (WPF) to ~120MB (WinUI 3) against ~15–20MB native. This process is
  permanently resident; idle footprint is a metric that matters.
- **Tauri / Electron** — rejected. WebView2 is a second resident process and the
  shell APIs that justify going native aren't reachable.
- **`WH_KEYBOARD_LL`** — rejected, and must stay rejected. It inserts the process
  into every keystroke system-wide, degrading whole-machine input latency and
  attracting security tooling. Use `RegisterHotKey`.

---

## Focus model: match Alt+Tab, take activation normally

`SetForegroundWindow` is restricted, but [one qualifying condition](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setforegroundwindow)
is "the calling process received the last input event" — and
[`RegisterHotKey`](https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-registerhotkey)
delivers `WM_HOTKEY` to your window as exactly that.

**So activating from the hotkey just works.** No `AttachThreadInput` trickery, no
`AllowSetForegroundWindow` dance. This is a second reason to prefer
`RegisterHotKey` over a keyboard hook, beyond latency.

Record `GetForegroundWindow()` *before* showing, so Esc restores the caller.

`WS_EX_NOACTIVATE` was considered and rejected: it would force hand-routing all
keyboard input for type-to-filter, for no benefit in an app whose entire purpose
is to switch away from the current window.

---

## Data sources

### Running windows

`EnumWindows` + filter on `DwmGetWindowAttribute(DWMWA_CLOAKED)` to drop hidden
UWP and shell windows.

**Maintain the list continuously via `SetWinEventHook`** (create / destroy /
foreground / name-change). Never enumerate at show time — enumerating on the
hotkey is the single most common reason these launchers feel sluggish.

Known caveat: on Win11 some windows (Taskbar, Start, Search) are only visible via
UI Automation, not `EnumWindows`. Probably irrelevant here; noted in case
something turns up missing.

### Window previews — the constrained part

Windows.Graphics.Capture draws a **yellow notification border around every
captured window**, one per session. At 50 tiles that is 50 yellow borders. Unusable.

Suppressing it requires all of:

1. `GraphicsCaptureSession.IsBorderRequired = false`
   ([docs](https://learn.microsoft.com/en-us/uwp/api/windows.graphics.capture.graphicscapturesession.isborderrequired))
2. The `graphicsCaptureWithoutBorder` capability in a package manifest — which
   requires **package identity**, i.e. a **sparse package** (packaging with
   external location). Does not change the installer or binary layout.
   ([docs](https://learn.microsoft.com/en-us/windows/apps/desktop/modernize/grant-identity-to-nonpackaged-apps))
3. One-time user consent via `RequestAccessAsync(GraphicsCaptureAccessKind::Borderless)`
   ([docs](https://learn.microsoft.com/en-us/uwp/api/windows.graphics.capture.graphicscaptureaccess.requestaccessasync))
4. Windows 11. Not available on Win10.

Sideloading with a restricted capability needs no approval; only Store
submission would.

Use `IGraphicsCaptureItemInterop::CreateForWindow(hwnd)` to target a window
directly and skip the system picker UI.

**Preview policy is per-tile, not global.** Windows get previews; pinned apps,
folders, and bookmarks get icons via `IShellItemImageFactory` (better than
`SHGetFileInfo` — real thumbnails and high-res icons).

**Do not run 50 live sessions.** A switcher needs a *recent frame*, not 60fps:
- Cap concurrent sessions at ~16–24, for visible tiles only.
- LRU everything else to its last cached frame.
- Paint from cache instantly on show; refresh behind that.
- Tear all sessions down on hide (prevents GPU memory growth).

### Browser tabs — WebSocket, not native messaging

No OS-level API exists. Requires a browser extension. **The obvious transport is
the wrong one:** MV3 service workers die after ~30s idle, and while
`connectNative()` is documented to keep them alive, there are
[open reports](https://github.com/GoogleChrome/developer.chrome.com/issues/2688)
of the worker dying at 5–6 minutes anyway.

**Design: the app hosts a localhost WebSocket server; the extension connects to
it.** Chrome 116+ keeps the service worker alive as long as WebSocket messages
flow, so a low-rate heartbeat gives a genuinely persistent channel.

One Chromium extension covers Chrome/Edge/Brave/Vivaldi. Firefox needs a
separate build.

### Who is allowed on the socket

A loopback WebSocket is not a private channel. Two callers can reach it and they
need different answers.

**A web page.** Any site you have open can script
`new WebSocket("ws://127.0.0.1:8777/")`. Without a check it could enumerate every
tab you have open, titles and URLs. Browsers attach `Origin` to the handshake and
page JavaScript cannot forge or suppress it, so an origin allowlist closes this
completely. This is the threat that actually matters.

**Another local program.** Not a browser, so `Origin` is whatever it types. A
token from `BCryptGenRandom` stands in the way, presented in the request path
and compared without an early return.

**Be honest about what the token is worth.** It lives in plaintext in
`flick.toml`, so anything running as you can simply read it. It stops processes
running as a *different* user, and it stops blind attempts. It does not stop
code running as you — but such code can already read the file, your browser
profile, and everything else you own, so this socket is not its best target.

The origin check is the gate that does the real work, because the threat that
actually exists is a web page, and a page cannot forge its origin.

Both are required, so half a configuration refuses to listen. The bridge is off
by default and binds `127.0.0.1` explicitly — never the unspecified address,
which would put the tab list on every interface on the machine.

Past the gate, the connection is still not trusted with much: a client can set
what flick shows and receive focus commands, nothing else. There is no path
from a tab's title or URL to `ShellExecuteW`. Message size, frame size, live
connections and tab count are all capped, because a client that passed the gate
can still be buggy.

Pairing is trust-on-first-use by hand: connect once, read the refused origin out
of the log, paste it into `browser.allow`. No pairing UI, and no default
allowlist to leak.

### Raising the browser: hand the right over, do not guess the window

Switching a tab needs nothing from Windows. Raising the window does, and the
browser cannot do it unaided — the foreground right belongs to flick, because
the hotkey made flick the last process to receive input.

So flick calls `AllowSetForegroundWindow` for the processes that own browser
windows, then asks the extension to do both halves. `chrome.windows.update`
then succeeds.

Rejected: flick raising the window itself. It has no way to map a browser's
internal `windowId` onto an HWND, and matching on title is a race — the title
only becomes correct *after* the tab switch. The browser already knows the
mapping. Give it the right and let it do the work.

Rejected: `AllowSetForegroundWindow(ASFW_ANY)`. Simpler, but it lets anything on
the machine steal foreground for the same window. The window store already knows
which pids own browser windows.

Note the socket's own peer process is the wrong target: Chrome opens sockets
from its network process, which owns no windows.

### Bookmarks — via the extension, not the files

Once the WebSocket channel exists, take bookmarks from `chrome.bookmarks` rather
than parsing files. The file paths are real
(`%LOCALAPPDATA%\Google\Chrome\User Data\Default\Bookmarks`, and Firefox's
`places.sqlite`) but Chrome holds its file in memory and overwrites on its own
schedule, and Firefox's DB is locked while running. The extension gives live,
correct data over a channel already required for tabs.

### Pinned apps / folders

Drag-and-drop via an `IDropTarget` COM server (`#[implement]` in the `windows`
crate). Resolve `.lnk` via `IShellLink`. Icons via `IShellItemImageFactory`.

---

## Safety rules — non-negotiable

The app is read-mostly and unprivileged by design. These four rules provide the
guarantee:

1. **Never request elevation.** Manifest `asInvoker`, always. A standard-user
   process cannot modify system files or anything outside the user profile. The
   app needs no privileged operation. This is the strongest available guarantee
   and it is free.
2. **Portable single exe.** Config next to the binary; caches in
   `%LOCALAPPDATA%\flick`. No installer, no scattered state.
3. **Read-only toward everything else.** Never write to a browser profile or
   another app's data. The only writes are flick's own config and cache.
4. **No `WH_KEYBOARD_LL`.** `RegisterHotKey` is process-scoped and released by
   the OS on exit, including on a crash.

### Total persistent system footprint

| Item | Reversal |
|---|---|
| Sparse package registration (per-user) | `Remove-AppxPackage` |
| Autostart entry (optional) | Delete one Run key / Startup shortcut |
| Browser extension per profile | Uninstall in browser |
| Borderless-capture consent | Revoke in Settings → Privacy & security |

Nothing else on the machine changes.

### Failure modes to design against

Not corruption — hangs. These are the real risks:

- **Invisible topmost window swallowing clicks.** The classic overlay-app
  failure, and the one that feels like "my PC is broken." Install a panic hook
  that destroys the window, plus a watchdog thread that force-hides if the UI
  thread stops ticking.
- **UI thread blocked on a shell call.** `IShellItemImageFactory` and
  `SHGetFileInfo` can block for *seconds* on network paths or a misbehaving shell
  extension. All shell and capture calls go on workers with timeouts. Top hang risk.
- **GPU memory growth from capture sessions.** Hard-cap concurrent sessions; tear
  down on hide.

### Development isolation

**Windows Sandbox is a poor fit and should be skipped.** Technically it's nested
GPU paravirtualization, which does not support the capture path. More
fundamentally, this app's entire job is enumerating *real* windows and tabs — in
a sandbox there is nothing to enumerate. A Hyper-V VM has the same emptiness
problem.

Develop on the real machine. Isolate only the two things that touch persistent
state:

- **Browser extension → a separate Chrome profile first.** Free to create, zero
  exposure to the main profile, fully testable.
- **Sparse package → the only system-level registration.** Per-user, and
  `Remove-AppxPackage` is reliable.

---

## Build order

**Milestone 1 — dry run. Done.**
Enumerate everything, render the full grid with icons, but make every click a
**no-op that logs what it would have activated**. Live with it for a few days.
This validates the entire risky half — reading the system — before any code acts
on anything. Dry-run mode is ON by default.

Scope: `asInvoker` manifest, tray icon + `RegisterHotKey`, composition visual
tree with configurable uniform grid, `SetWinEventHook` window model, icons via
`IShellItemImageFactory`, config file, Esc-restores-caller.

Built as designed. Implementation notes:

- Icons: two MTA workers plus a cache. UI thread asks, gets `None`, draws without
  an icon, repaints on `WM_ICON_READY`. A blocking COM call cannot be cancelled,
  so never waiting on one is the only real defence.
- `SIIGBF_ICONONLY`, not thumbnails. Thumbnail extraction is the hang-prone half
  of the shell imaging API. Windows get previews from capture anyway.
- Tile content is Direct2D + DirectWrite into a `CompositionDrawingSurface`. Same
  object a capture frame becomes, so Milestone 3 leaves the tile tree alone.
- Watchdog escape hatch: `SetWindowLongPtrW(GWL_EXSTYLE, |WS_EX_TRANSPARENT)`.
  Writes the window struct directly, no marshal to the owning thread, so it works
  when that thread is wedged. `ShowWindow` and `SetWindowPos` hang with it.
- `Handle(isize)` wraps `HWND`. `HWND` is `!Send` because of its raw pointer,
  which would keep the item store off worker threads.

**Milestone 2 — activation. Done.** Windows focus, everything else
`ShellExecuteW`. Every `focused`/`launched` log line matched the resulting
foreground window.

`SetForegroundWindow` needs no `AttachThreadInput` trickery, as predicted above.
The hotkey makes flick the last input recipient, and that right survives hiding
the panel. Minimized windows need `SW_RESTORE` first or they stay down.

Taskbar pins, sections and the parsing-name model landed here too, ahead of the
original order. "What's active" and "what I can launch" want the same tile.

**Milestone 3 — capture.** Sparse package, borderless consent, preview pipeline
with session caps and frame caching.

**Milestone 4 — browser.** Localhost WebSocket server, Chromium extension, tabs
+ bookmarks. Type-to-filter was pulled ahead of this and is **done**; 40 tabs
would have flooded the grid without it.

**Milestone 5 — rearranging and drag-and-drop pinning. Done**, ahead of 3 and 4.
`IDropTarget`, drag to reorder, unpinning, layout persistence.

### Edit mode: built, then removed

Original plan: an explicit mode, because click and drag are ambiguous on a tile
that also activates.

Built it. Wrong. Two reasons:

- First user could not find it. `F2` plus a tray item is not an entry point.
- Every comparable surface rejects the mode. Taskbar, bookmarks bar, Quick
  Access, Dock: click acts, drag rearranges, right-click manages. Only touch home
  screens have a mode, and for reasons flick does not share: no hover, no
  right-click, coarse targets.

The ambiguity is what `SM_CXDRAG` is for. 4px. The shell's own threshold, and
what those surfaces have used for twenty years.

Settled model:

| | |
|---|---|
| Click | activate |
| Drag a pinned tile | reorder in its section |
| Right-click | pin, unpin, show in Explorer, settings |
| Pushpin | keep the panel open |

**Pin what is in front of you.** The taskbar's best pin action is right-click the
running app. The browser's is a star on the current page. flick already lists
every running window, so right-click a window tile gives "Pin this app": no
picker, no typing. Same menu pins a tab as a bookmark once M4 lands.

Two constraints from the original design held:

- Reorder is for taskbar and manual sections only. Window tiles are MRU ordered
  by the foreground hook; a saved order would fight it on every focus change.
- Dropping from Explorer needs the panel to survive losing focus. That is a
  keep-open concern, not an edit one, so it is its own pushpin toggle. The drag
  *starts* in Explorer, so "suspend dismissal while a drag is in flight" is too
  late: the panel is gone before the drag exists.

Implementation notes:

- **A press is not yet a click.** `WM_LBUTTONDOWN` starts a press with capture;
  release decides. Past the threshold: reorder, or nothing on a tile flick cannot
  rearrange. Under it: activate, if the release is still on the same tile.
- **Layout gained bands.** One per rendered section, tiling the panel with no
  gaps, so a drop between two tiles still names a section. Insertion points are
  measured against tile centers.
- **Persistence is the config file, nothing else.** A manual reorder rewrites
  that section's `items`. Entries are moved, not rebuilt, so `{ title, target }`
  keeps its title. No separate layout store to fall out of sync.
- **Taskbar order is an `order` list of pin names** on the section. Windows does
  not expose its own (see below), so flick keeps one once the user states it.
- **Unpinning is manual sections only.** A taskbar entry belongs to the taskbar.
  Safety rule 3.
- **The pushpin is chrome, not content.** Does not scroll, so a long grid cannot
  carry it off the top. Glyph from Segoe MDL2, the shell's own icon font.
- **The drop target holds no state.** Each OLE call becomes a synchronous
  `SendMessageW` to the panel, so the panel stays the only thing touching its own
  fields. The reply is the drop effect, and it keeps the path list alive across
  the call for free.
- `OleInitialize` replaced `CoInitializeEx`. Same apartment, plus the
  drag-and-drop half `RegisterDragDrop` needs.

---

## The item model

A tile is a **window** (`HWND` to focus) or a **shell parsing name** (string for
the shell). Nothing else.

The second case carries the weight. Paths, folders, `.lnk`,
`shell:AppsFolder\<AppUserModelID>`, `ms-settings:display`, `https://example.com`
are all parsing names. One `ShellExecuteW` launches any of them. One
`IShellItemImageFactory` gets any of their icons. "Pin anything" needs no
per-type code.

Two exceptions, both in `shell/icons.rs`:

- A URI is not a shell item, but `SHCreateItemFromParsingName` does not say so.
  It returns a generic item with a blank-page icon. So for URIs, ask
  `AssocQueryStringW` what opens the scheme and use that app's icon.
- Packaged-app schemes have no exe to name, so that query fails with
  `ERROR_NO_ASSOCIATION`. `ms-settings` maps to the Settings AppUserModelID
  through a small table.

## Sections

Ordered list, each with a title and a source (`taskbar`, `windows`, `manual`),
configured in `flick.toml`. They stack under their own headers and share one
column count so tiles line up. Empty sections do not render.

**Pins and windows never merge.** Steam pinned and Steam running are two tiles.
The redundancy is the point: a pin never moves, so its position is learnable, and
each tile means one thing. Merging them, or hiding a pin while its app runs, makes
tiles shift as you open and close things. That defeats the fixed tile size.

**Taskbar pins come from the `.lnk` folder, not the registry.**
`%APPDATA%\Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar`, one
shortcut per pinned app. `ShellExecuteW` on a `.lnk` launches its target;
`IShellItemImageFactory` on a `.lnk` returns its target's icon. flick never
resolves the shortcut itself.

Order is not recoverable. It lives in `HKCU\...\Explorer\Taskband\Favorites` as
an undocumented binary blob of serialised PIDLs. Not worth parsing against a
format Microsoft can change silently. Sorted by name until the user says
otherwise, at which point that section's `order` list — written by dragging a
tile in edit mode — is what drives it.

### Grouping by intent

A windows section can carry a `match` list of process names. Sections claim
windows in order, each window is claimed once, and an unfiltered section is the
catch-all. That is the whole mechanism: putting `["chrome.exe", ...]` above the
catch-all is what pulls the browsers into their own group.

Rejected: auto-grouping windows by exe with no config. It needs no setup, but
sections then appear and disappear as apps open and close, and the whole panel
reflows under the cursor. Explicit rules keep the shape of the grid stable, which
is the same reason tile size is fixed.

Running things sort above launchable ones. Switching to what exists is the more
common intent, so it gets the top of the panel.

### Type-to-filter

Built before tabs, because it blocks them: 40 tabs would flood a 60-tile grid
and push everything else off screen. The panel takes activation normally, so
typing needs no plumbing beyond `WM_CHAR`, and no letter key was spoken for.

Four decisions, all of them about not disturbing the grid:

- **Filtering hides, it never reorders.** A surviving tile keeps its section and
  its place in it — MRU for windows, the pinned order for the rest. Stable
  positions are what make the grid learnable, and the same argument that fixed
  the tile size and rejected auto-grouping applies here. A list that re-sorts on
  every keystroke is a list you have to re-read every keystroke.

- **The column count freezes for the duration of a query.** Taken from the
  unfiltered grid on the first character. Without it the panel re-derives its
  width per keystroke and walks sideways as matches fall away — 9 columns, then
  6, then 2 — while the eye is trying to read it. Only the height gives way now.
  Still bounded by the screen, so a width frozen on an ultrawide cannot follow
  the panel onto a laptop display.

- **Every term must match, and matching is prefix or substring only.** Typing
  more always narrows. Subsequence ("fuzzy") matching was left out: across ~60
  tiles it mostly widens the result set with matches the user cannot predict,
  which is the opposite of the point. Both the title and the detail line are
  searched, so `chrome` finds a window whose title never says so.

- **Escape unwinds one step at a time.** The query first, the panel only once
  there is no query left. Backspacing out of a long mistyped filter is not what
  anyone reaches for Escape to do.

The score is used for exactly one thing: which surviving tile the selection
starts on, and therefore what Enter takes. Best score, then the shortest title,
then whatever came first.

Arrows plus Enter came with it. A selection had to exist for Enter to mean
anything, and once it exists moving it is arithmetic — so keyboard navigation
works on the unfiltered grid too, not only while filtering.

Dragging is off while a query is active. A filtered section shows a subset in a
subset's order, and writing that back as the new order would silently drop every
pin the query hid.

## Configuring without hand-editing TOML

Two mechanisms, both cheap because Windows already provides them:

- **Pickers.** `IFileOpenDialog` pointed at `shell:AppsFolder` is a real
  installed-app browser, Store apps included, and it returns a shell item whose
  parsing name is exactly what the target model stores. So "add an app" needs no
  bespoke UI. Folder and file pickers are the same dialog with different flags.
- **Live reload.** A worker polls `flick.toml`'s mtime and posts to the panel.
  Reload re-reads config, rebinds the hotkey if it changed, and rebuilds the
  sections. No restart, which makes hand-tuning the grid bearable.

Writes go through `toml_edit`, not serde. Round-tripping through `Config` would
silently discard every comment and blank line in a file meant to be hand-edited.
A tool that eats your comments is a tool you stop hand-editing.

## Resolved

- **Hotkey: `Alt+`` `.** `Ctrl+Alt+Space` was chosen first, but `RegisterHotKey`
  reported it already registered on this machine, so `Alt+Grave` took its place:
  adjacent to Tab, so it inherits Alt+Tab muscle memory. Configurable.
- **Grid reflow: grow-then-scroll, capped at 80% of the work area.** Tile size is
  fixed from config and never changes. The grid container grows outward from
  screen center as items are added, until it hits 80% of the monitor work area in
  either axis. Columns are then capped at what fits that width, and further items
  extend downward past the height cap, which scrolls.

  Rejected: fit-to-screen tile shrinking. Tile size and position shifting with
  item count destroys the muscle memory that makes a switcher fast.

- **Column cap: 9.** `max_screen_fraction` alone is not enough on a wide monitor.
  A row of 14 tiles stops being scannable in one look, and the eye has to
  traverse instead of jump. Capped in config, on top of the fraction.

- **Tile size: 140x100, no detail line.** The first pass at 220x150 with a
  process-name second line fit only 30 tiles, against a target of 50+. At compact
  sizes the title alone identifies a tile, and the second line costs a whole row
  across the panel. `show_detail = true` restores it.

## Open questions

- Tabs and bookmarks arrive with the extension (Milestone 4). Own sections, or
  merged into existing ones, still open.

**Resolved:** where a dropped item lands with several manual sections. The one it
was dropped on. Bands cover the whole panel, so the drop point already names a
section; only the fallback needed deciding, and that is the first manual section.
