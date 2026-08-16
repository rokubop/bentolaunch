# Status

Where DashPick stands and what to pick up next.

`DESIGN.md` is the source of truth for *why*. This file is *where*.

Last updated: 2026-08-16, end of the third implementation session.
Renamed from flick to DashPick at the end of it, then installed and merged the
sections down to two headers over four groups.

## Working

Milestones 1, 2 and 5 done, plus grouping, config UX and type-to-filter pulled
forward.

| Area | State |
|---|---|
| Hotkey | `` Alt+` ``, `RegisterHotKey`, rebinds on config save |
| Window model | `EnumWindows` + `SetWinEventHook`, MRU order, never scans on the hotkey |
| Grid | Composition visual tree, sections with headers, grow-then-scroll |
| Density | 140x100 tiles, 9 column cap, ~60 visible on 1080p |
| Grouping | Groups inside a section: `source = ["windows", "tabs"]`, each entry with its own `match`. Two sections, four groups by default. Claimed in order, each window once |
| Icons | `IShellItemImageFactory` on 2 MTA workers, cached, never blocks the UI |
| Activation | Focus for windows, `ShellExecuteW` for everything else |
| Targets | Paths, folders, `.lnk`, Store apps, `ms-settings:`, `https:`, browser tabs |
| Taskbar pins | Read from the `User Pinned\TaskBar` `.lnk` folder |
| Config | Live reload on save, tray pickers write pins via `toml_edit` |
| Tray | Show, add app/folder/file, edit settings, exit |
| Filtering | Type to narrow. 72px strip shows the query and "3 of 47", its text sized from its own height. Width frozen for the query's duration |
| Keyboard | Arrows move a selection, Enter takes it, Home/End jump. Esc clears the query, then hides |
| Tabs | Loopback WebSocket, MV3 extension. Favicons deduped by origin. Own group in `Active`, right behind the browser windows. Off until enabled and paired |
| Banding | Alternate groups take `theme.tile_alt`. The only cue left once a group lost its header; a rule or a spacer would break the uniform grid |
| Arranging | No mode. Drag past the shell's threshold to reorder, right-click to pin/unpin, drop from Explorer. Off while filtering |
| Keep open | Pushpin button, or the right-click menu. Off by default, resets on hide. **Slated for removal, see below** |
| Safety | `asInvoker`, panic hook, watchdog, no `WH_KEYBOARD_LL` |

97 tests: layout, bands, chrome placement, drop slots, reorder arithmetic,
hotkey and colour parsing, section claiming, every config-writing path, match
ranking, the frozen-width and search-strip geometry, and the socket gate both
as a unit and against a real listener.
`cargo clippy --all-targets` clean.

## Verified on the real machine

- Enumeration matched the actual window list, no shell junk, no cloaked UWP.
- Every `focused` / `launched` log line matched the resulting foreground window.
- Live reload applied a tile size change and a hotkey swap in ~3s, no restart.
- Icons resolve for exes, folders, `ms-settings:` and `https:`.
- The tray pickers, end to end.
- Dragging a manual tile rewrote that section's `items` in the new order, kept a
  `{ title, target }` entry's title, reloaded live.
- Dragging a taskbar tile wrote an `order` list of pin names and applied it.
- Unpinning removed that entry and left the rest alone.

Type-to-filter, driven end to end against a dry-run copy in a scratch directory
(its own `dashpick.toml`, its own hotkey, so the real instance was untouched):

- `s` → 9 of 10, `st` → 4, `sto` → 1. Backspace walked it back to 4.
- Panel went 926x290 → 926x362 the moment a query existed and back on clearing,
  the 72px strip exactly. Width and column count did not move across any
  keystroke.
- Enter after `sou` logged `would open "Sound" -> ms-settings:sound`.
- Down, Right, Enter with no query at all took the second tile.
- End then Enter took the last tile, a live window.
- A query matching nothing left the panel at full width with Enter inert, and
  Escape cleared it before the second Escape hid the panel.

The browser bridge, against a stand-in extension (a PowerShell `ClientWebSocket`
setting its own `Origin`):

- Paired origin plus token connected, sent 4 tabs, and they became tiles:
  `show: 5 items in 2 section(s)`.
- Typing `zombo` narrowed 5 to 1; Enter logged
  `asked the browser to switch to "Zombocom"` and the client received
  `{"type":"focus","tabId":104,"windowId":2}`.
- DashPick's 20s ping arrived and the client's pong came back. That heartbeat is
  what keeps an MV3 service worker alive.
- Refused, all with the correct token: a `https://` page origin, an unpaired
  extension id. Refused with the paired origin: a wrong token.
