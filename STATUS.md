# Status

Where flick stands and what to pick up next.

`DESIGN.md` is the source of truth for *why*. This file is *where*.

Last updated: 2026-08-15, end of the second implementation session.

## Working

Milestones 1, 2 and 5 done, plus grouping and config UX pulled forward.

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
| Tray | Show, add app/folder/file, arrange tiles, edit settings, exit |
| Arranging | No mode. Drag past the shell's threshold to reorder, right-click to pin/unpin, drop from Explorer |
| Keep open | Pushpin button, or the right-click menu. Off by default, resets on hide |
| Safety | `asInvoker`, panic hook, watchdog, no `WH_KEYBOARD_LL` |

61 tests: layout, bands, chrome placement, drop slots, reorder arithmetic,
hotkey and colour parsing, section claiming, every config-writing path.
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

Input for those was posted, not typed. Covers everything except what the OS owns:
real capture, and the OLE drag loop.

**Those runs predate the removal of edit mode.** The write paths underneath are
unchanged. The click-versus-drag handling and the right-click menu on top of them
have never been run.

## Not verified

- **Everything since edit mode came out**: drag threshold, right-click menu,
  "Pin this app", pushpin. Compiles, passes tests, never run.
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
- Dragging moves a tile within its own section only. Moving one between sections
  is a config edit.
- Config edits need the file or the tray. No in-app settings UI.

## Next steps

In recommended order. Each is independent; pick by what is most annoying in
daily use.

**1. Live previews** (Milestone 3)

The visible upgrade: window tiles become live captures instead of icons. Needs a
sparse package for `graphicsCaptureWithoutBorder`, one-time borderless consent,
a session cap of 16-24, and frame caching with teardown on hide. The tile visual
tree already takes a `CompositionDrawingSurface`, so previews drop into the
existing structure without restructuring.

Biggest single risk left in the project. The yellow capture border is unusable at
50 tiles and suppressing it depends on package identity working as documented.

**2. Browser tabs and bookmarks** (Milestone 4)

The largest gap in coverage: tabs are a big share of what is worth switching to.
Localhost WebSocket server plus a Chromium extension. Test against a separate
Chrome profile first.

Once that channel exists, a bookmark picker is just another entry in the tray
menu, reusing the pin-writing path.

**3. Smaller things**

- Type-to-filter. The panel already takes activation normally, so keyboard input
  needs no extra plumbing, and no letter key is spoken for.
- Keyboard navigation, arrows plus Enter.
- Dragging a tile between sections. The write path exists already (remove plus
  add); the drag has to survive crossing a band boundary, which today clamps to
  the section it started in.
- Release build and a real tray icon. Currently a stock system icon.
- Auto-start entry, one of the four reversible footprint items in `DESIGN.md`.

## Open questions

- Whether tabs get their own section or merge into `Browsing`.
- Whether an in-app settings UI is worth building, given every control would be
  hand-drawn. See the rejected-alternatives reasoning in `DESIGN.md`.
