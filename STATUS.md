# Status

Where flick stands and what to pick up next.

`DESIGN.md` is the source of truth for *why*. This file is *where*.

Last updated: 2026-08-15, end of the third implementation session.

## Working

Milestones 1, 2 and 5 done, plus grouping, config UX and type-to-filter pulled
forward.

| Area | State |
|---|---|
| Hotkey | `` Alt+` ``, `RegisterHotKey`, rebinds on config save |
| Window model | `EnumWindows` + `SetWinEventHook`, MRU order, never scans on the hotkey |
| Grid | Composition visual tree, sections with headers, grow-then-scroll |
| Density | 140x100 tiles, 9 column cap, ~60 visible on 1080p |
| Grouping | `match` rules per section, first section claims a window |
| Icons | `IShellItemImageFactory` on 2 MTA workers, cached, never blocks the UI |
| Activation | Focus for windows, `ShellExecuteW` for everything else |
| Targets | Paths, folders, `.lnk`, Store apps, `ms-settings:`, `https:` |
| Taskbar pins | Read from the `User Pinned\TaskBar` `.lnk` folder |
| Config | Live reload on save, tray pickers write pins via `toml_edit` |
| Tray | Show, add app/folder/file, edit settings, exit |
| Filtering | Type to narrow. 72px strip shows the query and "3 of 47", its text sized from its own height. Width frozen for the query's duration |
| Keyboard | Arrows move a selection, Enter takes it, Home/End jump. Esc clears the query, then hides |
| Arranging | No mode. Drag past the shell's threshold to reorder, right-click to pin/unpin, drop from Explorer. Off while filtering |
| Keep open | Pushpin button, or the right-click menu. Off by default, resets on hide. **Slated for removal, see below** |
| Safety | `asInvoker`, panic hook, watchdog, no `WH_KEYBOARD_LL` |

79 tests: layout, bands, chrome placement, drop slots, reorder arithmetic,
hotkey and colour parsing, section claiming, every config-writing path, match
ranking, and the frozen-width and search-strip geometry.
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
(its own `flick.toml`, its own hotkey, so the real instance was untouched):

- `s` → 9 of 10, `st` → 4, `sto` → 1. Backspace walked it back to 4.
- Panel went 926x290 → 926x362 the moment a query existed and back on clearing,
  the 72px strip exactly. Width and column count did not move across any
  keystroke.
- Enter after `sou` logged `would open "Sound" -> ms-settings:sound`.
- Down, Right, Enter with no query at all took the second tile.
- End then Enter took the last tile, a live window.
- A query matching nothing left the panel at full width with Enter inert, and
  Escape cleared it before the second Escape hid the panel.

Input for those was posted, not typed. Covers everything except what the OS owns:
real capture, and the OLE drag loop.

**Those runs predate the removal of edit mode.** The write paths underneath are
unchanged. The click-versus-drag handling and the right-click menu on top of them
have never been run.

## Not verified

- **Everything since edit mode came out**: drag threshold, right-click menu,
  "Pin this app", pushpin. Compiles, passes tests, never run.
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

## Known gaps

- Window tiles show icons, not live previews.
- No browser tabs or bookmarks.
- A filtered grid cannot be rearranged. Deliberate — see `DESIGN.md` — but it
  means a section has to be fully on screen to reorder it.
- Dragging moves a tile within its own section only. Moving one between sections
  is a config edit.
- Config edits need the file or the tray. No in-app settings UI.

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

**2. Browser tabs and bookmarks** (Milestone 4)

The largest gap in coverage, and where Roku wants to go: every tab in every
Chrome window. Unblocked now that filtering exists.

`chrome.tabs.query({})` returns them all with title, url, favIcon, windowId.
Switching is `chrome.tabs.update(id, {active:true})` plus
`chrome.windows.update(windowId, {focused:true})`. Localhost WebSocket server
here, extension connects. Test against a separate Chrome profile first.

Rejected again, for the record: UI Automation on the tab strip gives titles but
not URLs and turns on Chrome's accessibility mode; DevTools port 9222 needs
Chrome launched with a flag; profile files are locked and off limits.

Bookmarks are the same channel, and a bookmark picker is another tray entry over
the existing pin-writing path.

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
- Release build and a real tray icon. Currently a stock system icon.
- Auto-start entry, one of the four reversible footprint items in `DESIGN.md`.

## Open questions

- Does `WM_HOTKEY` fire during another process's drag loop? Decides whether
  drop-to-pin survives. See step 1 above.
- Whether tabs get their own section or merge into `Browsing`. Leaning own
  section, since filtering is how anyone will reach a specific tab.
- Whether the score should also weigh recency, so `ch` lands on the Chrome window
  used a minute ago rather than the shortest-titled one. Deferred until there are
  enough tiles for it to matter.
- Whether an in-app settings UI is worth building, given every control would be
  hand-drawn. See the rejected-alternatives reasoning in `DESIGN.md`.
