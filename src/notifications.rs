//! Native Windows update toasts. Protocol actions also work from Action Center
//! after the original process exits, through the existing single-instance IPC.
use anyhow::Result;

pub const INSTALL_URI_PREFIX: &str = "candle-tray://install-update/";
pub const SHOW_URI: &str = "candle-tray://show";

pub fn update_available(update: &crate::updater::AvailableUpdate) -> Result<()> {
    show(&content(Some(update), &update.version), "update-available")
}

pub fn update_complete(version: &str) -> Result<()> {
    show(&content(None, version), "update-complete")
}

#[cfg(any(windows, test))]
fn icon_uri(path: &str) -> String {
    use std::fmt::Write;
    let path = path.replace('\\', "/");
    let mut uri = if path.starts_with("//") {
        "file:"
    } else {
        "file:///"
    }
    .to_owned();
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"/:.-_~".contains(&byte) {
            uri.push(char::from(byte));
        } else {
            let _ = write!(uri, "%{byte:02X}");
        }
    }
    uri
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn content(update: Option<&crate::updater::AvailableUpdate>, version: &str) -> String {
    use crate::i18n::{format, tr};
    let (title, body) = if update.is_some() {
        (
            tr("notification-update-title"),
            format(
                "notification-update-body",
                &[("version", version.to_owned())],
            ),
        )
    } else {
        (
            tr("notification-updated-title"),
            format(
                "notification-updated-body",
                &[("version", version.to_owned())],
            ),
        )
    };
    let actions = update.map_or_else(String::new, |update| std::format!(
        r#"<actions><action content="{}" activationType="protocol" arguments="{}{}"/><action content="{}" activationType="protocol" arguments="{}"/></actions>"#,
        escape(&tr("notification-install-update")), INSTALL_URI_PREFIX, escape(version),
        escape(&tr("notification-view-release")), escape(&update.html_url),
    ));
    // The image path is filled after the embedded icon has been materialized.
    std::format!(
        r#"<toast activationType="protocol" launch="{SHOW_URI}"><visual><binding template="ToastGeneric"><image placement="appLogoOverride" src="{{icon}}"/><text>{}</text><text>{}</text></binding></visual>{actions}</toast>"#,
        escape(&title),
        escape(&body)
    )
}

#[cfg(not(windows))]
fn show(_xml: &str, _tag: &str) -> Result<()> {
    anyhow::bail!("update notifications are only supported on Windows")
}

#[cfg(windows)]
fn show(xml: &str, tag: &str) -> Result<()> {
    static SHOW_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _show = SHOW_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    use windows::{
        Data::Xml::Dom::XmlDocument,
        UI::Notifications::{ToastNotification, ToastNotificationManager},
        Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize},
    };
    use windows_core::HSTRING;
    // All callers run on workers. Balance even S_FALSE; never disturb GPUI's COM apartment.
    unsafe { RoInitialize(RO_INIT_MULTITHREADED).ok()? };
    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe { RoUninitialize() };
        }
    }
    let _apartment = Apartment;
    let icon = register()?;
    let document = XmlDocument::new()?;
    document.LoadXml(&HSTRING::from(xml.replace("{icon}", &escape(&icon))))?;
    let toast = ToastNotification::CreateToastNotification(&document)?;
    toast.SetTag(&HSTRING::from(tag))?;
    toast.SetGroup(&HSTRING::from("updates"))?;
    ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(AUMID))?.Show(&toast)?;
    Ok(())
}

#[cfg(windows)]
const AUMID: &str = "dev.CandleTray";

