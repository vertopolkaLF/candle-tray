//! Log: the log file and a live tail of it.

use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, div, px,
};

use super::kit::{self, Button, Kit, Row};
use super::window::SettingsWindow;
use crate::i18n::tr;

/// Lines shown in the live tail.
pub(super) const TAIL_LINES: usize = 200;

impl SettingsWindow {
    pub(super) fn log_page(&mut self, k: &mut Kit, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let actions = kit::card_of(k, |k| {
            vec![
                Row::new("log-file", tr("log-application-log"))
                    .icon(kit::row_icon(k, "file-text-fill"))
                    .description(k, tr("log-application-log-description"))
                    .trailing(
                        Button::new("log-open-file", tr("log-open-file"))
                            .on_click(kit::handler(|(), _, _| crate::logger::open()))
                            .render(k),
                    )
                    .trailing(
                        Button::icon_only("log-open-folder", "folder-open-fill")
                            .tooltip(tr("log-open-folder"))
                            .on_click(kit::handler(|(), _, _| crate::logger::open_folder()))
                            .render(k),
                    )
                    .render(k),
            ]
        });
        let theme = &k.theme;
        let text = if self.log.is_empty() {
            tr("log-empty").into()
        } else {
            self.log.clone()
        };
        let tail = div()
            .id("log-tail")
            .h(px(420.0))
            .w_full()
            .overflow_y_scroll()
            .track_scroll(&self.log_scroll)
            // The tail scrolls on its own; the page must not move with it.
            .on_scroll_wheel(cx.listener(|_, _, _, cx| cx.stop_propagation()))
            .rounded(px(kit::CARD_RADIUS))
            .bg(theme.card)
            .p(px(14.0))
            .font_family(theme.mono_font.clone())
            .text_size(px(12.0))
            .line_height(px(18.0))
            .text_color(theme.text_secondary)
            .child(text)
            .into_any_element();
        vec![actions, kit::section_heading(k, tr("log-live-tail")), tail]
    }

    /// Re-read the tail when the file changed. Follows new lines while the
    /// tail is scrolled to its end; otherwise keeps the user's place.
    pub(super) fn refresh_log(&mut self, force: bool) -> bool {
        let length = crate::logger::len();
        if !force && length == self.log_length {
            return false;
        }
        self.log_length = length;
        let log = crate::logger::tail_lines(TAIL_LINES).unwrap_or_else(|error| error.to_string());
        if self.log.as_ref() == log {
            return false;
        }
        let offset = -f32::from(self.log_scroll.offset().y);
        let max = f32::from(self.log_scroll.max_offset().height);
        if force || offset >= max - 4.0 {
            self.log_scroll.scroll_to_bottom();
        }
        self.log = log.into();
        true
    }
}
