//! Coordinator-owned windows and notifications. A hidden top-level window also
//! receives broadcasts that Windows deliberately excludes from message-only windows.
use super::SnapshotReason;
use std::{cell::RefCell, sync::OnceLock};
use windows::{
    Win32::{
        Devices::Display::GUID_DEVINTERFACE_MONITOR, Foundation::*, System::Power::*,
        UI::WindowsAndMessaging::*,
    },
    core::{GUID, w},
};
const CONSOLE_DISPLAY: GUID = GUID::from_u128(0x6fe69556_704a_47a0_8f24_c28d936fda47);
thread_local! { static REASONS:RefCell<Vec<SnapshotReason>> = const {RefCell::new(Vec::new())}; }
/// Logs the raw Windows signal, so a burst of refreshes can be traced to its source.
fn log_signal(message: u32, w: WPARAM, l: LPARAM) {
    let line = match message {
        WM_DISPLAYCHANGE => format!(
            "WM_DISPLAYCHANGE {}x{} {} bpp",
            l.0 & 0xffff,
            (l.0 >> 16) & 0xffff,
            w.0
        ),
        WM_DEVICECHANGE => {
            let kind = match w.0 as u32 {
                DBT_DEVICEARRIVAL => "DBT_DEVICEARRIVAL",
                DBT_DEVICEREMOVECOMPLETE => "DBT_DEVICEREMOVECOMPLETE",
                DBT_DEVNODES_CHANGED => "DBT_DEVNODES_CHANGED",
                _ => return,
            };
            let mut line = format!("WM_DEVICECHANGE {kind}");
            if l.0 != 0 && w.0 as u32 != DBT_DEVNODES_CHANGED {
                // SAFETY: arrival/removal always carry a DEV_BROADCAST_HDR.
                let header = unsafe { &*(l.0 as *const DEV_BROADCAST_HDR) };
                if header.dbch_devicetype == DBT_DEVTYP_DEVICEINTERFACE {
                    // SAFETY: the header announced a device interface broadcast,
                    // whose name is NUL-terminated.
                    let name = unsafe {
                        let interface = l.0 as *const DEV_BROADCAST_DEVICEINTERFACE_W;
                        windows::core::PCWSTR((*interface).dbcc_name.as_ptr())
                            .to_string()
                            .unwrap_or_default()
                    };
                    line.push_str(&format!(" {name}"));
                }
            }
            line
        }
        WM_POWERBROADCAST => match w.0 as u32 {
            PBT_APMSUSPEND => "WM_POWERBROADCAST suspend".to_owned(),
            PBT_APMRESUMEAUTOMATIC => "WM_POWERBROADCAST resume (automatic)".to_owned(),
            PBT_APMRESUMESUSPEND => "WM_POWERBROADCAST resume (user)".to_owned(),
            PBT_POWERSETTINGCHANGE if l.0 != 0 => {
                let setting = unsafe { &*(l.0 as *const POWERBROADCAST_SETTING) };
                if setting.PowerSetting != CONSOLE_DISPLAY || setting.DataLength < 4 {
                    return;
                }
                let state =
                    unsafe { std::ptr::read_unaligned(setting.Data.as_ptr() as *const u32) };
                let state = match state {
                    0 => "off",
                    1 => "on",
                    2 => "dimmed",
                    _ => "unknown",
                };
                format!("WM_POWERBROADCAST console display {state}")
            }
            _ => return,
        },
        _ => return,
    };
    super::log(format!("Signal: {line}"));
}

