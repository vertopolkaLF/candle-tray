//! Settings window (GPUI), opened from the popup footer or the tray menu.

use gpui::{App, Entity};

use crate::app::AppModel;

pub fn open(model: Entity<AppModel>, cx: &mut App) {
    let _ = (model, cx);
}
