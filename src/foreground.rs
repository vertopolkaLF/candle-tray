//! Tray-thread foreground and power-source watchers for profile triggers.
//!
//! Foreground events carry the first eligible application's lowercase executable
//! name, then only name changes. Fullscreen is sampled on those events; resizing
//! a window or switching between windows of the same executable does not emit.
//! Our process, taskbars and Explorer desktop surfaces are ignored without
//! clearing the last application. Ordinary Explorer folder windows count normally.

use std::{
    marker::PhantomData,
    rc::Rc,
    time::{Duration, Instant},
};

/// Owns the WinEvent hook. Create, poll and drop on the tray message-pump thread.
/// The guard is deliberately neither Send nor Sync. Only one may exist per thread.
pub struct Registration {
    #[cfg(windows)]
    hook: windows_sys::Win32::UI::Accessibility::HWINEVENTHOOK,
    #[cfg(windows)]
    last_exe: Option<String>,
    last_power: Option<bool>,
    next_power: Instant,
    _thread: PhantomData<Rc<()>>,
}

impl Registration {
    /// Installs the foreground hook. The first `poll` samples current foreground
    /// and power, emitting initial known states so triggers have a baseline.
    pub fn new() -> anyhow::Result<Self> {
        #[cfg(windows)]
        let hook = native::install()?;
        Ok(Self {
            #[cfg(windows)]
            hook,
            #[cfg(windows)]
            last_exe: None,
            last_power: None,
            next_power: Instant::now(),
            _thread: PhantomData,
        })
    }

    /// Call after pumping tray messages. The hook only records the newest HWND
    /// (rapid activations in one pump are coalesced); process lookup and command
    /// delivery happen here, outside the callback. Power is sampled every five
    /// seconds on the tray's existing timer: no extra window/thread is needed,
    /// and polling recovers naturally after sleep or a missed broadcast.
    pub fn poll(&mut self) {
        #[cfg(windows)]
        if let Some((exe, fullscreen)) = native::take_foreground()
            && changed(&mut self.last_exe, exe.clone())
        {
            crate::popup::send_command(crate::popup::PopupCommand::Foreground { exe, fullscreen });
        }
        if Instant::now() >= self.next_power {
            self.next_power = Instant::now() + Duration::from_secs(5);
            if let Some(on_battery) = on_battery_now()
                && changed(&mut self.last_power, on_battery)
            {
                crate::popup::send_command(crate::popup::PopupCommand::PowerSource { on_battery });
            }
        }
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        #[cfg(windows)]
        native::uninstall(self.hook);
    }
}

/// Reads power synchronously from any thread. `Some(true)` means battery power,
/// `Some(false)` AC. `None` means no battery, unknown status, or a failed query.
/// This function never emits commands and does not change the watcher's state.
pub fn on_battery_now() -> Option<bool> {
    #[cfg(windows)]
    {
        use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
        let mut status = SYSTEM_POWER_STATUS::default();
        unsafe { GetSystemPowerStatus(&mut status) }.ok()?;
        battery_source(status.ACLineStatus, status.BatteryFlag)
    }
    #[cfg(not(windows))]
    None
}

fn changed<T: PartialEq>(previous: &mut Option<T>, current: T) -> bool {
    if previous.as_ref() == Some(&current) {
        false
    } else {
        *previous = Some(current);
        true
    }
}

#[cfg(any(windows, test))]
fn exe_name(path: &str) -> Option<String> {
    let name = path.rsplit(['\\', '/']).next()?;
    (!name.is_empty()).then(|| name.to_lowercase())
}

#[cfg(any(windows, test))]
fn battery_source(ac: u8, flags: u8) -> Option<bool> {
    if flags == 255 || flags & 128 != 0 {
        return None;
    }
    match ac {
        0 => Some(true),
        1 => Some(false),
        _ => None,
    }
}

