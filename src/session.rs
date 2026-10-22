//! Session, suspend and lid notifications owned by the tray message thread.
//! The hidden top-level window receives broadcasts; a message-only window would
//! miss suspend/resume. No GPUI state or monitor I/O is touched here.

#[cfg(windows)]
mod native {
    use crate::popup::{PopupCommand, send_command};
    use std::{
        marker::PhantomData,
        rc::Rc,
        time::{Duration, Instant},
    };
    use windows_sys::{
        Win32::{
            Foundation::{HWND, LPARAM, LRESULT, WPARAM},
            System::{
                LibraryLoader::GetModuleHandleW,
                Power::{
                    HPOWERNOTIFY, POWERBROADCAST_SETTING, RegisterPowerSettingNotification,
                    UnregisterPowerSettingNotification,
                },
                RemoteDesktop::*,
            },
            UI::WindowsAndMessaging::*,
        },
        core::{GUID, w},
    };

    const LID: GUID = GUID::from_u128(0xba3e0f4d_b817_4094_a2d1_d56379e6a0f3);
    pub struct Watcher {
        hwnd: HWND,
        power: HPOWERNOTIFY,
        registered: bool,
        next_poll: Instant,
        _thread: PhantomData<Rc<()>>,
    }
    impl Watcher {
        pub fn new() -> anyhow::Result<Self> {
            unsafe {
                let instance = GetModuleHandleW(std::ptr::null());
                let class = WNDCLASSW {
                    lpfnWndProc: Some(proc),
                    hInstance: instance,
                    lpszClassName: w!("CandleTraySessionWatcher"),
                    ..std::mem::zeroed()
                };
                RegisterClassW(&class);
                let hwnd = CreateWindowExW(
                    0,
                    class.lpszClassName,
                    w!(""),
                    0,
                    0,
                    0,
                    0,
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    instance,
                    std::ptr::null(),
                );
                anyhow::ensure!(
                    !hwnd.is_null(),
                    "create session notification window: {}",
                    std::io::Error::last_os_error()
                );
                let mut watcher = Self {
                    hwnd,
                    power: 0,
                    registered: false,
                    next_poll: Instant::now(),
                    _thread: PhantomData,
                };
                watcher.poll();
                if !watcher.registered {
                    crate::logger::error(
                        "session notifications unavailable; retrying in five seconds",
                    );
                }
                if watcher.power == 0 {
                    crate::logger::error("lid notifications unavailable; retrying in five seconds");
                }
                Ok(watcher)
            }
        }
        /// Recover if the session service was not ready at startup. Polling
        /// also repairs a missed lock/unlock broadcast after resume.
        pub fn poll(&mut self) {
            if Instant::now() < self.next_poll {
                return;
            }
            self.next_poll = Instant::now() + Duration::from_secs(5);
            unsafe {
                if !self.registered {
                    self.registered =
                        WTSRegisterSessionNotification(self.hwnd, NOTIFY_FOR_THIS_SESSION) != 0;
                }
                // Windows reports lid state once a lid device is found and its
                // state is known. Until then the panel remains selected.
                if self.power == 0 {
                    self.power = RegisterPowerSettingNotification(
                        self.hwnd,
                        &LID,
                        DEVICE_NOTIFY_WINDOW_HANDLE,
                    );
                }
            }
            if let Some(locked) = locked_now() {
                send_command(PopupCommand::SessionLocked(locked));
            }
        }
    }
    impl Drop for Watcher {
        fn drop(&mut self) {
            unsafe {
                if self.power != 0 {
                    UnregisterPowerSettingNotification(self.power);
                }
                WTSUnRegisterSessionNotification(self.hwnd);
                DestroyWindow(self.hwnd);
            }
        }
    }
    fn locked_now() -> Option<bool> {
        unsafe {
            let mut buffer = std::ptr::null_mut();
            let mut size = 0;
            if WTSQuerySessionInformationW(
                WTS_CURRENT_SERVER_HANDLE,
                WTS_CURRENT_SESSION,
                WTSSessionInfoEx,
                &mut buffer,
                &mut size,
            ) == 0
            {
                return None;
            }
            let result = if !buffer.is_null() && size as usize >= size_of::<WTSINFOEXW>() {
                let info = &*buffer.cast::<WTSINFOEXW>();
                if info.Level == 1 {
                    match info.Data.WTSInfoExLevel1.SessionFlags as u32 {
                        WTS_SESSIONSTATE_LOCK => Some(true),
                        WTS_SESSIONSTATE_UNLOCK => Some(false),
                        _ => None,
                    }
                } else {
                    None
                }
            } else {
                None
            };
            WTSFreeMemory(buffer.cast());
            result
        }
    }
    unsafe extern "system" fn proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match message {
            WM_WTSSESSION_CHANGE => match wparam as u32 {
                7 => send_command(PopupCommand::SessionLocked(true)),
                8 => send_command(PopupCommand::SessionLocked(false)),
                _ => {}
            },
            WM_POWERBROADCAST => match wparam as u32 {
                PBT_APMSUSPEND => send_command(PopupCommand::Suspended(true)),
                PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND => {
                    if let Some(locked) = locked_now() {
                        send_command(PopupCommand::SessionLocked(locked));
                    }
                    send_command(PopupCommand::Suspended(false));
                }
                PBT_POWERSETTINGCHANGE if lparam != 0 => {
                    // Windows owns this buffer for this callback only. Data is
                    // variable-length and may be unaligned: read the DWORD once.
                    let setting = unsafe { &*(lparam as *const POWERBROADCAST_SETTING) };
                    if setting.PowerSetting.data1 == LID.data1
                        && setting.PowerSetting.data2 == LID.data2
                        && setting.PowerSetting.data3 == LID.data3
                        && setting.PowerSetting.data4 == LID.data4
                        && setting.DataLength == 4
                    {
                        let state = unsafe {
                            std::ptr::read_unaligned(setting.Data.as_ptr().cast::<u32>())
                        };
                        if state <= 1 {
                            send_command(PopupCommand::LidClosed(state == 0));
                        }
                    }
                }
                _ => {}
            },
            _ => {}
        }
        unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
    }
}
#[cfg(windows)]
pub use native::Watcher;
