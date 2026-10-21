//! Each worker owns its physical handle; destruction never races an in-flight call.
use super::{
    enumerate::{Target, wide},
    rules,
};
use windows::{
    Win32::{Devices::Display::*, Foundation::*, Graphics::Gdi::*},
    core::{BOOL, PCWSTR},
};

pub(super) struct Ddc(HANDLE);
impl Drop for Ddc {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyPhysicalMonitor(self.0);
        }
    }
}

unsafe extern "system" fn collect(h: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
    unsafe {
        (&mut *(data.0 as *mut Vec<HMONITOR>)).push(h);
    }
    BOOL(1)
}

impl Ddc {
    pub fn acquire(target: &Target, targets: &[Target]) -> Option<Self> {
        unsafe {
            let mut logical = Vec::<HMONITOR>::new();
            let _ = EnumDisplayMonitors(
                None,
                None,
                Some(collect),
                LPARAM(&mut logical as *mut _ as isize),
            );
            for h in logical {
                let mut info = MONITORINFOEXW::default();
                info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
                if !GetMonitorInfoW(h, &mut info as *mut _ as *mut MONITORINFO).as_bool()
                    || wide(&info.szDevice) != target.source
                {
                    continue;
                }
                let mut count = 0;
                if GetNumberOfPhysicalMonitorsFromHMONITOR(h, &mut count).is_err() || count == 0 {
                    continue;
                }
                let mut physical = vec![PHYSICAL_MONITOR::default(); count as usize];
                if GetPhysicalMonitorsFromHMONITOR(h, &mut physical).is_err() {
                    continue;
                }
                let source_w: Vec<_> = target.source.encode_utf16().chain(Some(0)).collect();
                let mut devices = Vec::new();
                for index in 0.. {
                    let mut d = DISPLAY_DEVICEW {
                        cb: size_of::<DISPLAY_DEVICEW>() as u32,
                        ..Default::default()
                    };
                    if !EnumDisplayDevicesW(PCWSTR(source_w.as_ptr()), index, &mut d, 1).as_bool() {
                        break;
                    }
                    if d.StateFlags.contains(DISPLAY_DEVICE_ATTACHED_TO_DESKTOP) {
                        devices.push(wide(&d.DeviceID));
                    }
                }
                let candidates: Vec<_> = targets
                    .iter()
                    .filter(|t| t.source == target.source)
                    .collect();
                let descriptions: Vec<_> = physical
                    .iter()
                    .map(|p| {
                        let description = p.szPhysicalMonitorDescription;
                        wide(&description)
                    })
                    .collect();
                let identities: Vec<_> = candidates
                    .iter()
                    .map(|t| (t.monitor.id.clone(), t.monitor.name.clone()))
                    .collect();
                let matches = rules::match_physical(&descriptions, &identities, &devices);
                let mut result = None;
                for (p, key) in physical.into_iter().zip(matches) {
                    let owned = Self(p.hPhysicalMonitor);
                    if key.as_deref() == Some(&target.monitor.id) {
                        result = Some(owned);
                    }
                }
                return result;
            }
            None
        }
    }
    fn retry(&self, mut operation: impl FnMut() -> i32) -> bool {
        for attempt in 0..3 {
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            if operation() != 0 {
                return true;
            }
            // Retry only the legacy transport/checksum failures, never unsupported codes.
            let error = unsafe { GetLastError().0 };
            if !matches!(
                error,
                0xc0262582 | 0xc0262583 | 0xc0262586 | 0xc0262589 | 0xc026258a | 0xc026258b
            ) {
                break;
            }
        }
        false
    }
    pub fn read(&self, code: u8) -> Option<(u32, u32)> {
        let (mut current, mut max) = (0, 0);
        self.retry(|| unsafe {
            GetVCPFeatureAndVCPFeatureReply(self.0, code, None, &mut current, Some(&mut max))
        })
        .then_some((current, max))
        .filter(|(_, max)| *max > 0)
    }
    pub fn set(&self, code: u8, value: u32) -> bool {
        self.retry(|| unsafe { SetVCPFeature(self.0, code, value) })
    }
    /// The MCCS capabilities string, e.g. `(prot(monitor)...vcp(10 12 60(0F 11))...)`.
    pub fn capabilities(&self) -> Option<String> {
        let mut len = 0;
        // Some monitors fail this request even without a transient transport
        // error. Legacy deliberately retries the length query three times.
        let found = (0..3).any(|attempt| {
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            unsafe { GetCapabilitiesStringLength(self.0, &mut len) != 0 }
        });
        if !found || len == 0 || len > 1024 * 1024 {
            return None;
        }
        let mut buffer = vec![0; len as usize];
        self.retry(|| unsafe { CapabilitiesRequestAndCapabilitiesReply(self.0, &mut buffer) })
            .then(|| String::from_utf8_lossy(&buffer).into_owned())
    }
}
