//! Embedded SVG icons for the GPUI windows.
//!
//! GPUI paints SVGs as alpha masks tinted by `text_color`, so the Iconify
//! sources are used as-is for their geometry. Every icon is normalized to a
//! square, centered view box first: GPUI scales by width only.

use std::borrow::Cow;

use gpui::{AssetSource, SharedString};

pub(crate) struct PopupAssets;

/// Asset name of a profile glyph.
pub(crate) fn profile_glyph(icon: crate::settings::ProfileIcon) -> &'static str {
    use crate::settings::ProfileIcon;
    match icon {
        ProfileIcon::Sun => "profile-sun",
        ProfileIcon::Moon => "profile-moon",
        ProfileIcon::Lightbulb => "profile-lightbulb",
        ProfileIcon::Book => "profile-book",
        ProfileIcon::Movie => "profile-movie",
        ProfileIcon::Game => "profile-game",
        ProfileIcon::Code => "profile-code",
        ProfileIcon::Work => "profile-work",
        ProfileIcon::Coffee => "profile-coffee",
        ProfileIcon::Home => "profile-home",
        ProfileIcon::Star => "profile-star",
        ProfileIcon::Heart => "profile-heart",
    }
}

/// Asset path for an icon name.
pub(crate) fn icon_path(name: &str) -> SharedString {
    SharedString::from(format!("icons/{name}.svg"))
}

fn source(name: &str) -> Option<&'static str> {
    Some(match name {
        "fluent-settings" => include_str!("../../../assets/icons/fluent-settings-20-filled.svg"),
        "fluent-power" => include_str!("../../../assets/icons/fluent-power-20-filled.svg"),
        "fluent-hdr" => include_str!("../../../assets/icons/fluent-hdr-20-regular.svg"),
        "fluent-refresh" => include_str!("../../../assets/icons/fluent-arrow-sync-20-filled.svg"),
        "fluent-chevron-down" => {
            include_str!("../../../assets/icons/fluent-chevron-down-16-regular.svg")
        }
        "fluent-drag" => {
            include_str!("../../../assets/icons/fluent-re-order-dots-vertical-16-filled.svg")
        }
        "fluent-warning" => include_str!("../../../assets/icons/fluent-warning-16-filled.svg"),
        "fluent-checkmark" => include_str!("../../../assets/icons/fluent-checkmark-20-filled.svg"),
        "fluent-info" => include_str!("../../../assets/icons/fluent-info-16-filled.svg"),
        "fluent-link" => include_str!("../../../assets/icons/fluent-link-20-filled.svg"),
        "fluent-link-dismiss" => {
            include_str!("../../../assets/icons/fluent-link-dismiss-20-filled.svg")
        }
        "fluent-laptop" => include_str!("../../../assets/icons/fluent-laptop-20-regular.svg"),
        "fluent-desktop" => include_str!("../../../assets/icons/fluent-desktop-20-regular.svg"),
        "fluent-sun" => include_str!("../../../assets/icons/fluent-weather-sunny-20-regular.svg"),
        "fluent-contrast" => {
            include_str!("../../../assets/icons/fluent-dark-theme-20-regular.svg")
        }
        "fluent-brightness" => {
            include_str!("../../../assets/icons/fluent-brightness-high-20-regular.svg")
        }
        "fluent-add" => include_str!("../../../assets/icons/fluent-add-20-filled.svg"),
        "fluent-delete" => include_str!("../../../assets/icons/fluent-delete-20-regular.svg"),
        "fluent-save" => include_str!("../../../assets/icons/fluent-save-20-regular.svg"),
        "profile-sun" => include_str!("../../../assets/icons/fluent-weather-sunny-20-regular.svg"),
        "profile-moon" => include_str!("../../../assets/icons/fluent-weather-moon-20-regular.svg"),
        "profile-lightbulb" => {
            include_str!("../../../assets/icons/fluent-lightbulb-20-regular.svg")
        }
        "profile-book" => include_str!("../../../assets/icons/fluent-book-open-20-regular.svg"),
        "profile-movie" => {
            include_str!("../../../assets/icons/fluent-movies-and-tv-20-regular.svg")
        }
        "profile-game" => include_str!("../../../assets/icons/fluent-games-20-regular.svg"),
        "profile-code" => include_str!("../../../assets/icons/fluent-code-20-regular.svg"),
        "profile-work" => include_str!("../../../assets/icons/fluent-briefcase-20-regular.svg"),
        "profile-coffee" => {
            include_str!("../../../assets/icons/fluent-drink-coffee-20-regular.svg")
        }
        "profile-home" => include_str!("../../../assets/icons/fluent-home-20-regular.svg"),
        "profile-star" => include_str!("../../../assets/icons/fluent-star-20-regular.svg"),
        "profile-heart" => include_str!("../../../assets/icons/fluent-heart-20-regular.svg"),
        // Phosphor glyphs used by the Settings window, keyed by file stem.
        "check-bold" => include_str!("../../../assets/icons/ph-check-bold.svg"),
        "check-circle-fill" => include_str!("../../../assets/icons/ph-check-circle-fill.svg"),
        "dots-three-bold" => include_str!("../../../assets/icons/ph-dots-three-bold.svg"),
        "info-fill" => include_str!("../../../assets/icons/ph-info-fill.svg"),
        "github-logo-fill" => include_str!("../../../assets/icons/ph-github-logo-fill.svg"),
        "download-simple-fill" => {
            include_str!("../../../assets/icons/ph-download-simple-fill.svg")
        }
        "flag-fill" => include_str!("../../../assets/icons/ph-flag-fill.svg"),
        "at-fill" => include_str!("../../../assets/icons/ph-at-fill.svg"),
        "arrow-square-out" => include_str!("../../../assets/icons/ph-arrow-square-out.svg"),
        "sliders-horizontal-fill" => {
            include_str!("../../../assets/icons/ph-sliders-horizontal-fill.svg")
        }
        "plus-bold" => include_str!("../../../assets/icons/ph-plus-bold.svg"),
        "x-circle-fill" => include_str!("../../../assets/icons/ph-x-circle-fill.svg"),
        "caret-down-bold" => include_str!("../../../assets/icons/ph-caret-down-bold.svg"),
        "magnifying-glass-bold" => {
            include_str!("../../../assets/icons/ph-magnifying-glass-bold.svg")
        }
        "warning-fill" => include_str!("../../../assets/icons/ph-warning-fill.svg"),
        "desktop-fill" => include_str!("../../../assets/icons/ph-desktop-fill.svg"),
        "gear-six-fill" => include_str!("../../../assets/icons/ph-gear-six-fill.svg"),
        "keyboard-fill" => include_str!("../../../assets/icons/ph-keyboard-fill.svg"),
        "clock-fill" => include_str!("../../../assets/icons/ph-clock-fill.svg"),
        "monitor-fill" => include_str!("../../../assets/icons/ph-monitor-fill.svg"),
        "laptop-fill" => include_str!("../../../assets/icons/ph-laptop-fill.svg"),
        "arrow-counter-clockwise-bold" => {
            include_str!("../../../assets/icons/ph-arrow-counter-clockwise-bold.svg")
        }
        "dots-six-vertical-bold" => {
            include_str!("../../../assets/icons/ph-dots-six-vertical-bold.svg")
        }
        _ => {
            return crate::settings::TrayIcon::ALL
                .into_iter()
                .find(|glyph| crate::tray::glyph_asset(*glyph) == name)
                .map(crate::tray::glyph_svg);
        }
    })
}

