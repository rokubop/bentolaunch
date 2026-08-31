<img src="assets/bentolaunch-256.png" width="88" alt="">

![Version](https://img.shields.io/github/v/release/rokubop/bentolaunch?label=version&color=blue)
![Status](https://img.shields.io/badge/status-experimental-orange)
![License](https://img.shields.io/badge/license-MIT-green)

# BentoLaunch

A hotkey-triggered overlay in the center of your screen for all your running apps and browser tabs represented as big icons that are easily clickable. Type filtering also supported.

![The BentoLaunch panel: taskbar pins and running windows down the left, bookmarks and browser tabs down the right, and a bar of modes along the foot.](assets/preview.png)

Press `` Alt+` ``, click what you want. Type to narrow it down.

## Install

Windows 11 only. Go to the
[latest release](https://github.com/rokubop/bentolaunch/releases/latest) and
follow the install instructions there. Two files: an `.exe`, and a Chromium
browser extension for tabs and bookmarks.

## Build from source

If you want to build it yourself instead of downloading from the releases: requires [Rust](https://rustup.rs) and **PowerShell, not WSL**.

```powershell
git clone https://github.com/rokubop/bentolaunch
cd bentolaunch
cargo build --release
target\release\bentolaunch.exe
```

`cargo build` without `--release` keeps a console window, so the log is visible.

Reads the config next to the `exe` location, so an installed copy
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

## More from me
My other software and Talon packages for UI, mouse control, input mapping,
parrot and gaming are at
[talon-hub-roku](https://github.com/rokubop/talon-hub-roku).