- First run with `enabled = true` generated a token and wrote it back through
  `toml_edit`, leaving the rest of the file intact.
- A client killed mid-connection was logged and cleaned up, and its tabs left
  the grid.

**Then against real Chrome**, which settled the three unknowns:

- The MV3 worker does send `Origin: chrome-extension://<id>`, and a loopback
  WebSocket needs no host permissions.
- `AllowSetForegroundWindow` plus `chrome.windows.update` raises the window.
  Clicking a tab tile switches to it. This was the riskiest guess in the design.
- Favicons arrive and paint. `_favicon` needs no network access from DashPick.

Input for those was posted, not typed. Covers everything except what the OS owns:
real capture, and the OLE drag loop.

**Those runs predate the removal of edit mode.** The write paths underneath are
unchanged. The click-versus-drag handling and the right-click menu on top of them
have never been run.

## Not verified

- **Everything since edit mode came out**: drag threshold, right-click menu,
  "Pin this app", pushpin. Compiles, passes tests, never run.
- Two browsers connected at once. One connection is all that has ever run.
- **What the filter strip looks like.** Its geometry is verified from the logged
  panel size and its draw call reports no error, but the pixels have never been
  seen — `WS_EX_NOREDIRECTIONBITMAP` means nothing can read them back off a
  screen DC. Same constraint as every other visual in the app.
- **Dropping from Explorer.** Needs a real cross-process mouse drag; cannot be
  posted. Everything under it is tested: the drop target answers, the
  section-under-cursor rule, the same `add` path the pickers use. Pin the panel
  open first, then drag a folder onto it.
- Past ~60 tiles, and scrolling with many sections.
- Real hotkey presses, as opposed to a posted `WM_HOTKEY`. Posting shows the
  panel but grants no foreground rights, so it sometimes self-dismisses within a
  few hundred ms. Capture within ~250ms. Screenshots must come from the OS
  (PrtScn): the panel is `WS_EX_NOREDIRECTIONBITMAP`, so `BitBlt` off the screen
  DC reads nothing.
- A second monitor at a different DPI. Scale is read per show; a mid-session
  change is untested.

## Security

`DESIGN.md`, "Who is allowed on the socket", has the reasoning. What stands:

- Off by default. Loopback only, never the unspecified address. Half a
  configuration refuses to listen.
- Origin allowlist is the gate that matters. A page cannot forge its origin, so
  a site cannot enumerate your tabs. Verified against a live listener with the
  correct token.
- Capped past the gate: 4 MiB messages, 1 MiB frames, 8 connections, 2000 tabs.
- A client can set what DashPick shows and receive focus commands. Nothing reaches
  `ShellExecuteW` from a tab's title or URL.
- `dashpick.toml` is gitignored, so the token is not in the repo.
- The token lives in `%LOCALAPPDATA%\dashpick\bridge-token`, not beside the exe.
  A portable build can land in `Program Files`, where a file next to it is
  readable by every account on the machine. Migrated out of `dashpick.toml` on
  startup.

Live concerns, none urgent:

- **The panic hook is global.** A panic on a socket thread calls `neutralize`
  and disables the panel until restart. Socket code is `Result`/`Option`
  throughout, but nothing enforces that. Catching panics per connection would
  close it.
- **The token is weaker than it reads.** Readable by anything running as you.
  It stops other accounts and blind attempts only.
- **The extension can read every tab title and URL.** Inherent to the feature.
  The mitigation is that `worker.js` is ~150 lines and yours.

## Known gaps

- Window tiles show icons, not live previews.
- No bookmarks yet. Same channel, not built.
- Firefox needs its own extension build.
- A filtered grid cannot be rearranged. Deliberate — see `DESIGN.md` — but it
  means a section has to be fully on screen to reorder it.
