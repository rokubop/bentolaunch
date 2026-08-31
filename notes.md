## What's new

- **The center starts empty.** Eighteen blank squares in the middle of a fresh
  panel was the best place on it spent on nothing. Right-click anything and
  choose **Add to Center**, or use **Edit center**. The block appears around the
  first one and grows by what it needs: one favorite is one square, five are
  3 x 2. Empty it and it goes away again.
- **Reset layout**, a square in Settings. Boxes, grid and block back to stock.
  Your hotkey, theme, hand-added apps, pin order and the block's own lists come
  through untouched. It asks first, on a surface of its own, and the old file is
  copied to `%LOCALAPPDATA%\bentolaunch\bentolaunch.toml.bak`.
- **The bar at the foot stays put** once the grid is long enough to scroll. It
  used to go off the bottom with everything else, which is the one row aimed at
  by position.
- **Move window is one bar, not two.** The six moves take the modes bar's row
  rather than stacking under it, and the square leading them finishes the mode.
  "Stay open" is gone: switching it off left you in a mode that did nothing.
- **Favorites is Center**, everywhere: the square, the menu, the config. The
  right-click menu says where a tile goes now - **Add to Launch**, **Add to
  Center**, **Remove from Launch**.
- **New marks**, all from one icon set. The folder and the file were the same
  blank rectangle before.
- **The block's size moved to Edit layout**, where the block is on screen next
  to the four squares that size it.

## Breaking

The config format changed and nothing carries over. Either delete
`bentolaunch.toml` and let a fresh one be written, or by hand:

- `[favorites]` is `[center]`
- `source = "favorites"` is `source = "center"`
- `split`, `at`, `browser.allow` and `browser.token` are gone
- `bentopick.toml` and `%LOCALAPPDATA%\bentopick` are no longer read
