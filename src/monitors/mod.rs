//! Monitor discovery and brightness control.
//!
//! This module is the only place that talks to display hardware. Everything
//! runs off the UI thread: [`MonitorService::start`] spawns a coordinator
//! thread that owns per-monitor worker threads, so a monitor whose DDC/CI
//! bus hangs only stalls its own worker and never the popup or other
//! monitors.
//!
//! Backends:
//! - DDC/CI through dxva2 physical monitor handles (external displays).
//! - WMI `root\wmi` `WmiMonitorBrightness` / `WmiMonitorBrightnessMethods`
//!   (internal laptop panels).
//! - SDR white level for displays with HDR active (DisplayConfig).
//! - Contrast through DDC/CI VCP 0x12 where the display answers it.
//!
//! Identity: every display is keyed by the instance part of its device path
//! (`\\?\DISPLAY#<model>#<instance>#{guid}` → `<instance>`, trimmed at the
//! first `_`), matching legacy Twinkle Tray's `hwid[2]` key so imported
//! settings line up.
//!
//! Direct level setters and monitor snapshots use hardware percentages
//! (0..=100 of the backend's own range). `Execute` accepts explicitly typed
//! high-level action requests, evaluated on the worker with `crate::levels`
//! before calling those same hardware-percent setters. Normalized values
//! must never be passed directly to `SetBrightness` or `Apply`.

use std::time::Duration;
#[cfg(windows)]
mod ddc;
#[cfg(windows)]
mod enumerate;
#[cfg(windows)]
mod events;
#[cfg(windows)]
mod hdr;
mod rules;
#[cfg(windows)]
mod service;
#[cfg(windows)]
mod wmi;
fn log(message: impl std::fmt::Display) {
    crate::logger::info(format!("[monitors] {message}"));
}