#[cfg(windows)]
mod native {
    use std::cell::Cell;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HWND, RECT},
        Graphics::{
            Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute},
            Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow},
        },
        System::Threading::{
            GetCurrentProcessId, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
            QueryFullProcessImageNameW,
        },
        UI::{
            Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent},
            WindowsAndMessaging::{
                EVENT_SYSTEM_FOREGROUND, GetClassNameW, GetForegroundWindow, GetWindowRect,
                GetWindowThreadProcessId, IsIconic, WINEVENT_OUTOFCONTEXT,
            },
        },
    };

    thread_local! {
        static PENDING: Cell<HWND> = const { Cell::new(std::ptr::null_mut()) };
        static INSTALLED: Cell<bool> = const { Cell::new(false) };
    }

    pub(super) fn install() -> anyhow::Result<HWINEVENTHOOK> {
        anyhow::ensure!(
            !INSTALLED.get(),
            "foreground watcher is already installed on this thread"
        );
        let hook = unsafe {
            SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_FOREGROUND,
                std::ptr::null_mut(),
                Some(callback),
                0,
                0,
                WINEVENT_OUTOFCONTEXT,
            )
        };
        anyhow::ensure!(
            !hook.is_null(),
            "install foreground WinEvent hook: {}",
            std::io::Error::last_os_error()
        );
        INSTALLED.set(true);
        PENDING.set(unsafe { GetForegroundWindow() });
        Ok(hook)
    }

    pub(super) fn uninstall(hook: HWINEVENTHOOK) {
        // The non-Send guard guarantees this is the installing thread.
        unsafe { UnhookWinEvent(hook) };
        PENDING.set(std::ptr::null_mut());
        INSTALLED.set(false);
    }

    unsafe extern "system" fn callback(
        _hook: HWINEVENTHOOK,
        _event: u32,
        hwnd: HWND,
        _object: i32,
        _child: i32,
        _thread: u32,
        _time: u32,
    ) {
        // OUTOFCONTEXT delivers on the installing thread while it pumps messages.
        // No allocations, process queries, logging or locks in this callback.
        PENDING.set(hwnd);
    }

    pub(super) fn take_foreground() -> Option<(String, bool)> {
        let hwnd = PENDING.replace(std::ptr::null_mut());
        if hwnd.is_null() || hwnd != unsafe { GetForegroundWindow() } {
            return None;
        }
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        if pid == 0 || pid == unsafe { GetCurrentProcessId() } {
            return None;
        }
        let mut class = [0u16; 64];
        let len = unsafe { GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32) };
        // Filter shell surfaces by class, never explorer.exe itself: folder
        // windows (CabinetWClass) are real application activations. This also
        // prevents a tray click from activating an Explorer profile.
        if len > 0
            && matches!(
                String::from_utf16_lossy(&class[..len as usize]).as_str(),
                "Shell_TrayWnd"
                    | "Shell_SecondaryTrayWnd"
                    | "NotifyIconOverflowWindow"
                    | "TopLevelWindowForOverflowXamlIsland"
                    | "Progman"
                    | "WorkerW"
            )
        {
            return None;
        }
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if process.is_null() {
            return None;
        }
        // Windows' maximum extended path fits in this stack buffer.
        let mut path = [0u16; 32768];
        let mut len = path.len() as u32;
        let success =
            unsafe { QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut len) };
        unsafe { CloseHandle(process) };
        if success == 0 {
            return None;
        }
        let exe = super::exe_name(&String::from_utf16_lossy(&path[..len as usize]))?;
        Some((exe, fullscreen(hwnd)))
    }

    fn fullscreen(hwnd: HWND) -> bool {
        if unsafe { IsIconic(hwnd) } != 0 {
            return false;
        }
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        // Extended frame bounds exclude invisible resize borders, so a normal
        // maximized window is not mistaken for fullscreen because of its shadow.
        let frame = unsafe {
            DwmGetWindowAttribute(
                hwnd,
                DWMWA_EXTENDED_FRAME_BOUNDS as u32,
                (&mut rect as *mut RECT).cast(),
                std::mem::size_of::<RECT>() as u32,
            )
        };
        if frame < 0 && unsafe { GetWindowRect(hwnd, &mut rect) } == 0 {
            return false;
        }
        let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            rcMonitor: rect,
            rcWork: rect,
            dwFlags: 0,
        };
        if unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 {
            return false;
        }
        rect.left <= info.rcMonitor.left
            && rect.top <= info.rcMonitor.top
            && rect.right >= info.rcMonitor.right
            && rect.bottom >= info.rcMonitor.bottom
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_lowercase_basename() {
        assert_eq!(
            exe_name(r"C:\Games\Some Game\GAME.EXE").as_deref(),
            Some("game.exe")
        );
        assert_eq!(
            exe_name("C:/Windows/Explorer.EXE").as_deref(),
            Some("explorer.exe")
        );
        assert_eq!(exe_name("game.exe").as_deref(), Some("game.exe"));
        assert_eq!(exe_name(""), None);
        assert_eq!(exe_name("C:\\"), None);
    }

    #[test]
    fn emits_initial_value_and_only_changes() {
        let mut previous = None;
        assert!(changed(&mut previous, "game.exe"));
        assert!(!changed(&mut previous, "game.exe"));
        assert!(changed(&mut previous, "explorer.exe"));
        assert!(changed(&mut previous, "game.exe"));
        let mut power = None;
        assert!(changed(&mut power, false));
        assert!(!changed(&mut power, false));
        assert!(changed(&mut power, true));
    }

    #[test]
    fn distinguishes_battery_ac_absent_and_unknown() {
        assert_eq!(battery_source(0, 1), Some(true));
        assert_eq!(battery_source(1, 8), Some(false));
        assert_eq!(battery_source(1, 128), None);
        assert_eq!(battery_source(0, 255), None);
        assert_eq!(battery_source(255, 1), None);
    }
}
