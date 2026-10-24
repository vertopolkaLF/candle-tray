//! Each worker owns its physical handle; destruction never races an in-flight call.
use super::{
    enumerate::{Target, wide},
    log, rules,
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
                log(format!(
                    "DDC handles on {}: physical={descriptions:?} devices={devices:?} matched={matches:?}",
                    target.source
                ));
                let mut result = None;
                for (p, key) in physical.into_iter().zip(matches) {
                    let owned = Self(p.hPhysicalMonitor);
                    if key.as_deref() == Some(&target.monitor.id) {
                        result = Some(owned);
                    }
                }
                if result.is_none() {
                    log("DDC handle not matched; display has no DDC/CI control");
                }
                return result;
            }
            log(format!("DDC: no logical monitor for {}", target.source));
            None
        }
    }
    /// Runs a DXVA2 call with the legacy retries; logs the outcome of `what`.
    fn retry(&self, what: &str, mut operation: impl FnMut() -> i32) -> bool {
        let started = std::time::Instant::now();
        let mut error = 0;
        let mut attempts = 0;
        for attempt in 0..3 {
            attempts = attempt + 1;
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            if operation() != 0 {
                log(format!(
                    "DDC {what} ok ({attempts} attempt(s), {} ms)",
                    started.elapsed().as_millis()
                ));
                return true;
            }
            // Retry only the legacy transport/checksum failures, never unsupported codes.
            error = unsafe { GetLastError().0 };
            if !matches!(
                error,
                0xc0262582 | 0xc0262583 | 0xc0262586 | 0xc0262589 | 0xc026258a | 0xc026258b
            ) {
                break;
            }
        }
        log(format!(
            "DDC {what} failed: error 0x{error:08X} ({attempts} attempt(s), {} ms)",
            started.elapsed().as_millis()
        ));
        false
    }
    pub fn read(&self, code: u8) -> Option<(u32, u32)> {
        self.read_raw(code).filter(|(_, max)| *max > 0)
    }
    /// Discrete VCP features may report no meaningful maximum. Raw actions
    /// need their current value without the percentage reader's max check.
    pub fn read_raw(&self, code: u8) -> Option<(u32, u32)> {
        let (mut current, mut max) = (0, 0);
        let ok = self.retry(&format!("read 0x{code:02X}"), || unsafe {
            GetVCPFeatureAndVCPFeatureReply(self.0, code, None, &mut current, Some(&mut max))
        });
        if ok {
            log(format!("DDC read 0x{code:02X} = {current}/{max}"));
        }
        ok.then_some((current, max))
    }
    pub fn set(&self, code: u8, value: u32) -> bool {
        self.retry(&format!("write 0x{code:02X} = {value}"), || unsafe {
            SetVCPFeature(self.0, code, value)
        })
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
            log(format!(
                "DDC capabilities length unavailable (found={found}, length={len})"
            ));
            return None;
        }
        let mut buffer = vec![0; len as usize];
        let capabilities = self
            .retry("capabilities", || unsafe {
                CapabilitiesRequestAndCapabilitiesReply(self.0, &mut buffer)
            })
            .then(|| String::from_utf8_lossy(&buffer).into_owned());
        if let Some(capabilities) = &capabilities {
            log(format!(
                "DDC capabilities: {}",
                capabilities.trim_end_matches('\0')
            ));
        }
        capabilities
    }
}
