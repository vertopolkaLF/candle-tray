//! Fluent color tokens for the GPUI popup.
//!
//! Values mirror the WinUI 3 theme dictionaries, so cards, strokes and text
//! keep their exact contrast. The accent roles follow the Fluent mapping
//! shared with the Settings window (fill = Light2/Dark1, text = Light3/Dark2).

use gpui::{Hsla, Rgba, SharedString};

use crate::settings::{BackdropBrightness, PopupMaterial, Theme};

pub(crate) fn rgba8(r: u8, g: u8, b: u8, a: u8) -> Hsla {
    Rgba {
        r: f32::from(r) / 255.0,
        g: f32::from(g) / 255.0,
        b: f32::from(b) / 255.0,
        a: f32::from(a) / 255.0,
    }
    .into()
}

pub(crate) fn rgb8((r, g, b): (u8, u8, u8)) -> Hsla {
    rgba8(r, g, b, 255)
}

/// The default UI family: Segoe UI Variable on Windows 11, Segoe UI on older
/// systems that do not ship the variable font.
pub(crate) fn default_font_family(cx: &gpui::App) -> SharedString {
    let names = cx.text_system().all_font_names();
    ["Segoe UI Variable Text", "Segoe UI Variable", "Segoe UI"]
        .into_iter()
        .find(|candidate| names.iter().any(|name| name == candidate))
        .unwrap_or("Segoe UI")
        .into()
}

