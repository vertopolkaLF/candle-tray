//! HKCU startup registration. Reconciliation adopts installer registrations and
//! Task Manager's StartupApproved disabled state before applying settings.
use crate::settings::Settings;
use anyhow::Result;

pub fn reconcile(settings: &mut Settings) -> Result<bool> {
    let before = settings.launch_at_startup;
    if platform::disabled()? {
        settings.launch_at_startup = false;
    } else if platform::registered()? {
        settings.launch_at_startup = true;
    } else {
        set_enabled(settings.launch_at_startup)?;
    }
    Ok(before != settings.launch_at_startup)
}
pub fn set_enabled(enabled: bool) -> Result<()> {
    platform::set_enabled(enabled)
}
pub fn is_enabled() -> Result<bool> {
    Ok(platform::registered()? && !platform::disabled()?)
}

#[cfg(windows)]
mod platform {
    use anyhow::{Context, Result, ensure};
    use std::ptr;
    use windows_sys::Win32::{
        Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS},
        System::Registry::*,
    };
    const RUN: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
    const APPROVED: &str =
        "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run";
    const NAME: &str = "Candle Tray";
    struct Key(HKEY);
    impl Drop for Key {
        fn drop(&mut self) {
            unsafe {
                RegCloseKey(self.0);
            }
        }
    }
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }
    fn read(path: &str, flags: u32) -> Result<Option<Vec<u8>>> {
        let mut size = 0;
        let path = wide(path);
        let name = wide(NAME);
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                path.as_ptr(),
                name.as_ptr(),
                flags,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut size,
            )
        };
        if matches!(status, ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND) {
            return Ok(None);
        }
        ensure!(
            status == ERROR_SUCCESS,
            "read startup registration: {status}"
        );
        let mut data = vec![0; size as usize];
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                path.as_ptr(),
                name.as_ptr(),
                flags,
                ptr::null_mut(),
                data.as_mut_ptr().cast(),
                &mut size,
            )
        };
        ensure!(
            status == ERROR_SUCCESS,
            "read startup registration data: {status}"
        );
        data.truncate(size as usize);
        Ok(Some(data))
    }
    pub fn registered() -> Result<bool> {
        Ok(read(RUN, RRF_RT_REG_SZ)?.is_some())
    }
    pub fn disabled() -> Result<bool> {
        Ok(read(APPROVED, RRF_RT_REG_BINARY)?
            .is_some_and(|data| matches!(data.first(), Some(3 | 7))))
    }
    pub fn set_enabled(enabled: bool) -> Result<()> {
        let command = if enabled {
            Some(wide(&format!(
                "\"{}\"",
                std::env::current_exe()
                    .context("resolve startup executable")?
                    .display()
            )))
        } else {
            None
        };
        let mut raw = ptr::null_mut();
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                wide(RUN).as_ptr(),
                0,
                ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                ptr::null(),
                &mut raw,
                ptr::null_mut(),
            )
        };
        ensure!(
            status == ERROR_SUCCESS,
            "open startup registry key: {status}"
        );
        let key = Key(raw);
        let name = wide(NAME);
        let status = if let Some(command) = command {
            unsafe {
                RegSetValueExW(
                    key.0,
                    name.as_ptr(),
                    0,
                    REG_SZ,
                    command.as_ptr().cast(),
                    (command.len() * 2) as u32,
                )
            }
        } else {
            unsafe { RegDeleteValueW(key.0, name.as_ptr()) }
        };
        ensure!(
            status == ERROR_SUCCESS || (!enabled && status == ERROR_FILE_NOT_FOUND),
            "set startup registration: {status}"
        );
        // Explicit user changes clear the old Task Manager decision. Reconcile
        // never calls this when it discovers a disabled entry.
        let mut raw = ptr::null_mut();
        let status = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                wide(APPROVED).as_ptr(),
                0,
                KEY_SET_VALUE,
                &mut raw,
            )
        };
        if matches!(status, ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND) {
            return Ok(());
        }
        ensure!(status == ERROR_SUCCESS, "open StartupApproved: {status}");
        let approved = Key(raw);
        let status = unsafe { RegDeleteValueW(approved.0, name.as_ptr()) };
        ensure!(
            matches!(status, ERROR_SUCCESS | ERROR_FILE_NOT_FOUND),
            "clear StartupApproved: {status}"
        );
        Ok(())
    }
}
#[cfg(not(windows))]
mod platform {
    use anyhow::Result;
    pub fn registered() -> Result<bool> {
        Ok(false)
    }
    pub fn disabled() -> Result<bool> {
        Ok(false)
    }
    pub fn set_enabled(_: bool) -> Result<()> {
        anyhow::bail!("startup registration requires Windows")
    }
}
