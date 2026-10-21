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
/// `DISPLAYCONFIG_SET_HDR_STATE` (Windows 11 24H2+) and
/// `DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE`: one flag in bit 0.
#[repr(C)]
struct SetFlag {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    value: u32,
}
#[repr(C)]
struct SetWhite {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    level: u32,
    final_value: u8,
}

/// Advanced color state of a display.
struct ColorState {
    /// `DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO_2` answered (Windows 11 24H2+),
    /// which tells HDR apart from WCG/ACM.
    modern: bool,
    supported: bool,
    active: bool,
}
fn color_state(target: &Target) -> ColorState {
    unsafe {
        let mut info = ColorInfo2 {
            header: header::<ColorInfo2>(
                DISPLAYCONFIG_DEVICE_INFO_TYPE(15),
                target.adapter,
                target.target,
            ),
            ..Default::default()
        };
        if DisplayConfigGetDeviceInfo(&mut info.header) == 0 {
            return ColorState {
                modern: true,
                // highDynamicRangeSupported
                supported: info.flags & (1 << 4) != 0,
                active: info.mode == 2,
            };
        }
        let mut old = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO {
            header: header::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>(
                DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
                target.adapter,
                target.target,
            ),
            ..Default::default()
        };
        let ok = DisplayConfigGetDeviceInfo(&mut old.header) == 0;
        let value = old.Anonymous.value;
        ColorState {
            modern: false,
            // advancedColorSupported, advancedColorEnabled
            supported: ok && value & 1 != 0,
            active: ok && value & 2 != 0,
        }
    }
}
/// The display can switch HDR on.
pub(super) fn supported(target: &Target) -> bool {
    color_state(target).supported
}
/// Switch HDR on or off, like the toggle in Windows display settings.
pub(super) fn enable(target: &Target, on: bool) -> bool {
    // SET_HDR_STATE leaves ACM alone; older Windows only has the advanced
    // color switch.
    let kind = if color_state(target).modern { 16 } else { 10 };
    let params = SetFlag {
        header: header::<SetFlag>(
            DISPLAYCONFIG_DEVICE_INFO_TYPE(kind),
            target.adapter,
            target.target,
        ),
        value: u32::from(on),
    };
    unsafe { DisplayConfigSetDeviceInfo(&params.header) == 0 }
}
pub(super) fn read(target: &Target) -> Option<SdrLevel> {
    let ColorState { modern, active, .. } = color_state(target);
    if !active {
        return None;
    }
    unsafe {
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
