//! Drives idle dimming and the time-of-day schedule.
//!
//! A timer on the GPUI thread calls [`AppModel::automation_tick`], which
//! decides everything; this module only runs the loop and asks Windows how
//! long the user has been away and whether something keeps the screen on.

use std::time::Duration;

use gpui::{App, Entity};

use crate::app::AppModel;

/// Start the automation loop for the app's lifetime.
pub fn start(model: Entity<AppModel>, cx: &mut App) {
    cx.spawn(async move |cx| {
        loop {
            let Ok(delay) = model.update(cx, |model, cx| model.automation_tick(cx)) else {
                break;
            };
            cx.background_executor().timer(delay).await;
        }
    })
    .detach();
}

/// Time since the last keyboard or mouse input anywhere in the session.
#[cfg(windows)]
pub fn idle_time() -> Duration {
    use windows_sys::Win32::System::SystemInformation::GetTickCount;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
    let mut info = LASTINPUTINFO {
        cbSize: size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    // SAFETY: `info` is a properly sized LASTINPUTINFO owned by this frame.
    if unsafe { GetLastInputInfo(&mut info) } == 0 {
        return Duration::ZERO;
    }
    // Both tick counts wrap every 49.7 days; the difference stays right.
    let now = unsafe { GetTickCount() };
    Duration::from_millis(u64::from(now.wrapping_sub(info.dwTime)))
}

#[cfg(not(windows))]
pub fn idle_time() -> Duration {
    Duration::ZERO
}

/// Independent idle exclusions. Display power requests and fullscreen state
/// have different meanings and must not enable each other.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BusyState {
    pub fullscreen: bool,
    pub media: bool,
}
impl BusyState {
    pub fn excludes(self, config: crate::settings::IdleDim) -> bool {
        (self.fullscreen && config.fullscreen_excluded()) || (self.media && config.media_excluded())
    }
}

#[cfg(windows)]
pub fn busy_state() -> BusyState {
    use windows::Win32::System::Power::{CallNtPowerInformation, SystemExecutionState};
    use windows_sys::Win32::UI::Shell::{
        QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState,
    };
    let mut state = 0u32;
    // SAFETY: SystemExecutionState writes a single ULONG to this buffer.
    let status = unsafe {
        CallNtPowerInformation(
            SystemExecutionState,
            None,
            0,
            Some((&mut state as *mut u32).cast()),
            size_of::<u32>() as u32,
        )
    };
    let mut notification = 0;
    let ok = unsafe { SHQueryUserNotificationState(&mut notification) } == 0;
    BusyState {
        media: status.is_ok() && state & 2 != 0,
        fullscreen: (ok
            && matches!(
                notification,
                QUNS_RUNNING_D3D_FULL_SCREEN | QUNS_PRESENTATION_MODE
            ))
            || crate::foreground::fullscreen_now(),
    }
}
#[cfg(not(windows))]
pub fn busy_state() -> BusyState {
    BusyState::default()
}

/// Runtime gates shared by every automatic producer. Manual input holds ALS
/// until an automation edit or a new foreground/power trigger takes ownership.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ControlState {
    pub locked: bool,
    pub suspended: bool,
    pub manual: bool,
}
impl ControlState {
    pub fn blocked(self, settings: &crate::settings::Settings) -> bool {
        self.suspended || (self.locked && settings.pause_when_locked)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{IdleDim, Settings};

    #[test]
    fn idle_exclusions_are_independent_and_migrate_legacy_values() {
        for legacy in [false, true] {
            let mut idle: IdleDim =
                serde_json::from_value(serde_json::json!({"respect_busy": legacy})).unwrap();
            assert_eq!(idle.fullscreen_excluded(), legacy);
            assert_eq!(idle.media_excluded(), legacy);
            idle.exclude_media = Some(false);
            idle.exclude_fullscreen = Some(true);
            assert!(
                BusyState {
                    fullscreen: true,
                    media: false
                }
                .excludes(idle)
            );
            assert!(
                !BusyState {
                    fullscreen: false,
                    media: true
                }
                .excludes(idle)
            );
            let saved = serde_json::to_string(&idle).unwrap();
            assert_eq!(serde_json::from_str::<IdleDim>(&saved).unwrap(), idle);
        }
    }
    #[test]
    fn lock_option_applies_live_and_suspend_always_blocks() {
        let mut settings = Settings::default();
        let mut state = ControlState {
            locked: true,
            ..Default::default()
        };
        assert!(!state.blocked(&settings));
        settings.pause_when_locked = true;
        assert!(state.blocked(&settings));
        state.suspended = true;
        state.locked = false;
        assert!(state.blocked(&settings));
        state.suspended = false;
        assert!(!state.blocked(&settings));
        let old: Settings = serde_json::from_str("{}").unwrap();
        assert!(!old.pause_when_locked && !old.hide_closed_lid);
        assert!(!old.monitor("display").skip_restore);
    }
}
