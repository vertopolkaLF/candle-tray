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
//!
//! Identity: every display is keyed by the instance part of its device path
//! (`\\?\DISPLAY#<model>#<instance>#{guid}` → `<instance>`, trimmed at the
//! first `_`), matching legacy Twinkle Tray's `hwid[2]` key so imported
//! settings line up.
//!
//! All levels crossing this API are hardware percentages (0..=100 of the
//! backend's own range). Per-monitor min/max normalization is the caller's
//! job (see `crate::levels`).

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
    /// The display reported VCP 0xD6 (power mode) in its capabilities.
    pub supports_ddc_power: bool,
    /// True while the monitor's worker is stuck or timed out; the UI dims
    /// the row and ignores input until the next successful refresh.
    pub unresponsive: bool,
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
    /// Re-enumerate displays and re-read every level. Emits
    /// [`Event::Snapshot`] when done.
    Refresh,
    /// Coalesced: only the latest value per monitor is written, at most once
    /// per throttle interval, and the final value is always written.
    SetBrightness {
        id: MonitorId,
        percent: u8,
    },
    SetSdr {
        id: MonitorId,
        percent: u8,
    },
    /// Write several levels at once (restore after startup/resume/display
    /// change). Same coalescing as the single setters.
    Apply(Vec<LevelTarget>),
    TurnOff(TurnOffMode),
    /// Minimum interval between hardware writes per target.
    SetThrottle(Duration),
    Shutdown,
}

#[derive(Clone, Debug)]
pub struct LevelTarget {
    pub id: MonitorId,
    pub brightness: Option<u8>,
    pub sdr: Option<u8>,
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
            let (sender, _) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                on_event(Event::Snapshot {
                    monitors: Vec::new(),
                    reason: SnapshotReason::Startup,
                })
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