fn attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let needle = format!(" {name}=\"");
    let start = tag.find(&needle)? + needle.len();
    let end = tag[start..].find('"')? + start;
    Some(&tag[start..end])
}

fn strip_attribute(tag: &str, name: &str) -> String {
    let needle = format!(" {name}=\"");
    let Some(start) = tag.find(&needle) else {
        return tag.to_owned();
    };
    let value_start = start + needle.len();
    let Some(end) = tag[value_start..].find('"') else {
        return tag.to_owned();
    };
    format!("{}{}", &tag[..start], &tag[value_start + end + 1..])
}

/// Rewrite the root `<svg>` tag to a square view box centered on the original
/// artwork, with explicit pixel dimensions so usvg never resolves `em` units.
pub(crate) fn normalize_svg(svg: &str) -> String {
    let Some(open) = svg.find("<svg") else {
        return svg.to_owned();
    };
    let Some(close) = svg[open..].find('>').map(|index| index + open) else {
        return svg.to_owned();
    };
    let tag = &svg[open..close];
    let numbers = attribute(tag, "viewBox")
        .map(|value| {
            value
                .split(|c: char| c.is_whitespace() || c == ',')
                .filter(|part| !part.is_empty())
                .filter_map(|part| part.parse::<f64>().ok())
                .collect::<Vec<_>>()
        })
        .filter(|numbers| numbers.len() == 4)
        .unwrap_or_else(|| vec![0.0, 0.0, 24.0, 24.0]);
    let (min_x, min_y, width, height) = (numbers[0], numbers[1], numbers[2], numbers[3]);
    let side = width.max(height).max(f64::EPSILON);
    let x = min_x - (side - width) / 2.0;
    let y = min_y - (side - height) / 2.0;
    let mut rewritten = tag.to_owned();
    for name in ["viewBox", "width", "height"] {
        rewritten = strip_attribute(&rewritten, name);
    }
    format!(
        "{}{} width=\"{side}\" height=\"{side}\" viewBox=\"{x} {y} {side} {side}\"{}",
        &svg[..open],
        rewritten,
        &svg[close..]
    )
}

impl AssetSource for PopupAssets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        if path == "color/app-icon.png" {
            return Ok(Some(Cow::Borrowed(include_bytes!(
                "../../../assets/app-icon.png"
            ))));
        }
        let Some(name) = path
            .strip_prefix("icons/")
            .and_then(|rest| rest.strip_suffix(".svg"))
        else {
            return Ok(None);
        };
        Ok(source(name).map(|svg| Cow::Owned(normalize_svg(svg).into_bytes())))
    }

    fn list(&self, _path: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tall_marks_are_centered_in_a_square_view_box() {
        let normalized = normalize_svg(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="0.83em" height="1em" viewBox="0 0 256 312"><path d="M0 0"/></svg>"#,
        );
        assert!(normalized.contains(r#"viewBox="-28 0 312 312""#));
        assert!(normalized.contains(r#"width="312""#));
    }
}
