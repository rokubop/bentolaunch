<img src="assets/bentolaunch-256.png" width="88" alt="">

# BentoLaunch (WIP)

A hotkey-triggered overlay in the center of your screen for all your running apps and browser tabs represented as big icons that are easily clickable. Type filtering also supported.

![The BentoLaunch panel: taskbar pins and running windows down the left, bookmarks and browser tabs down the right, and a bar of modes along the foot.](assets/preview.png)

Press `` Alt+` ``, click what you want. Type to narrow it down.

## Install

Windows 11 only. Go to the
[latest release](https://github.com/rokubop/bentolaunch/releases/latest) and
follow the install instructions there. Two files: an `.exe`, and a Chromium
browser extension for tabs and bookmarks.

## Build from source

Needs [Rust](https://rustup.rs). From **PowerShell, not WSL**: the toolchain and
the window are Windows-native.

```powershell
git clone https://github.com/rokubop/bentolaunch
cd bentolaunch
cargo build --release
target\release\bentolaunch.exe
```

`cargo build` without `--release` keeps a console window, so the log is visible.

The panel reads the config beside whichever exe started it, so an installed copy
and `target\release\` have separate settings.

## Config

`bentolaunch.toml`, beside the exe. Most of it has a square in Settings; the
rest is hand-edited, and it reloads on save. [CONFIG.md](CONFIG.md) is the
reference.

`%LOCALAPPDATA%\bentolaunch` holds the log, the paired browsers, and the config
as it was before the last **Reset layout**.

## Safety

Never elevates. Never writes to a browser profile or any other app's data. The
browser bridge is off until you switch it on, listens on loopback only, and
refuses anything that is not a paired origin with a token to match.

## Licence

MIT.
