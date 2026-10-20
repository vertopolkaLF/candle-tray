//! Settings window (GPUI), opened from the popup footer or the tray menu.
//!
//! The component kit, theme, Mica backdrop and text input are Codex
//! Minibar's; the window and its two pages are Candle Tray's own.

mod backdrop;
#[allow(dead_code)]
pub(crate) mod input;
#[allow(dead_code)]
pub(crate) mod kit;
mod pages;
#[allow(dead_code)]
pub(crate) mod theme;
mod window;

use std::cell::RefCell;

use gpui::{
    App, AppContext, Bounds, Entity, Point, TitlebarOptions, WindowBackgroundAppearance,
    WindowBounds, WindowHandle, WindowKind, WindowOptions, px, size,
};

use crate::app::AppModel;

const WINDOW_WIDTH: f32 = 860.0;
const WINDOW_HEIGHT: f32 = 640.0;

thread_local! {
    static SETTINGS_WINDOW: RefCell<Option<WindowHandle<window::SettingsWindow>>> =
        const { RefCell::new(None) };
}

/// Register key bindings used by the Settings window. Called once when the
/// GPUI application starts.
pub fn init(cx: &mut App) {
    input::bind_keys(cx);
}

/// Open Settings, or bring the open window to the front.
pub fn open(model: Entity<AppModel>, cx: &mut App) {
    if let Some(handle) = SETTINGS_WINDOW.with(|slot| *slot.borrow())
        && handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        return;
    }
    let bounds = Bounds::centered(None, size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some(crate::i18n::tr("settings-window-title").into()),
            appears_transparent: true,
            traffic_light_position: Some(Point::default()),
        }),
        focus: true,
        show: true,
        kind: WindowKind::Normal,
        is_movable: true,
        is_resizable: true,
        is_minimizable: true,
        display_id: None,
        window_background: WindowBackgroundAppearance::Opaque,
        app_id: Some("CandleTray".into()),
        window_min_size: Some(size(px(640.0), px(480.0))),
        window_max_size: None,
        window_decorations: None,
        tabbing_identifier: None,
    };
    match cx.open_window(options, |window, cx| {
        cx.new(|cx| window::SettingsWindow::new(model, window, cx))
    }) {
        Ok(handle) => {
            SETTINGS_WINDOW.with(|slot| *slot.borrow_mut() = Some(handle));
            let _ = handle.update(cx, |_, window, _| window.activate_window());
        }
        Err(error) => crate::logger::error(format!("could not open Settings: {error:#}")),
    }
}

/// Called by the window as it closes.
pub(crate) fn window_closed() {
    SETTINGS_WINDOW.with(|slot| slot.borrow_mut().take());
}