unsafe extern "system" fn procedure(hwnd: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    log_signal(message, w, l);
    let reason = match message {
        WM_DISPLAYCHANGE => Some(SnapshotReason::DisplaysChanged),
        WM_DEVICECHANGE
            if matches!(
                w.0 as u32,
                DBT_DEVICEARRIVAL | DBT_DEVICEREMOVECOMPLETE | DBT_DEVNODES_CHANGED
            ) =>
        {
            Some(SnapshotReason::DisplaysChanged)
        }
        WM_POWERBROADCAST
            if matches!(w.0 as u32, PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND) =>
        {
            Some(SnapshotReason::Resumed)
        }
        WM_POWERBROADCAST if w.0 as u32 == PBT_POWERSETTINGCHANGE && l.0 != 0 => {
            let setting = unsafe { &*(l.0 as *const POWERBROADCAST_SETTING) };
            (setting.PowerSetting == CONSOLE_DISPLAY
                && setting.DataLength >= 4
                && unsafe { std::ptr::read_unaligned(setting.Data.as_ptr() as *const u32) } == 1)
                .then_some(SnapshotReason::Resumed)
        }
        _ => None,
    };
    if let Some(reason) = reason {
        REASONS.with(|r| r.borrow_mut().push(reason));
    }
    unsafe { DefWindowProcW(hwnd, message, w, l) }
}
pub(super) struct Events {
    message: HWND,
    broadcast: HWND,
    power: Option<HPOWERNOTIFY>,
    device: Option<HDEVNOTIFY>,
}
impl Events {
    pub fn new() -> windows::core::Result<Self> {
        unsafe {
            static CLASS: OnceLock<u16> = OnceLock::new();
            let atom = CLASS.get_or_init(|| {
                RegisterClassW(&WNDCLASSW {
                    lpfnWndProc: Some(procedure),
                    lpszClassName: w!("CandleTrayMonitorEvents"),
                    ..Default::default()
                })
            });
            if *atom == 0 {
                return Err(windows::core::Error::from_thread());
            }
            let message = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("CandleTrayMonitorEvents"),
                w!(""),
                WINDOW_STYLE::default(),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                None,
                None,
            )?;
            let broadcast = match CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("CandleTrayMonitorEvents"),
                w!(""),
                WINDOW_STYLE::default(),
                0,
                0,
                0,
                0,
                None,
                None,
                None,
                None,
            ) {
                Ok(hwnd) => hwnd,
                Err(error) => {
                    let _ = DestroyWindow(message);
                    return Err(error);
                }
            };
            let power = RegisterPowerSettingNotification(
                HANDLE(message.0),
                &CONSOLE_DISPLAY,
                DEVICE_NOTIFY_WINDOW_HANDLE,
            )
            .map_err(|e| super::log(format!("Power notifications: {e}")))
            .ok();
            let filter = DEV_BROADCAST_DEVICEINTERFACE_W {
                dbcc_size: size_of::<DEV_BROADCAST_DEVICEINTERFACE_W>() as u32,
                dbcc_devicetype: DBT_DEVTYP_DEVICEINTERFACE.0,
                dbcc_classguid: GUID_DEVINTERFACE_MONITOR,
                ..Default::default()
            };
            let device = RegisterDeviceNotificationW(
                HANDLE(message.0),
                &filter as *const _ as *const _,
                DEVICE_NOTIFY_WINDOW_HANDLE,
            )
            .map_err(|e| super::log(format!("Device notifications: {e}")))
            .ok();
            Ok(Self {
                message,
                broadcast,
                power,
                device,
            })
        }
    }
    pub fn pump() -> Vec<SnapshotReason> {
        unsafe {
            let mut message = MSG::default();
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        REASONS.with(|r| std::mem::take(&mut *r.borrow_mut()))
    }
}
impl Drop for Events {
    fn drop(&mut self) {
        unsafe {
            if let Some(power) = self.power {
                let _ = UnregisterPowerSettingNotification(power);
            }
            if let Some(device) = self.device {
                let _ = UnregisterDeviceNotification(device);
            }
            let _ = DestroyWindow(self.message);
            let _ = DestroyWindow(self.broadcast);
        }
    }
}
/// Tick count of the last keyboard or mouse input in this session.
pub(super) fn last_input_tick() -> u32 {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
    let mut info = LASTINPUTINFO {
        cbSize: size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    // SAFETY: `info` is a properly sized LASTINPUTINFO owned by this frame.
    unsafe { GetLastInputInfo(&mut info) };
    info.dwTime
}

pub(super) fn turn_off() {
    unsafe {
        let _ = PostMessageW(
            Some(HWND_BROADCAST),
            WM_SYSCOMMAND,
            WPARAM(SC_MONITORPOWER as usize),
            LPARAM(2),
        );
    }
}
