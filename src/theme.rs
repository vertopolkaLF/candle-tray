//! Shared animation and accent tokens for the GPUI popup and Settings window.

use std::time::Duration;

/// Fluent `ControlFasterAnimationDuration` — pointer-over / micro-interactions.
pub const CONTROL_FASTER_ANIMATION: Duration = Duration::from_millis(83);
/// Fluent `ControlFastAnimationDuration`.
pub const CONTROL_FAST_ANIMATION: Duration = Duration::from_millis(167);
/// Fluent `ControlNormalAnimationDuration`.
pub const CONTROL_NORMAL_ANIMATION: Duration = Duration::from_millis(250);

/// Respect the Windows "Animation effects" accessibility preference.
pub fn animations_enabled() -> bool {
    crate::popup::system_animations_enabled()
}

pub fn duration(duration: Duration) -> Duration {
    if animations_enabled() {
        duration
    } else {
        Duration::ZERO
    }
}

/// Accent ramp shared by every UI surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccentRamp {
    pub base: (u8, u8, u8),
    pub light1: (u8, u8, u8),
    pub light2: (u8, u8, u8),
    pub light3: (u8, u8, u8),
    pub dark1: (u8, u8, u8),
    pub dark2: (u8, u8, u8),
    pub dark3: (u8, u8, u8),
}

fn tone((r, g, b): (u8, u8, u8), amount: f64, lighter: bool) -> (u8, u8, u8) {
    let target = if lighter { 255.0 } else { 0.0 };
    let channel = |value: u8| {
        (f64::from(value) + (target - f64::from(value)) * amount)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    (channel(r), channel(g), channel(b))
}

impl AccentRamp {
    pub fn from_base(base: (u8, u8, u8)) -> Self {
        Self {
            base,
            light1: tone(base, 0.25, true),
            light2: base,
            light3: tone(base, 0.70, true),
            dark1: base,
            dark2: tone(base, 0.45, false),
            dark3: tone(base, 0.70, false),
        }
    }

    /// Fill role (`AccentFillColorDefaultBrush`) for the resolved theme.
    pub const fn fill(self, dark: bool) -> (u8, u8, u8) {
        if dark { self.light2 } else { self.dark1 }
    }

    /// Text role (`AccentTextFillColorPrimaryBrush`) for the resolved theme.
    pub const fn text(self, dark: bool) -> (u8, u8, u8) {
        if dark { self.light3 } else { self.dark2 }
    }
}

/// The Windows accent ramp, read fresh on every call (it can change at any
/// time while the app runs).
pub fn accent_ramp() -> AccentRamp {
    system_accent_ramp().unwrap_or_else(|| AccentRamp::from_base((0, 120, 212)))
}

#[cfg(windows)]
fn system_accent_ramp() -> Option<AccentRamp> {
    use windows::UI::ViewManagement::{UIColorType, UISettings};

    let settings = UISettings::new().ok()?;
    let rgb = |kind| {
        settings
            .GetColorValue(kind)
            .ok()
            .map(|color| (color.R, color.G, color.B))
    };
    Some(AccentRamp {
        base: rgb(UIColorType::Accent)?,
        light1: rgb(UIColorType::AccentLight1)?,
        light2: rgb(UIColorType::AccentLight2)?,
        light3: rgb(UIColorType::AccentLight3)?,
        dark1: rgb(UIColorType::AccentDark1)?,
        dark2: rgb(UIColorType::AccentDark2)?,
        dark3: rgb(UIColorType::AccentDark3)?,
    })
}

#[cfg(not(windows))]
fn system_accent_ramp() -> Option<AccentRamp> {
    None
}

/// `SystemUsesLightTheme`: the taskbar's own theme, which the tray icon
/// must contrast with. Unreadable means light.
#[cfg(windows)]
pub fn taskbar_uses_light_theme() -> bool {
    read_personalize_dword("SystemUsesLightTheme").is_none_or(|value| value != 0)
}

#[cfg(not(windows))]
pub fn taskbar_uses_light_theme() -> bool {
    true
}

#[cfg(windows)]
fn read_personalize_dword(name: &str) -> Option<u32> {
    use windows_sys::Win32::{
        Foundation::ERROR_SUCCESS,
        System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW},
    };

    let wide = |text: &str| text.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let subkey = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let value_name = wide(name);
    let mut value = 0u32;
    let mut size = size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            value_name.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&mut value as *mut u32).cast(),
            &mut size,
        )
    };
    (status == ERROR_SUCCESS).then_some(value)
}