/// Black/white with a WinUI-style alpha byte.
fn ink(dark: bool, alpha: u8) -> Hsla {
    if dark {
        rgba8(255, 255, 255, alpha)
    } else {
        rgba8(0, 0, 0, alpha)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Palette {
    pub(crate) dark: bool,
    pub(crate) card_radius: f32,
    pub(crate) control_radius: f32,
    pub(crate) card_gap: f32,
    pub(crate) font_family: SharedString,
    pub(crate) text_primary: Hsla,
    pub(crate) text_secondary: Hsla,
    pub(crate) text_tertiary: Hsla,
    pub(crate) text_on_accent: Hsla,
    pub(crate) accent: Hsla,
    pub(crate) accent_text: Hsla,
    pub(crate) card_background: Hsla,
    pub(crate) card_stroke: Hsla,
    pub(crate) subtle_fill: Hsla,
    pub(crate) control_fill: Hsla,
    /// Slider rail (`ControlStrongFillColorDefault`).
    pub(crate) control_strong_fill: Hsla,
    /// Slider thumb outer disc (`ControlSolidFillColorDefault`).
    pub(crate) control_solid: Hsla,
    /// Thumb edge (`ControlElevationBorderBrush`).
    pub(crate) control_elevation_border: Hsla,
    pub(crate) divider: Hsla,
    pub(crate) solid_background: Hsla,
    pub(crate) footer_background: Hsla,
    pub(crate) caution: Hsla,
    pub(crate) chrome_icon: Hsla,
    pub(crate) chrome_icon_hover: Hsla,
    pub(crate) material: PopupMaterial,
    pub(crate) backdrop_brightness: BackdropBrightness,
}

impl Palette {
    /// Luminosity controls backdrop brightness independently of tint opacity,
    /// so a lighter tint can preserve wallpaper hue without washing out on white.
    pub(crate) fn capsule_background(&self, frosted: bool) -> Hsla {
        let level = self.backdrop_brightness;
        match self.material {
            // Opaque #202020 / #F3F3F3 with no backdrop blur behind it.
            PopupMaterial::Solid => return self.solid_background,
            // WinUI Mica tint opacity (0.8 dark / 0.5 light) over the
            // luminosity-matched wallpaper; solid where Mica is unsupported.
            PopupMaterial::Mica => {
                if !frosted {
                    return self.solid_background;
                }
                let opacity = match (self.dark, level) {
                    (false, _) => 0.5,
                    (true, BackdropBrightness::Normal) => 0.8,
                    (true, BackdropBrightness::Bright) => 0.5,
                    (true, BackdropBrightness::Brighter) => 0.3,
                };
                return self.solid_background.opacity(opacity);
            }
            PopupMaterial::Acrylic => {}
        }
        let opacity = if frosted {
            match (self.dark, level) {
                (false, _) => 0.25,
                (true, BackdropBrightness::Normal) => 0.35,
                (true, BackdropBrightness::Bright) => 0.25,
                (true, BackdropBrightness::Brighter) => 0.15,
            }
        } else if self.dark {
            0.94
        } else {
            0.96
        };
        self.solid_background.opacity(opacity)
    }

    /// Lightness source of the Mica/Acrylic backdrop: the Fluent base
    /// (#202020 dark, near-white light). On a dark wallpaper the result lands
    /// close to this level, so the brighter dark steps target measured
    /// flyouts once the tint is composited: about 42 (Windows Quick
    /// Settings) and 50 (Twinkle Tray).
    pub(crate) fn backdrop_luminosity(&self) -> (u8, u8, u8) {
        let level = match (self.dark, self.material, self.backdrop_brightness) {
            (false, _, _) => 245,
            (true, _, BackdropBrightness::Normal) => 32,
            (true, PopupMaterial::Mica, BackdropBrightness::Bright) => 52,
            (true, PopupMaterial::Mica, BackdropBrightness::Brighter) => 60,
            (true, _, BackdropBrightness::Bright) => 45,
            (true, _, BackdropBrightness::Brighter) => 55,
        };
        (level, level, level)
    }

    pub(crate) fn new(
        dark: bool,
        accent: crate::theme::AccentRamp,
        material: PopupMaterial,
        font_family: SharedString,
        backdrop_brightness: BackdropBrightness,
    ) -> Self {
        let accent_fill = rgb8(accent.fill(dark));
        Self {
            dark,
            card_radius: crate::popup::CARD_CORNER_RADIUS_DIP as f32,
            // ControlCornerRadius.
            control_radius: 4.0,
            card_gap: 6.0,
            font_family,
            // TextFillColorPrimary/Secondary/Tertiary.
            text_primary: if dark {
                rgba8(255, 255, 255, 0xFF)
            } else {
                rgba8(0, 0, 0, 0xE4)
            },
            text_secondary: if dark {
                rgba8(255, 255, 255, 0xC5)
            } else {
                rgba8(0, 0, 0, 0x9E)
            },
            text_tertiary: if dark {
                rgba8(255, 255, 255, 0x87)
            } else {
                rgba8(0, 0, 0, 0x72)
            },
            // TextOnAccentFillColorPrimary.
            text_on_accent: if dark {
                rgba8(0, 0, 0, 0xFF)
            } else {
                rgba8(255, 255, 255, 0xFF)
            },
            accent: accent_fill,
            accent_text: rgb8(accent.text(dark)),
            // CardBackgroundFillColorDefault / CardStrokeColorDefault.
            card_background: if dark {
                rgba8(255, 255, 255, 0x0D)
            } else {
                rgba8(255, 255, 255, 0xB3)
            },
            card_stroke: if dark {
                rgba8(0, 0, 0, 0x19)
            } else {
                rgba8(0, 0, 0, 0x0F)
            },
            // SubtleFillColorSecondary.
            subtle_fill: ink(dark, if dark { 0x0F } else { 0x09 }),
            // ControlFillColorDefault.
            control_fill: if dark {
                rgba8(255, 255, 255, 0x0F)
            } else {
                rgba8(255, 255, 255, 0xB3)
            },
            // ControlStrongFillColorDefault.
            control_strong_fill: if dark {
                rgba8(255, 255, 255, 0x8B)
            } else {
                rgba8(0, 0, 0, 0x72)
            },
            // ControlSolidFillColorDefault.
            control_solid: if dark {
                rgba8(0x45, 0x45, 0x45, 0xFF)
            } else {
                rgba8(0xFF, 0xFF, 0xFF, 0xFF)
            },
            control_elevation_border: if dark {
                rgba8(255, 255, 255, 0x17)
            } else {
                rgba8(0, 0, 0, 0x1A)
            },
            // DividerStrokeColorDefault.
            divider: ink(dark, if dark { 0x15 } else { 0x0F }),
            // SolidBackgroundFillColorBase.
            solid_background: if dark {
                rgba8(0x20, 0x20, 0x20, 0xFF)
            } else {
                rgba8(0xF3, 0xF3, 0xF3, 0xFF)
            },
            // Subtle footer tint over the capsule background.
            footer_background: rgba8(0, 0, 0, if dark { 0x24 } else { 0x0D }),
            // SystemFillColorCaution.
            caution: if dark {
                rgba8(0xFC, 0xE1, 0x00, 0xFF)
            } else {
                rgba8(0x9D, 0x5D, 0x00, 0xFF)
            },
            chrome_icon: if dark {
                rgb8((190, 190, 190))
            } else {
                rgb8((17, 17, 17))
            },
            chrome_icon_hover: if dark {
                rgb8((230, 230, 230))
            } else {
                rgb8((17, 17, 17))
            },
            material,
            backdrop_brightness,
        }
    }
}

/// Resolve Auto against the window's system appearance.
pub(crate) fn resolve_dark(theme: Theme, system_dark: bool) -> bool {
    match theme {
        Theme::Auto => system_dark,
        Theme::Light => false,
        Theme::Dark => true,
    }
}

pub(crate) fn system_dark(window: &gpui::Window) -> bool {
    matches!(
        window.appearance(),
        gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark
    )
}

pub(crate) trait HslaExt {
    #[allow(dead_code)]
    fn alpha(self, alpha: f32) -> Hsla;
    fn mix(self, other: Hsla, amount: f32) -> Hsla;
}

impl HslaExt for Hsla {
    fn alpha(mut self, alpha: f32) -> Hsla {
        self.a *= alpha.clamp(0.0, 1.0);
        self
    }

    /// Linear blend in RGB space; used for hover color crossfades.
    fn mix(self, other: Hsla, amount: f32) -> Hsla {
        let t = amount.clamp(0.0, 1.0);
        if t <= 0.0 {
            return self;
        }
        if t >= 1.0 {
            return other;
        }
        let a = self.to_rgb();
        let b = other.to_rgb();
        Rgba {
            r: a.r + (b.r - a.r) * t,
            g: a.g + (b.g - a.g) * t,
            b: a.b + (b.b - a.b) * t,
            a: a.a + (b.a - a.a) * t,
        }
        .into()
    }
}
