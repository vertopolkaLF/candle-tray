//! A short, non-activating level indicator styled after the Windows volume
//! flyout. Hardware acknowledgements drive it.
//!
//! One window lives for the whole session: it is created hidden on first use,
//! then only moved, shown and hidden natively. Like the popup host, every
//! native call that moves, sizes or shows it runs outside GPUI borrows.
#![cfg(windows)]

use super::{theme, win32};
use crate::app::AppModel;
use gpui::{
    App, AppContext, AsyncApp, Bounds, Context, Entity, Hsla, Render, SharedString, Window,
    WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind, WindowOptions, div, point,
    prelude::*, px, size, svg,
};
use std::time::Duration;
use windows_sys::Win32::Foundation::HWND;

const WIDTH: f32 = 236.0;
/// Height of one display's row; with several displays each row also carries
/// the display name.
const ROW_HEIGHT: f32 = 48.0;
const MULTI_ROW_HEIGHT: f32 = 46.0;
const MULTI_PADDING: f32 = 6.0;
const MAX_ROWS: usize = 6;
/// Distance above the bottom of the work area, as the system flyouts keep.
const MARGIN: f64 = 24.0;
const HIDE_AFTER: Duration = Duration::from_millis(1800);

pub(super) struct Osd {
    model: Entity<AppModel>,
    hwnd: HWND,
    font: SharedString,
    accent: Hsla,
    /// Display name and level, in the order the displays first reported.
    entries: Vec<(SharedString, u8)>,
    visible: bool,
    generation: u64,
}

/// Create the hidden indicator window. Must run outside any GPUI borrow.
pub(super) fn create(model: Entity<AppModel>, cx: &mut AsyncApp) -> Option<WindowHandle<Osd>> {
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            point(px(0.0), px(0.0)),
            size(px(WIDTH), px(ROW_HEIGHT)),
        ))),
        titlebar: None,
        focus: false,
        show: false,
        kind: WindowKind::PopUp,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        window_background: WindowBackgroundAppearance::Transparent,
        ..Default::default()
    };
    let opened = cx.update(|cx: &mut App| {
        let font = theme::default_font_family(cx);
        cx.open_window(options, |_, cx| {
            cx.new(|_| Osd {
                model,
                hwnd: std::ptr::null_mut(),
                font,
                accent: gpui::black(),
                entries: Vec::new(),
                visible: false,
                generation: 0,
            })
        })
    });
    let handle = match opened {
        Ok(Ok(handle)) => handle,
        Ok(Err(error)) => {
            crate::logger::error(format!("could not create the OSD window: {error:#}"));
            return None;
        }
        Err(_) => return None,
    };
    let hwnd = handle
        .update(cx, |_, window, _| win32::hwnd_from_window(window))
        .ok()
        .flatten()?;
    win32::configure_osd(hwnd);
    // A hidden GPUI window starts at CW_USEDEFAULT size; release that
    // surface until the first show supplies the real geometry.
    win32::park_hidden(hwnd);
    let _ = handle.update(cx, |osd, _, _| osd.hwnd = hwnd);
    Some(handle)
}

/// Show `value` for display `name`, restarting the hide timer. Must run
/// outside any GPUI borrow.
pub(super) fn show(handle: WindowHandle<Osd>, name: String, value: u8, cx: &mut AsyncApp) {
    let shown = handle.update(cx, |osd, _, cx| {
        if !osd.visible {
            osd.entries.clear();
        }
        let name = SharedString::from(name);
        if let Some(entry) = osd.entries.iter_mut().find(|(known, _)| *known == name) {
            entry.1 = value;
        } else if osd.entries.len() < MAX_ROWS {
            osd.entries.push((name, value));
        }
        let accent = crate::theme::accent_ramp();
        let dark = osd.dark(cx);
        osd.accent = theme::rgb8(accent.fill(dark));
        let first = !osd.visible;
        osd.visible = true;
        osd.generation = osd.generation.wrapping_add(1);
        osd.expire(cx);
        cx.notify();
        (osd.hwnd, osd.entries.len(), first)
    });
    let Ok((hwnd, rows, first)) = shown else {
        return;
    };
    if hwnd.is_null() {
        return;
    }
    let rect = target_rect(rows);
    // Unchanged geometry while a wheel turns needs no native calls at all.
    if first || win32::window_rect(hwnd) != rect {
        win32::place(hwnd, rect);
        // A DPI change during placement makes GPUI apply Windows' suggested
        // rect; place again with the final geometry.
        if win32::window_rect(hwnd) != rect {
            win32::place(hwnd, rect);
        }
    }
    if first {
        let _ = handle.update(cx, |_, window, _| window.refresh());
        win32::show(hwnd);
    }
}

fn content_height(rows: usize) -> f32 {
    if rows <= 1 {
        ROW_HEIGHT
    } else {
        rows as f32 * MULTI_ROW_HEIGHT + 2.0 * MULTI_PADDING
    }
}

/// Bottom-centre of the work area under the cursor, in physical pixels.
fn target_rect(rows: usize) -> win32::Rect {
    use windows_sys::Win32::{Foundation::POINT, UI::WindowsAndMessaging::GetCursorPos};
    let mut cursor = POINT { x: 0, y: 0 };
    unsafe {
        GetCursorPos(&mut cursor);
    }
    let monitor = win32::monitor_for_point(cursor.x, cursor.y);
    let work = monitor.work;
    let (left, top, width, height) = indicator_rect(
        (work.left, work.top, work.right, work.bottom),
        monitor.scale(),
        content_height(rows),
    );
    win32::Rect {
        left,
        top,
        right: left + width,
        bottom: top + height,
    }
}

