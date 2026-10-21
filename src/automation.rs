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

/// Something wants the screen on: a video or presentation holds a display
/// power request, or an app runs full screen.
#[cfg(windows)]
pub fn screen_kept_on() -> bool {
    use windows::Win32::System::Power::{CallNtPowerInformation, SystemExecutionState};
    use windows_sys::Win32::UI::Shell::{
        QUNS_BUSY, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN,
        SHQueryUserNotificationState,
    };
    /// `ES_DISPLAY_REQUIRED`.
    const DISPLAY_REQUIRED: u32 = 0x0000_0002;
    let mut state = 0u32;
    // SAFETY: the output buffer is a u32 owned by this frame, as
    // SystemExecutionState requires.
    let status = unsafe {
        CallNtPowerInformation(
            SystemExecutionState,
            None,
            0,
            Some((&mut state as *mut u32).cast()),
            size_of::<u32>() as u32,
        )
    };
    if status.is_ok() && state & DISPLAY_REQUIRED != 0 {
        return true;
    }
    let mut notification = 0;
    // SAFETY: plain out-parameter call.
    let ok = unsafe { SHQueryUserNotificationState(&mut notification) } == 0;
    ok && matches!(
        notification,
        QUNS_BUSY | QUNS_RUNNING_D3D_FULL_SCREEN | QUNS_PRESENTATION_MODE
    )
}

#[cfg(not(windows))]
pub fn screen_kept_on() -> bool {
    false
}
