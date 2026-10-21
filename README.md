# Candle Tray

Candle Tray is a Windows 10/11 monitor brightness utility written in Rust with
GPUI, and a rewrite of [Twinkle Tray](https://github.com/xanderfrangos/twinkle-tray)
by Xander Frangos. Candle Tray is licensed under the MIT license.

## Features

- Monitor brightness through DDC/CI and WMI.
- SDR white level for HDR displays.
- Linked brightness levels, display power-off, and level restoration.
- Level profiles, and global shortcuts for profiles, brightness steps and
  turning displays off. Scrolling over the tray icon changes brightness.
- A time-of-day schedule (clock times, sunrise and sunset) with smooth
  transitions, and dimming after a period without input.
- Input source switching for DDC/CI displays (VCP 0x60).
- General and per-monitor settings that apply live.
- Twinkle Tray settings import.
- English and Russian interfaces.

## Install

Download an x64 or ARM64 installer or portable ZIP from
[Releases](https://github.com/vertopolkaLF/candle-tray/releases).
The installer uses `%LOCALAPPDATA%\Programs\Candle Tray` for the current user
and creates a Start Menu shortcut. For the portable version, extract the ZIP
and run `candle-tray.exe`; no separate asset folder is required.
Uninstalling preserves your settings silently.

## Build

Install Rust 1.95.0 and Visual Studio C++ Build Tools with the Windows SDK.
ARM64 packaging also needs the ARM64 C++ tools and Clang (used by dependencies).
Run these commands in PowerShell from the repository root:

```powershell
$env:CARGO_HTTP_CHECK_REVOKE = "false"
.\cargo-worktree.ps1 check --locked
.\cargo-worktree.ps1 fmt --package candle-tray --check
.\cargo-worktree.ps1 clippy --all-targets --locked '--' -D warnings
.\cargo-worktree.ps1 build --release --locked
```

The wrapper reuses the main checkout's `target/` directory in Git worktrees.
To create release packages in `dist/<version>/`:

```powershell
.\build.ps1 -Arch x64,arm64
.\build.ps1 -Arch x64 -SkipInstaller
```

NSIS 3.11 is used from PATH, the standard installation directory, or `.tools/`.
If absent, the script downloads its ZIP using curl's `--ssl-no-revoke` to work
around this machine's Schannel revocation lookup failures; TLS certificate
verification remains enabled. The portable-only command does not require NSIS.
Regenerate the committed icons offline with `.\tools\generate-icon.ps1` once
Cargo dependencies have been cached. The renderer uses the original tray SVG.

CI checks formatting on Ubuntu and runs Clippy and tests on Windows. Manually
run the Release workflow to build both architectures and create a draft GitHub
release with four assets; publication is a separate action.

## Settings and import

Settings are stored in `%APPDATA%\Candle Tray\settings.toml`, including when
running the portable executable. Use the Twinkle Tray settings import offered
by the app to bring over supported settings from an existing installation.

See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for acknowledgements.
