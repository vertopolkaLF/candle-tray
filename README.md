<p align="center">
  <img src="assets/app-icon.png" alt="Candle Tray logo" width="256" height="256">
</p>

<h1 align="center">Candle Tray</h1>

<p align="center">
  <b>Free, open-source Windows tray app for monitor brightness with DDC/CI and WMI control, HDR white level, profiles, global shortcuts, a time-of-day schedule, adaptive brightness, and in-place updates.</b>
</p>

<p align="center">
  <a href="https://github.com/vertopolkaLF/candle-tray/releases">Download</a>
  |
  <a href="https://github.com/vertopolkaLF/candle-tray/issues">Issues</a>
</p>

<p align="center">
  <img src="https://img.shields.io/github/downloads/vertopolkalf/candle-tray/total?style=flat-square" alt="Downloads">
  <img src="https://img.shields.io/badge/platform-Windows%2010%20%2F%2011-blue?style=flat-square" alt="Platform">
  <img src="https://img.shields.io/badge/Rust-2024-orange?style=flat-square" alt="Rust edition">
  <img src="https://img.shields.io/badge/UI-GPUI-green?style=flat-square" alt="UI framework">
</p>

---

## Overview

Candle Tray puts a brightness slider for every display in the Windows notification area. It is a
native Windows application written in Rust, with the tray popup and Settings both rendered by
GPUI, and a rewrite of [Twinkle Tray](https://github.com/xanderfrangos/twinkle-tray) by
Xander Frangos.

<p align="center">
  <img src="docs/media/popup.png" alt="Candle Tray popup with brightness and contrast sliders for three displays">
</p>

## Features

### Every display in one popup

Click the tray icon for brightness sliders for every display: external monitors through DDC/CI
and laptop panels through WMI. Optional contrast sliders (VCP 0x12) and input source buttons
(VCP 0x60) can be turned on per display. HDR displays get an HDR toggle and an SDR white level
slider. Link levels to move every display together, or turn the displays off from the popup.
Scrolling over the tray icon changes brightness too.

### Per-display settings

Rename or hide displays, map the slider through a calibrated brightness curve with a hardware minimum and maximum, give inputs your
own names, and set a vendor brightness VCP code for displays that need one. Monitor volume (VCP 0x62) and additional hexadecimal VCP controls can also be enabled per display. Calibration accepts increasing slider:hardware points, such as `25:10, 50:30, 75:60`. Empty calibration uses a linear min/max range. Every setting
applies live.

<p align="center">
  <img src="docs/media/settings-monitors.png" alt="Monitors settings page">
</p>

### Profiles

Save levels for every display as a profile with its own icon and switch to it from the
popup's bottom bar or with a global shortcut. Profiles can also be applied automatically while
an app is in front, or when the computer switches between battery and mains power.

<p align="center">
  <img src="docs/media/settings-profiles.png" alt="Profiles settings page">
</p>

### Automation

- A time-of-day schedule with clock times, dawn, sunrise, solar noon, golden hour, sunset start, sunset, dusk and night (with location detection), offsets and smooth transitions.
- Independent media and fullscreen idle exclusions, optional pause while the session is locked, and a manual override with a Resume automation control.
- Full-path or executable-name foreground matching, optional closed-lid panel hiding and per-display restoration exclusions.
- Dimming after a period without input.
- Adaptive brightness from an ambient light sensor.

### Look and feel

Light, dark or system theme, Acrylic, Mica or solid popup material, and a choice of tray
glyphs. English and Russian interfaces.

<p align="center">
  <img src="docs/media/settings-general.png" alt="General settings page">
</p>

### More features

- Global shortcuts for brightness steps, profiles and turning displays off.
- Restoring levels after sleep and display reconnection.
- Command-line control for scripts (see [Command line](#command-line)).
- Twinkle Tray settings import.
- Start with Windows and update in place from GitHub Releases.

## Requirements

- Windows 10 or Windows 11 (x64, ARM64 or x86).
- External displays need DDC/CI enabled in their on-screen menu for hardware brightness.

## Install

1. Open the [latest release](https://github.com/vertopolkaLF/candle-tray/releases/latest).
2. Download the installer matching your Windows architecture (`x64`, `arm64` or `x86`) and
   run it. The installer is per-user, uses `%LOCALAPPDATA%\Programs\Candle Tray` and creates a
   Start Menu shortcut. Uninstalling preserves your settings silently.
3. Alternatively, download the matching portable ZIP, extract it, and run `candle-tray.exe`;
   no separate asset folder is required.
4. Find the icon in the notification area. If it is hidden, Windows may have tucked it under
   the `^` overflow menu.

To bring over an existing setup, use the Twinkle Tray settings import offered by the app.

## Updating

Candle Tray checks for a new release at startup and every hour (Settings → About can turn this
off). A Windows notification shows the app icon and **Install update** and **View Release**
buttons. Choose **Install update**, or **Update** in the flyout or on the About page, to
download the portable ZIP for your architecture, verify its SHA-256 and restart into the new
version. A notification confirms the successful update after relaunch. Both the installed
and the portable version update this way.

<p align="center">
  <img src="docs/media/settings-about.png" alt="About page with update controls">
</p>

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
candle-tray.exe --List
candle-tray.exe --VCP=0x62:35 --MonitorNum=1 --Overlay
candle-tray.exe --Set=50 --Panel
```

The selector defaults to `all`: all visible displays. It can also be a 1-based
position in the popup's configured display order, an exact display ID, or a
case-insensitive substring of the display name (including a custom name).
An exact ID wins over a name match; a substring selects every matching display.
Hidden displays are excluded. Brightness and contrast values are 0–100;
offsets are signed integer percentage points, clamped to 0–100 per display.
Brightness uses the popup slider scale, including configured min/max mapping
and SDR white level while HDR is active. Contrast requires per-display opt-in.
Unavailable controls/displays are skipped and logged.

Profiles accept a case-insensitive full name or a 1-based profile number.
Duplicate names are rejected. `--off` uses the configured power-off method.
Input switching requires an explicit selector and accepts `HDMI1`, `HDMI2`,
`DP1`, `DP2`, `USB-C`, or a nonzero hexadecimal VCP 0x60 value such as `0x11`.
The display must have a DDC/CI connection. Explicit CLI/action input writes accept any nonzero byte, including vendor values missing from its advertised input list. Input chips in the popup still respect the configured visible sources.

Settings → Shortcuts also supports named action shortcuts. Each shortcut stores
an ordered sequence with its own display selection, control and operation per
action. Controls are shared-slider brightness, explicit SDR white level,
contrast, raw VCP, input source and DDC power. Operations are set, signed offset
and cycle through unique values. Cycles choose the next value after the current
value, wrap at the end, and start at the first value when the current value is
not listed. Failed/invalid actions do not advance a cursor or overwrite saved
levels. Values are evaluated by the monitor worker after earlier actions finish.
Each display preserves action order; different displays execute independently.
Hidden/unresponsive displays and unsupported controls are skipped; powered-off
displays accept only power actions until switched on. Explicit raw VCP actions can address unadvertised codes on DDC displays. Raw 0x60/0xD6 use the typed input/power
validation rules. Raw values are 0–65535, input values are nonzero
bytes, and power values are 1 (on), 2 (standby), 3 (suspend), 4 (DPM off), 5 (off).

The same sequences can be sent through `--actions '<JSON array>'`. Each entry
has `selector`, `control` and `operation`; selectors are `"All"`, `{"Id":"id"}`,
`{"Position":1}` or `{"NameOrId":"Desk"}`. Controls are `"Brightness"`, `"Sdr"`,
`"Contrast"`, `{"Vcp":18}`, `"Input"` or `"Power"`. Operations are `{"Set":50}`,
`{"Offset":-5}` or `{"Cycle":[25,50,100]}`. For example, in PowerShell:

```powershell
candle-tray.exe --actions '[{"selector":"All","control":"Brightness","operation":{"Cycle":[25,50,100]}}]'
```

Existing shortcut slots, profile shortcuts, CLI spellings and configuration
files remain supported. Percentage actions use the normalized slider scale;
direct monitor setters still take hardware percentages. Successful worker
acknowledgements update remembered levels without requiring a restart.

Twinkle Tray spellings `--MonitorNum=2`, `--MonitorID=<id-or-substring>`, `--All`,
`--Set=50`, and `--Offset=-10` are also accepted, with either value form. `--MonitorID` prefers an exact ID, then selects case-insensitive ID substring matches.
For example: `candle-tray.exe --MonitorNum 2 --Offset -10`.

Commands are sent to the running instance using a JSON `WM_COPYDATA` payload;
they do not open the popup unless `--show` or `--Panel` is used. `--Overlay` requests a brief non-activating OSD after a successful level action. The level indicator for shortcuts and tray scrolling is under Shortcuts > Brightness; scroll inversion is under Shortcuts > Mouse. DDC power values (4, 5, or 4 then 5) are configured under Shortcuts > Power and appear only when DDC/CI or Both is selected. A plain second launch still
opens the popup. If Candle Tray is not running, it starts normally and queues
hardware commands until the first monitor snapshot. Unknown arguments, invalid
values, conflicting commands/selectors, and forwarding failures exit nonzero.
`--List` is a read-only one-shot query that prints a JSON display inventory, including hidden displays. It creates no GPUI window or normal app instance, and may take up to 25 seconds while hardware is queried. Its brightness field is hardware percent; it does not acknowledge queued writes in another instance.

Execution is asynchronous: successful forwarding acknowledges queueing, not
hardware completion. Selection/control failures are written to
`%APPDATA%\Candle Tray\log.txt` and do not change the sender's exit status.

## Build from source

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
.\build.ps1 -Arch x64,arm64,x86
.\build.ps1 -Arch x64 -SkipInstaller
```

NSIS 3.11 is used from PATH, the standard installation directory, or `.tools/`.
If absent, the script downloads its ZIP using curl's `--ssl-no-revoke` to work
around this machine's Schannel revocation lookup failures; TLS certificate
verification remains enabled. The portable-only command does not require NSIS.
Regenerate the committed icons from `assets/app-icon.svg` offline with
`.\tools\generate-icon.ps1` once Cargo dependencies have been cached.

CI checks formatting on Ubuntu and runs Clippy and tests on Windows. Manually
run the Release workflow to build every architecture and create a draft GitHub
release with an installer and a portable ZIP for each; publication is a separate
action.

## Settings

Settings are stored in `%APPDATA%\Candle Tray\settings.toml`, including when
running the portable executable.

## License

Licensed under the [MIT License](LICENSE). See
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for acknowledgements.
