use super::{
    SdrLevel,
    enumerate::{Target, header},
    rules,
};
use windows::Win32::Devices::Display::*;

#[repr(C)]
#[derive(Default)]
struct ColorInfo2 {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    flags: u32,
    encoding: u32,
    bits: u32,
    mode: u32,
}
#[repr(C)]
struct SetWhite {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    level: u32,
    final_value: u8,
}

pub(super) fn read(target: &Target) -> Option<SdrLevel> {
    unsafe {
        let mut info = ColorInfo2 {
            header: header::<ColorInfo2>(
                DISPLAYCONFIG_DEVICE_INFO_TYPE(15),
                target.adapter,
                target.target,
            ),
            ..Default::default()
        };
        let modern = DisplayConfigGetDeviceInfo(&mut info.header) == 0;
        let active = if modern {
            info.mode == 2
        } else {
            let mut old = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO {
                header: header::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>(
                    DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
                    target.adapter,
                    target.target,
                ),
                ..Default::default()
            };
            DisplayConfigGetDeviceInfo(&mut old.header) == 0 && old.Anonymous.value & 2 != 0
        };
        if !active {
            return None;
        }
        let mut white = DISPLAYCONFIG_SDR_WHITE_LEVEL {
            header: header::<DISPLAYCONFIG_SDR_WHITE_LEVEL>(
                DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL,
                target.adapter,
                target.target,
            ),
            ..Default::default()
        };
        if DisplayConfigGetDeviceInfo(&mut white.header) != 0 {
            return None;
        }
        // On older Windows advancedColorEnabled also includes WCG/ACM.
        // Legacy distinguishes real HDR by reapplying the current white level.
        if !modern && !set_raw(target, white.SDRWhiteLevel) {
            return None;
        }
        Some(SdrLevel {
            percent: rules::sdr_percent(white.SDRWhiteLevel),
        })
    }
}
pub(super) fn set(target: &Target, percent: u8) -> bool {
    set_raw(target, rules::sdr_raw(percent))
}
fn set_raw(target: &Target, raw: u32) -> bool {
    let params = SetWhite {
        header: header::<SetWhite>(
            DISPLAYCONFIG_DEVICE_INFO_TYPE(-18),
            target.adapter,
            target.target,
        ),
        level: raw,
        final_value: 1,
    };
    unsafe { DisplayConfigSetDeviceInfo(&params.header) == 0 }
}