/// Physical pixels, including monitors left of or above the primary display.
fn indicator_rect(
    work: (i32, i32, i32, i32),
    scale: f64,
    content_height: f32,
) -> (i32, i32, i32, i32) {
    let (left, top, right, bottom) = work;
    let available_width = (right - left).max(1);
    let available_height = (bottom - top).max(1);
    let width = ((f64::from(WIDTH) * scale).round() as i32).clamp(1, available_width);
    let height = ((f64::from(content_height) * scale).round() as i32).clamp(1, available_height);
    let margin = ((MARGIN * scale).round() as i32).clamp(0, available_height - height);
    (
        left + (available_width - width) / 2,
        bottom - height - margin,
        width,
        height,
    )
}

impl Osd {
    fn dark(&self, cx: &App) -> bool {
        match self.model.read(cx).settings.theme {
            crate::settings::Theme::Light => false,
            crate::settings::Theme::Dark => true,
            _ => !crate::theme::taskbar_uses_light_theme(),
        }
    }

    fn expire(&mut self, cx: &mut Context<Self>) {
        let generation = self.generation;
        let hwnd = self.hwnd;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HIDE_AFTER).await;
            let hide = this
                .update(cx, |osd, _| {
                    let current = osd.generation == generation;
                    if current {
                        osd.visible = false;
                    }
                    current
                })
                .unwrap_or(false);
            // Outside the update: hiding re-enters GPUI's window procedure.
            if hide && !hwnd.is_null() {
                win32::hide(hwnd);
            }
        })
        .detach();
    }
}

impl Render for Osd {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dark = self.dark(cx);
        // The system flyout surfaces and slider rail (ControlStrongFill).
        let (background, stroke, text, secondary, rail) = if dark {
            (
                theme::rgba8(0x2C, 0x2C, 0x2C, 0xFF),
                theme::rgba8(255, 255, 255, 0x14),
                theme::rgba8(255, 255, 255, 0xFF),
                theme::rgba8(255, 255, 255, 0xC5),
                theme::rgba8(255, 255, 255, 0x8B),
            )
        } else {
            (
                theme::rgba8(0xF9, 0xF9, 0xF9, 0xFF),
                theme::rgba8(0, 0, 0, 0x0F),
                theme::rgba8(0, 0, 0, 0xE4),
                theme::rgba8(0, 0, 0, 0x9E),
                theme::rgba8(0, 0, 0, 0x72),
            )
        };
        let accent = self.accent;
        let named = self.entries.len() > 1;
        let rows = self.entries.iter().map(|(name, value)| {
            let level = div()
                .flex()
                .items_center()
                .gap(px(12.0))
                .child(
                    svg()
                        .path(super::assets::icon_path("fluent-brightness"))
                        .size(px(16.0))
                        .flex_none()
                        .text_color(text),
                )
                .child(
                    div().flex_1().h(px(4.0)).rounded(px(2.0)).bg(rail).child(
                        div()
                            .h_full()
                            .w(gpui::relative(f32::from(*value) / 100.0))
                            .rounded(px(2.0))
                            .bg(accent),
                    ),
                )
                .child(
                    div()
                        .w(px(26.0))
                        .flex_none()
                        .flex()
                        .justify_end()
                        .text_size(px(14.0))
                        .child(value.to_string()),
                );
            // A lone row fills the capsule inside its 1px stroke.
            let row = div()
                .when(named, |row| row.h(px(MULTI_ROW_HEIGHT)))
                .when(!named, |row| row.flex_1())
                .flex()
                .flex_col()
                .justify_center();
            if named {
                row.gap(px(2.0))
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(secondary)
                            .truncate()
                            .child(name.clone()),
                    )
                    .child(level)
            } else {
                row.child(level)
            }
        });
        div()
            .size_full()
            .rounded(px(8.0))
            .border_1()
            .border_color(stroke)
            .bg(background)
            .text_color(text)
            .font_family(self.font.clone())
            .px(px(16.0))
            .when(named, |this| this.py(px(MULTI_PADDING - 1.0)))
            .flex()
            .flex_col()
            .children(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osd_geometry_is_compact_at_each_dpi_and_tracks_the_target_monitor() {
        assert_eq!(
            indicator_rect((0, 0, 1920, 1040), 1.0, ROW_HEIGHT),
            (842, 968, 236, 48)
        );
        assert_eq!(
            indicator_rect((-2560, 0, 0, 1400), 1.5, ROW_HEIGHT),
            (-1457, 1292, 354, 72)
        );
        assert_eq!(
            indicator_rect((1920, -2160, 5760, 0), 2.0, ROW_HEIGHT),
            (3604, -144, 472, 96)
        );
        let (x, y, w, h) = indicator_rect((-80, -40, 40, 20), 2.0, ROW_HEIGHT);
        assert_eq!((x, y, w, h), (-80, -40, 120, 60));
    }

    #[test]
    fn several_displays_stack_rows() {
        assert_eq!(content_height(1), ROW_HEIGHT);
        assert_eq!(content_height(2), 104.0);
    }
}
