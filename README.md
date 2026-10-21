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

## Command line

Use one command per invocation (values accept both `--option value` and
`--option=value`):

```text
candle-tray.exe --set 50 [--display <selector>]
candle-tray.exe --offset -10 [--display <selector>]
candle-tray.exe --contrast 70 [--display <selector>]
candle-tray.exe --profile "Night"
candle-tray.exe --off
candle-tray.exe --input HDMI1 --display <selector>
candle-tray.exe --show
```

The selector defaults to `all`: all visible displays. It can also be a 1-based
position in the popup's configured display order, an exact display ID, or a
case-insensitive substring of the display name (including a custom name).
An exact ID wins over a name match; a substring selects every matching display.
Hidden displays are excluded. Brightness and contrast values are 0?100;
offsets are signed integer percentage points, clamped to 0?100 per display.
Brightness uses the popup slider scale, including configured min/max mapping
and SDR white level while HDR is active. Contrast requires per-display opt-in.
Unavailable controls/displays are skipped and logged.

Profiles accept a case-insensitive full name or a 1-based profile number.
Duplicate names are rejected. `--off` uses the configured power-off method.
Input switching requires an explicit selector and accepts `HDMI1`, `HDMI2`,
`DP1`, `DP2`, `USB-C`, or a nonzero hexadecimal VCP 0x60 value such as `0x11`.
The display must support DDC/CI input switching; raw values are forwarded even
when absent from its advertised input list.

Twinkle Tray spellings `--MonitorNum=2`, `--MonitorID=<exact-id>`, `--All`,
`--Set=50`, and `--Offset=-10` are also accepted, with either value form.
For example: `candle-tray.exe --MonitorNum 2 --Offset -10`.

Commands are sent to the running instance using a JSON `WM_COPYDATA` payload;
they do not open the popup unless `--show` is used. A plain second launch still
opens the popup. If Candle Tray is not running, it starts normally and queues
hardware commands until the first monitor snapshot. Unknown arguments, invalid
values, conflicting commands/selectors, and forwarding failures exit nonzero.
Execution is asynchronous: successful forwarding acknowledges queueing, not
hardware completion. Selection/control failures are written to
`%APPDATA%\Candle Tray\log.txt` and do not change the sender's exit status.

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
