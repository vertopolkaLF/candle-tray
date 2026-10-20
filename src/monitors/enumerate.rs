use super::{BrightnessBackend, Monitor, MonitorKind, Rect, rules};
use windows::Win32::{Devices::Display::*, Foundation::*};

#[derive(Clone)]
pub(super) struct Target {
    pub monitor: Monitor,
    pub adapter: LUID,
    pub target: u32,
    pub source: String,
}

pub(super) fn wide(value: &[u16]) -> String {
    String::from_utf16_lossy(&value[..value.iter().position(|v| *v == 0).unwrap_or(value.len())])
}

pub(super) fn header<T>(
    kind: DISPLAYCONFIG_DEVICE_INFO_TYPE,
    adapter: LUID,
    id: u32,
) -> DISPLAYCONFIG_DEVICE_INFO_HEADER {
    DISPLAYCONFIG_DEVICE_INFO_HEADER {
        r#type: kind,
        size: size_of::<T>() as u32,
        adapterId: adapter,
        id,
    }
}

pub(super) fn enumerate() -> windows::core::Result<Vec<Target>> {
    unsafe {
        let (mut count, mut mode_count) = (0, 0);
        let (mut paths, mut modes);
        loop {
            GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut count, &mut mode_count).ok()?;
            paths = vec![DISPLAYCONFIG_PATH_INFO::default(); count as usize];
            modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
            let status = QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut count,
                paths.as_mut_ptr(),
                &mut mode_count,
                modes.as_mut_ptr(),
                None,
            );
            if status == ERROR_INSUFFICIENT_BUFFER {
                continue;
            }
            status.ok()?;
            paths.truncate(count as usize);
            modes.truncate(mode_count as usize);
            break;
        }
        let mut targets = Vec::new();
        for path in paths {
            let mut name = DISPLAYCONFIG_TARGET_DEVICE_NAME {
                header: header::<DISPLAYCONFIG_TARGET_DEVICE_NAME>(
                    DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
                    path.targetInfo.adapterId,
                    path.targetInfo.id,
                ),
                ..Default::default()
            };
            let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
                header: header::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>(
                    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                    path.sourceInfo.adapterId,
                    path.sourceInfo.id,
                ),
                ..Default::default()
            };
            // Never return a partial target list: positional DDC fallback would shift.
            windows::core::WIN32_ERROR(DisplayConfigGetDeviceInfo(&mut name.header) as u32).ok()?;
            windows::core::WIN32_ERROR(DisplayConfigGetDeviceInfo(&mut source.header) as u32)
                .ok()?;
            let device_path = wide(&name.monitorDevicePath);
            let Some((model, id)) = rules::identity(&device_path) else {
                continue;
            };
            let source = wide(&source.viewGdiDeviceName);
            let num = source
                .trim_start_matches(r"\\.\DISPLAY")
                .parse()
                .unwrap_or(path.sourceInfo.id + 1);
            let bounds = modes
                .iter()
                .find(|m| {
                    m.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE
                        && m.id == path.sourceInfo.id
                        && m.adapterId == path.sourceInfo.adapterId
                })
                .map(|m| {
                    let s = m.Anonymous.sourceMode;
                    Rect {
                        x: s.position.x,
                        y: s.position.y,
                        width: s.width as i32,
                        height: s.height as i32,
                    }
                });
            let internal = matches!(
                path.targetInfo.outputTechnology,
                DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL
                    | DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED
                    | DISPLAYCONFIG_OUTPUT_TECHNOLOGY_UDI_EMBEDDED
                    | DISPLAYCONFIG_OUTPUT_TECHNOLOGY_LVDS
            );
            let mut friendly = wide(&name.monitorFriendlyDeviceName);
            if friendly.is_empty() {
                friendly = format!("Display {num}");
            }
            targets.push(Target {
                adapter: path.targetInfo.adapterId,
                target: path.targetInfo.id,
                source,
                monitor: Monitor {
                    id,
                    model,
                    device_path,
                    name: friendly,
                    kind: if internal {
                        MonitorKind::Internal
                    } else {
                        MonitorKind::External
                    },
                    num,
                    bounds,
                    backend: BrightnessBackend::Unsupported,
                    brightness: None,
                    sdr: None,
                    contrast: None,
                    supports_ddc_power: false,
                    powered_off: false,
                    unresponsive: false,
                },
            });
        }
        targets.sort_by_key(|t| t.monitor.num);
        let mut seen = std::collections::HashSet::new();
        targets.retain(|t| seen.insert(t.monitor.id.clone()));
        Ok(targets)
    }
}
