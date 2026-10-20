//! Small text helpers shared by the popup and the Settings window.

use gpui::{Div, FontWeight, Hsla, ParentElement, SharedString, Styled, div, px};

pub(crate) fn text(value: impl Into<SharedString>, size: f32, line: f32, color: Hsla) -> Div {
    div()
        .text_size(px(size))
        .line_height(px(line))
        .text_color(color)
        .child(value.into())
}

pub(crate) fn caption(value: impl Into<SharedString>, color: Hsla) -> Div {
    text(value, 12.0, 16.0, color)
}

pub(crate) fn nowrap(element: Div) -> Div {
    element
        .whitespace_nowrap()
        .overflow_hidden()
        .text_ellipsis()
}

/// Width of `value` set in `family` at `size`, the widest line for
/// multi-line text.
pub(crate) fn measure_text(
    text_system: &gpui::WindowTextSystem,
    family: SharedString,
    size: f32,
    weight: FontWeight,
    value: &str,
) -> f32 {
    let mut font = gpui::font(family);
    font.weight = weight;
    value
        .lines()
        .map(|line| {
            let run = gpui::TextRun {
                len: line.len(),
                font: font.clone(),
                color: gpui::black(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            f32::from(
                text_system
                    .shape_line(line.to_owned().into(), px(size), &[run], None)
                    .width,
            )
        })
        .fold(0.0, f32::max)
}
