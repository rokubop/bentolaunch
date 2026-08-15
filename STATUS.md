# Status

Where flick stands and what to pick up next.

`DESIGN.md` is the source of truth for *why*. This file is *where*.

Last updated: 2026-08-15, end of the first implementation session.

## Working

Milestones 1 and 2 done, plus grouping and config UX pulled forward.

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
| Safety | `asInvoker`, panic hook, watchdog, no `WH_KEYBOARD_LL` |

42 tests. Layout, hotkey parsing, colour parsing, section claiming, and
config-writing are all covered. `cargo clippy --all-targets` is clean.

## Verified on the real machine

- Enumeration matched the actual window list, no shell junk, no cloaked UWP.
- Every `focused` / `launched` log line matched the resulting foreground window.
- Live reload applied a tile size change and a hotkey swap in ~3s, no restart.
- Icons resolve for exes, folders, `ms-settings:` and `https:`.

## Not verified

- **The three tray pickers.** Modal dialogs need real input, so they were never
  exercised end to end. The `toml_edit` write path underneath them is tested. Try
  "Add app…" from the tray first thing.
- Behaviour past ~60 tiles, and scrolling with many sections.
- Anything on a second monitor with a different DPI. The scale factor is read per
  show, but a mid-session DPI change is untested.

## Known gaps

- Taskbar pin order is alphabetical, not the taskbar's. The real order is an
  undocumented registry blob. A manual section gives exact control.
- Window tiles show icons, not live previews.
- No browser tabs or bookmarks.
- No drag-and-drop, no reordering, no edit mode.
- Config edits need the file or the tray. No in-app settings UI.

## Next steps

In recommended order. Each is independent; pick by what is most annoying in
daily use.

**1. Edit mode + drop-to-pin** (Milestone 5, moved up)

The natural completion of the config work already done. Edit mode first, because
without an explicit mode a slightly-dragged click reorders the grid when the user
meant to switch. Then `IDropTarget` so a file or folder dragged from Explorer
pins itself, and drag-to-reorder for taskbar and manual sections only.

Blockers to solve: dismissal must be suspended while a drag is in flight, and
reorder needs a persisted order that does not fight MRU on window tiles.

**2. Live previews** (Milestone 3)

The visible upgrade: window tiles become live captures instead of icons. Needs a
sparse package for `graphicsCaptureWithoutBorder`, one-time borderless consent,
a session cap of 16-24, and frame caching with teardown on hide. The tile visual
tree already takes a `CompositionDrawingSurface`, so previews drop into the
existing structure without restructuring.

Biggest single risk left in the project. The yellow capture border is unusable at
50 tiles and suppressing it depends on package identity working as documented.

**3. Browser tabs and bookmarks** (Milestone 4)

The largest gap in coverage: tabs are a big share of what is worth switching to.
Localhost WebSocket server plus a Chromium extension. Test against a separate
Chrome profile first.

Once that channel exists, a bookmark picker is just another entry in the tray
menu, reusing the pin-writing path.

**4. Smaller things**

- Type-to-filter. The panel already takes activation normally, so keyboard input
  needs no extra plumbing.
- Keyboard navigation, arrows plus Enter.
- Release build and a real tray icon. Currently a stock system icon.
- Auto-start entry, one of the four reversible footprint items in `DESIGN.md`.

## Open questions

- Where a dropped item lands when several manual sections exist.
- Whether tabs get their own section or merge into `Browsing`.
- Whether an in-app settings UI is worth building, given every control would be
  hand-drawn. See the rejected-alternatives reasoning in `DESIGN.md`.