/// Register a per-user identity for installed AND portable copies. A stub
/// activator CLSID keeps toasts in Action Center; every action uses a protocol.
#[cfg(windows)]
fn register() -> Result<String> {
    use anyhow::{Context, ensure};
    use std::{fs, mem::ManuallyDrop};
    use windows::Win32::{
        Foundation::PROPERTYKEY,
        System::{
            Com::{
                CLSCTX_INPROC_SERVER, CoCreateInstance, IPersistFile,
                StructuredStorage::{
                    PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
                },
            },
            Variant::{VT_CLSID, VT_LPWSTR},
        },
        UI::Shell::{IShellLinkW, PropertiesSystem::IPropertyStore, ShellLink},
    };
    use windows_core::{GUID, HSTRING, Interface, PWSTR};
    let exe = std::env::current_exe().context("resolve notification executable")?;
    let roaming = directories::BaseDirs::new().context("resolve application data")?;
    let dir = roaming.data_local_dir().join("Candle Tray");
    fs::create_dir_all(&dir)?;
    let icon = dir.join("notification-icon.png");
    fs::write(&icon, include_bytes!("../assets/app-icon.png"))?;
    register_protocol(&exe)?;

    let shortcut = roaming
        .data_dir()
        .join("Microsoft/Windows/Start Menu/Programs/Candle Tray/Candle Tray.lnk");
    fs::create_dir_all(shortcut.parent().context("shortcut parent")?)?;
    // SAFETY: COM is initialized by show; all interface and borrowed variant
    // values remain valid until SetValue copies them. Borrowed variants must
    // not be passed to PropVariantClear.
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        let exe = HSTRING::from(exe.as_os_str());
        link.SetPath(&exe).ok()?;
        link.SetIconLocation(&exe, 0).ok()?;
        let properties: IPropertyStore = link.cast()?;
        let fmtid = GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3);
        let mut id: Vec<u16> = AUMID.encode_utf16().chain([0]).collect();
        let id_value = PROPVARIANT {
            Anonymous: PROPVARIANT_0 {
                Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                    vt: VT_LPWSTR,
                    Anonymous: PROPVARIANT_0_0_0 {
                        pwszVal: PWSTR(id.as_mut_ptr()),
                    },
                    ..Default::default()
                }),
            },
        };
        properties
            .SetValue(&PROPERTYKEY { fmtid, pid: 5 }, &id_value)
            .ok()?;
        let mut clsid = GUID::from_u128(0x1040b758_061b_4688_b632_d86770c71f29);
        let clsid_value = PROPVARIANT {
            Anonymous: PROPVARIANT_0 {
                Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                    vt: VT_CLSID,
                    Anonymous: PROPVARIANT_0_0_0 { puuid: &mut clsid },
                    ..Default::default()
                }),
            },
        };
        properties
            .SetValue(&PROPERTYKEY { fmtid, pid: 26 }, &clsid_value)
            .ok()?;
        properties.Commit().ok()?;
        let persist: IPersistFile = link.cast()?;
        persist
            .Save(&HSTRING::from(shortcut.as_os_str()), true)
            .ok()?;
    }
    ensure!(icon.is_absolute(), "notification icon must be absolute");
    Ok(icon_uri(&icon.to_string_lossy()))
}

#[cfg(windows)]
fn register_protocol(exe: &std::path::Path) -> Result<()> {
    use anyhow::ensure;
    use windows_sys::Win32::System::Registry::*;
    let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain([0]).collect() };
    for (subkey, name, value) in [
        ("", "", "URL:Candle Tray".to_owned()),
        ("", "URL Protocol", String::new()),
        (
            "\\shell\\open\\command",
            "",
            format!("\"{}\" \"%1\"", exe.display()),
        ),
    ] {
        let mut key = std::ptr::null_mut();
        let path = wide(&format!("Software\\Classes\\candle-tray{subkey}"));
        // SAFETY: NUL-terminated UTF-16 input and valid output pointers.
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                path.as_ptr(),
                0,
                std::ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                std::ptr::null(),
                &mut key,
                std::ptr::null_mut(),
            )
        };
        ensure!(status == 0, "register notification protocol: {status}");
        let value = wide(&value);
        let status = unsafe {
            RegSetValueExW(
                key,
                wide(name).as_ptr(),
                0,
                REG_SZ,
                value.as_ptr().cast(),
                (value.len() * 2) as u32,
            )
        };
        unsafe { RegCloseKey(key) };
        ensure!(status == 0, "write notification protocol: {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn icon_paths_are_valid_file_uris() {
        assert_eq!(
            icon_uri(r"C:\Users\Test User\a#b.png"),
            "file:///C:/Users/Test%20User/a%23b.png"
        );
        assert_eq!(
            icon_uri(r"\\server\share\a.png"),
            "file://server/share/a.png"
        );
        assert_eq!(icon_uri(r"C:\Ю\a.png"), "file:///C:/%D0%AE/a.png");
    }
    #[test]
    fn toast_actions_use_versioned_install_protocol_and_escaped_release_url() {
        let update = crate::updater::AvailableUpdate {
            version: "1.2.3".into(),
            asset_url: String::new(),
            html_url: "https://example.com/?a=1&b=\"2\"".into(),
            size: None,
            sha256: None,
        };
        let xml = content(Some(&update), &update.version);
        assert!(xml.contains("arguments=\"candle-tray://install-update/1.2.3\""));
        assert!(xml.contains("https://example.com/?a=1&amp;b=&quot;2&quot;"));
        assert_eq!(xml.matches("<action ").count(), 2);
        assert!(xml.contains("appLogoOverride"));
        let success = content(None, "1.2.3");
        assert!(!success.contains("<actions>"));
        assert!(success.contains("1.2.3"));
    }
}
