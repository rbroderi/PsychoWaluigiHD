# Psycho Waluigi HD

An HD edition of the fan game **Psycho Waluigi**, with gamepad support.

## Features

- **HD graphics**: every sprite and background is redrawn at 9x the original 320x240
  (2880x2160, which fills a 4K screen). Press F6 while playing to switch between two upscaled
  art styles, xBRZ and cleanEdge, or back to the original pixels.
- **Controller support**: play with an XInput gamepad (Xbox pads, 8BitDo pads in X-input mode,
  and similar). Every button, D-pad direction and stick direction can be mapped to any game
  action from the in-game **Controls** menu.
- **Rebindable keyboard controls**: each game action can be moved to any key. The game's own
  Configure Controls dialog is replaced by the Controls menu.
- **Full screen** in a borderless window (Alt+Enter), with the menu bar hidden if you like (F8).
- **Crash fix**: a crash in the original runtime, triggered by pausing during a level
  transition, is worked around.

This repository contains **no game files**. The installer asks for your own copy of
`psychowaluigi.exe` and builds the HD version from it on your PC.

## Requirements

- Windows 10 or 11, 64-bit
- A graphics card with Direct3D 11
- About 1.2 GB of free disk space (the HD textures)
- Your copy of `psychowaluigi.exe` (the standalone release made with Multimedia Fusion 2,
  build 251, about 30 MB)

## Installing

1. Download [`release/PsychoWaluigiHD-Setup.exe`](release/PsychoWaluigiHD-Setup.exe)
   (on the file's page, click **Download raw file**).
2. Run it. It installs for your user only, so it doesn't need administrator rights. Windows
   SmartScreen may warn that the installer is unrecognised, because it isn't signed. Choose
   **More info → Run anyway**.
3. Pick an install folder. The default is `%LOCALAPPDATA%\Programs\Psycho Waluigi HD`.
4. On the **Original game** page, browse to your `psychowaluigi.exe`. The installer checks it is
   a supported copy before going on. Your original file is not changed; it is copied into the
   install folder.
5. Click **Install**. Building the HD version takes a few minutes, and a progress bar shows the
   steps: unpacking the game's runtime, extracting its images, then upscaling them with xBRZ
   and cleanEdge. If saves (`walusave1`, `walusave2`, ...) are next to your original exe, they
   are copied over too.
6. Start **Psycho Waluigi HD** from the Start menu, or from the desktop shortcut if you chose one.

### Silent install

```
PsychoWaluigiHD-Setup.exe /VERYSILENT /GAME="D:\Games\Psycho Waluigi\psychowaluigi.exe"
```

`/DIR="..."` sets the install folder, and `/TASKS=""` skips the desktop shortcut.

## Playing

### Keyboard

The default keys are the game's own: arrow keys (move, rotate the aura, enter pipes and doors),
Shift (Button 1: jump / confirm), Ctrl (Button 2: capture / throw / cancel), Backspace
(Button 3: exit levels you have beaten), Enter (pause) and F2 (restart). To rebind any of them,
use **Controls → Keyboard**.

The HD window adds these keys:

| Key | Action |
|-----|--------|
| F6 | Switch the look: xBRZ, cleanEdge, original pixels |
| Alt+Enter | Full screen (Esc leaves it) |
| F8 | Show or hide the menu bar |
| F9 | Save screenshots (to `hd_runtime\shots`) |

### Controller

Connect an XInput gamepad before or while playing: an Xbox controller, an 8BitDo pad in its
X-input mode, or another pad that shows up as an Xbox controller. It works while the game
window has focus. The default mapping:

| Pad | Game action |
|-----|-------------|
| D-pad / left stick | Move, rotate the aura, enter pipes and doors |
| A | Button 1: jump / confirm |
| B or X | Button 2: capture / throw / cancel |
| Y | Button 4 |
| Select / Back | Button 3: exit levels you have beaten |
| Start | Pause |

**Controls → Controller** shows whether a pad is connected, turns the controller on or off
(**Use controller**) and lets you change the mapping. Any button, D-pad direction, stick
direction, shoulder button, trigger or stick click can be set to any game action, or to nothing.
**Reset controller to defaults** restores the table above. The keyboard menu has
**Reset keyboard to defaults**.

Keyboard and controller settings are saved in `hd_runtime\controls.cfg`.

## Uninstalling

Use **Settings → Apps → Psycho Waluigi HD → Uninstall**. Everything is removed except your saves
and `controls.cfg`.

## How it works

- `proxy/mmfs2_proxy` is a stand-in for the game runtime's graphics library, `mmfs2.dll`
  (32-bit). It forwards every call to the real DLL and publishes each frame (the sprite list,
  background drawing and the low-res screen) to shared memory. It also works around a crash in
  the runtime itself: pressing Enter (Pause) during a level transition makes it read freed event
  data (see `proxy/mmfs2_proxy/src/guard.rs`).
- `viewer/hd_view` (64-bit, Direct3D 11) starts the game hidden and takes its place. It redraws
  each frame at 9x with the HD textures and passes your input through to the game.
- `tools/hd_setup` is run by the installer. It unpacks the runtime and the image bank from your
  exe, writes the image fingerprints and masks the proxy needs, and makes the xBRZ set (xBRZ 6x,
  then Lanczos to 9x).
- `tools/cleanedge` makes the cleanEdge set, using a CPU port of torcado's cleanEdge shader.
- `installer/` holds the Inno Setup script and the build script.

## Building from source

You need [Rust](https://rustup.rs) with the `i686-pc-windows-msvc` target (for the 32-bit proxy),
the MSVC build tools and [Inno Setup 6](https://jrsoftware.org/isinfo.php):

```
rustup target add i686-pc-windows-msvc
winget install JRSoftware.InnoSetup
powershell -ExecutionPolicy Bypass -File installer\build_installer.ps1
```

The installer is written to `installer\output\PsychoWaluigiHD-Setup.exe`.

`proxy/mmfs2_proxy/src/thunks.rs` and `exports.def` are generated by `tools/gen_proxy.py` from
`tools/mmfs2_ordinals.tsv` and the original `mmfs2.dll`. That DLL comes from the game, so it isn't
included here; `hd_setup` unpacks it as `hd_runtime\mmfs2_orig.dll`.

## Licences

- This project: GPL-3.0 (see `LICENSE`).
- xBRZ, via the `xbrz-rs` crate: GPL-3.0.
- cleanEdge © 2022 torcado: MIT (`installer/licenses/cleanEdge-MIT.txt`).

Psycho Waluigi and its runtime belong to their respective authors. This is an unofficial fan
project that only works with a copy of the game you already have.