- Dragging moves a tile within its own group only. Moving one between sections,
  or across a seam inside a merged section, is a config edit.
- Banding is the only cue between groups. It is subtle by design, and two
  adjacent groups on one row read as one block until you look. A divider needs
  either a broken row or a spacer column — neither is free.
- Config edits need the file or the tray. No in-app settings UI.

## Outside the repo

Folder renamed to `R:\dev\dashpick` on 2026-08-16. The extension was re-loaded
from the new path and the bridge reconnects; the old flick id is out of
`browser.allow`.

The installed copy — what actually runs day to day — is separate from the build
tree:

| | |
|---|---|
| `%LOCALAPPDATA%\Programs\dashpick\` | `dashpick.exe`, `dashpick.ico`, `dashpick.toml` |
| `%APPDATA%\...\Start Menu\Programs\Startup\DashPick.lnk` | autostart, delete to reverse |
| `%LOCALAPPDATA%\dashpick\` | `bridge-token`, `dashpick.log` — shared by every build |

Installing a new build is a copy of the exe. It does not touch the config
already there, so pins survive. The config the panel reads is the one beside
whichever exe is running: the installed one edits
`%LOCALAPPDATA%\Programs\dashpick\dashpick.toml`, a `cargo run` edits
`target\debug\dashpick.toml`. Two separate files, easy to confuse.

## Next steps

Session two's order, minus type-to-filter, which was step 2 and is now done.

**1. Drop the pushpin and drop-to-pin**

Roku's call: the keep-open pin is not worth it. It exists only to serve Explorer
drops, so both go together. Adding stays covered by right-click "Pin this app",
the tray pickers, and hand-editing.

Removes: `dropzone.rs`, the pushpin, `grid::chrome` and its 3 tests,
`OleInitialize` back to `CoInitializeEx`. ~200 lines.

**Test one thing first.** Does `WM_HOTKEY` fire while another process owns a drag
loop? If it does, the flow is: drag a file, press the hotkey, panel appears
without stealing focus (`SWP_NOACTIVATE`), drop. That is better than keep-open
ever was and would save the drop target. ~10 minutes to answer. If it does not
fire, delete both.

**2. Finish Milestone 4**

Tabs work end to end in Chrome. What is left:

- **Bookmarks.** Same channel, `chrome.bookmarks`. A bookmark picker is another
  tray entry over the existing pin-writing path.
- **"Bookmark this tab"** in the right-click menu, over the same path.

**3. Live previews** (Milestone 3)

Window tiles become live captures. Needs a sparse package for
`graphicsCaptureWithoutBorder`, one-time borderless consent, a session cap of
16-24, frame caching with teardown on hide. The tile tree already takes a
`CompositionDrawingSurface`, so previews drop in without restructuring.

Still the biggest single risk in the project. The yellow capture border is
unusable at 50 tiles and suppressing it depends on package identity behaving as
documented.

**4. Smaller things**

- Page Up / Page Down, and letting the arrows wrap across a row edge rather than
  clamping. Arrows, Enter and Home/End landed with type-to-filter.
- Dragging a tile between sections. The write path exists already (remove plus
  add); the drag has to survive crossing a band boundary, which today clamps to
  the section it started in.
- A real tray icon. Currently a stock system icon. `rc.exe` is not on this
  machine either, so the exe has no icon of its own — only the Startup shortcut
  points at `dashpick.ico`. Both wait on the same missing tool.
- Auto-start is **done**, as a Startup-folder shortcut — the reversible footprint
  item in `DESIGN.md`. Not yet a tray toggle; adding or removing it is manual.

## Open questions

- Does `WM_HOTKEY` fire during another process's drag loop? Decides whether
  drop-to-pin survives. See step 1 above.
- **Resolved:** tabs keep their own section, placed directly after `Browsing`.
  Roku's call, one group for browser things. Merging them into one section is
  still open, and would need a section to take more than one `source`.
- Whether the score should also weigh recency, so `ch` lands on the Chrome window
  used a minute ago rather than the shortest-titled one. Deferred until there are
  enough tiles for it to matter.
- Whether an in-app settings UI is worth building, given every control would be
  hand-drawn. See the rejected-alternatives reasoning in `DESIGN.md`.