/// Stable per-display key (device path instance, legacy `hwid[2]`).
pub type MonitorId = String;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MonitorKind {
    /// Built-in laptop panel (controlled through WMI).
    Internal,
    /// External display (controlled through DDC/CI when supported).
    External,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrightnessBackend {
    /// DDC/CI through the given VCP code (0x10 luminance by default).
    Ddc {
        vcp: u8,
    },
    Wmi,
    /// The display was found but its brightness cannot be controlled.
    Unsupported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Monitor {
    pub vcp_values: std::collections::BTreeMap<u8, (u32, u32)>,
    pub id: MonitorId,
    /// EDID manufacturer + product code, legacy `hwid[1]` (e.g. `DEL41D9`).
    pub model: String,
    /// Full `\\?\DISPLAY#...` device interface path.
    pub device_path: String,
    /// Friendly name from EDID (WMI `WmiMonitorID` / DisplayConfig target
    /// name); falls back to a generic localized-agnostic name like
    /// "Display 2" only when the hardware reports nothing.
    pub name: String,
    pub kind: MonitorKind,
    /// 1-based position in Windows' display numbering (stable sort key).
    pub num: u32,
    /// Desktop coordinates of the source mode, for ordering and icons.
    pub bounds: Option<Rect>,
    pub backend: BrightnessBackend,
    /// Current hardware brightness percent, `None` when unsupported or the
    /// last read failed.
    pub brightness: Option<u8>,
    /// Present only while HDR (advanced color) is active on the display.
    pub sdr: Option<SdrLevel>,
    /// The display can switch HDR on (it may be off right now).
    pub hdr_supported: bool,
    /// Current DDC/CI contrast percent (VCP 0x12), `None` when the display
    /// does not answer it.
    pub contrast: Option<u8>,
    /// The display reported VCP 0xD6 (power mode) in its capabilities.
    pub supports_ddc_power: bool,
    /// Input sources listed for VCP 0x60 in the display's capabilities.
    pub inputs: Vec<u8>,
    /// Top-level VCP codes listed in the display capabilities, empty if unknown.
    pub vcp_codes: Vec<u8>,
    /// Current input source, `None` when unknown.
    pub input: Option<u8>,
    /// Switched off through DDC/CI (VCP 0xD6) by this app.
    pub powered_off: bool,
    /// True while the monitor's worker is stuck or timed out; the UI dims
    /// the row and ignores input until the next successful refresh.
    pub unresponsive: bool,
}

/// Name of a VCP 0x60 input source value (MCCS), such as "HDMI 1".
pub fn input_label(value: u8) -> String {
    let name = match value {
        0x01 => "VGA 1",
        0x02 => "VGA 2",
        0x03 => "DVI 1",
        0x04 => "DVI 2",
        0x05 => "Composite 1",
        0x06 => "Composite 2",
        0x07 => "S-Video 1",
        0x08 => "S-Video 2",
        0x09 => "Tuner 1",
        0x0a => "Tuner 2",
        0x0b => "Tuner 3",
        0x0c => "Component 1",
        0x0d => "Component 2",
        0x0e => "Component 3",
        0x0f => "DisplayPort 1",
        0x10 => "DisplayPort 2",
        0x11 => "HDMI 1",
        0x12 => "HDMI 2",
        // Not in MCCS, but what most USB-C displays report.
        0x1b => "USB-C",
        _ => return format!("0x{value:02X}"),
    };
    name.to_owned()
}

/// SDR content brightness on an HDR display.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SdrLevel {
    /// 0..=100, where `nits = 80 + percent * 4` (legacy Twinkle mapping).
    pub percent: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TurnOffMode {
    /// `SC_MONITORPOWER` broadcast (Windows puts every display to sleep).
    Windows,
    /// VCP 0xD6 to every DDC display that supports it.
    Ddc,
    Both,
}

#[derive(Clone, Debug)]
pub enum Command {
    ConfigureFeatures {
        id: MonitorId,
        codes: Vec<u8>,
    },
    /// Re-enumerate displays and re-read every level. Emits
    /// [`Event::Snapshot`] when done.
    Refresh,
    /// FIFO writes per display; never coalesced or reordered within a sequence.
    Execute(Vec<(MonitorId, crate::actions::Write)>, std::time::Instant),
    /// Re-read the current levels of connected displays through the handles
    /// already open (no re-enumeration). Emits [`Event::Updated`] per display.
    ReadLevels,
    /// Emit a complete inventory after probes and capability reads finish or
    /// time out. Unlike the initial Snapshot, this includes input/VCP support.
    ReadInventory,
    /// Coalesced: only the latest value per monitor is written, at most once
    /// per throttle interval, and the final value is always written.
    SetBrightness {
        id: MonitorId,
        percent: u8,
    },
    /// Prefer this brightness code and re-probe only this display. None clears
    /// the override. Preferences persist across display re-enumeration.
    SetBrightnessVcp {
        id: MonitorId,
        vcp: Option<u8>,
    },
    SetSdr {
        id: MonitorId,
        percent: u8,
    },
    SetContrast {
        id: MonitorId,
        percent: u8,
    },
    /// Switch HDR on or off. The display then re-reports its SDR level.
    SetHdr {
        id: MonitorId,
        on: bool,
    },
    /// Switch the display to another input source (VCP 0x60).
    SetInput {
        id: MonitorId,
        value: u8,
    },
    /// Write several levels at once (restore after startup/resume/display
    /// change). Same coalescing as the single setters.
    Apply(Vec<LevelTarget>),
    TurnOff(TurnOffMode),
    /// Switch one display on or off through DDC/CI (VCP 0xD6). When every
    /// connected display is off, the next keyboard or mouse input switches
    /// them all back on.
    SetPower {
        id: MonitorId,
        on: bool,
    },
    /// Minimum interval between hardware writes per target.
    SetThrottle(Duration),
    /// Cancel queued brightness/SDR/contrast writes on session suspension.
    /// An already executing driver call cannot be cancelled safely.
    DiscardPendingLevels,
    /// Cancel queued writes only for these displays, preserving other workers.
    DiscardPendingFor(Vec<MonitorId>),
    Shutdown,
}

#[derive(Clone, Debug)]
pub struct LevelTarget {
    pub id: MonitorId,
    pub brightness: Option<u8>,
    pub sdr: Option<u8>,
    pub contrast: Option<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SnapshotReason {
    Startup,
    /// Explicit [`Command::Refresh`].
    Requested,
    /// `WM_DISPLAYCHANGE`, device arrival/removal or HDR toggled.
    DisplaysChanged,
    /// Resumed from sleep/hibernate or the console display turned back on.
    Resumed,
}

#[derive(Clone, Debug)]
pub enum Event {
    /// Inventory requested by [`Command::ReadInventory`].
    Inventory(Vec<Monitor>),
    /// Successful ordered action. Remembered levels are normalized slider
    /// percentages; the monitor snapshot retains hardware percentages.
    ActionApplied {
        control: crate::actions::Control,
        issued: std::time::Instant,
        monitor: Monitor,
        brightness: Option<u8>,
        contrast: Option<u8>,
    },
    /// Full list of displays, sorted by `num`.
    Snapshot {
        monitors: Vec<Monitor>,
        reason: SnapshotReason,
    },
    /// One monitor changed (a write landed, a read finished, or it became
    /// unresponsive/responsive again).
    Updated(Monitor),
}

/// Handle to the running service. Dropping it shuts the service down.
pub struct MonitorService {
    sender: std::sync::mpsc::Sender<Command>,
}

impl MonitorService {
    #[cfg(test)]
    pub(crate) fn test_channel() -> (Self, std::sync::mpsc::Receiver<Command>) {
        let (sender, receiver) = std::sync::mpsc::channel();
        (Self { sender }, receiver)
    }

    /// Start the coordinator thread, its hidden message window (display,
    /// power and resume notifications) and an initial refresh. `on_event` is
    /// called from service threads; it must be cheap and non-blocking.
    pub fn start(on_event: impl Fn(Event) + Send + Sync + 'static) -> Self {
        #[cfg(windows)]
        {
            service::start(on_event)
        }
        #[cfg(not(windows))]
        {
            let (sender, receiver) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                on_event(Event::Snapshot {
                    monitors: Vec::new(),
                    reason: SnapshotReason::Startup,
                });
                while let Ok(command) = receiver.recv() {
                    match command {
                        Command::ReadInventory => on_event(Event::Inventory(Vec::new())),
                        Command::Shutdown => break,
                        _ => {}
                    }
                }
            });
            Self { sender }
        }
    }

    pub fn send(&self, command: Command) {
        let _ = self.sender.send(command);
    }
}

impl Drop for MonitorService {
    fn drop(&mut self) {
        let _ = self.sender.send(Command::Shutdown);
    }
}
