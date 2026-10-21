//! The Settings window shell: title bar, sidebar navigation and the
//! scrolling page area.

use std::time::Duration;

use gpui::{
    AnyElement, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement,
    KeyDownEvent, ParentElement, Render, ScrollHandle, ScrollWheelEvent, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Window, WindowControlArea, div, point, px,
};

use super::input::{HasInputs, InputEvent, Inputs, TextInput};
use super::kit::{self, Handler, Kit, eid, icon};
use super::theme::{Fonts, Theme};
use crate::app::AppModel;
use crate::popup::ui::fx;

const TITLEBAR_HEIGHT: f32 = 48.0;
const NAV_ITEM_HEIGHT: f32 = 36.0;
const NAV_ITEM_GAP: f32 = 4.0;
const SIDEBAR_WIDTH: f32 = 240.0;
const CONTENT_MAX_WIDTH: f32 = 760.0;
const SCROLL_GLIDE: Duration = Duration::from_millis(140);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tab {
    General,
    Monitors,
    Profiles,
    Shortcuts,
    Automation,
}

impl Tab {
    const ALL: [Tab; 5] = [
        Tab::General,
        Tab::Monitors,
        Tab::Profiles,
        Tab::Shortcuts,
        Tab::Automation,
    ];

    fn label(self) -> String {
        match self {
            Tab::General => crate::i18n::tr("settings-general"),
            Tab::Monitors => crate::i18n::tr("settings-monitors"),
            Tab::Profiles => crate::i18n::tr("settings-profiles"),
            Tab::Shortcuts => crate::i18n::tr("settings-shortcuts"),
            Tab::Automation => crate::i18n::tr("settings-automation"),
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Tab::General => "gear-six-fill",
            Tab::Monitors => "monitor-fill",
            Tab::Profiles => "sliders-horizontal-fill",
            Tab::Shortcuts => "keyboard-fill",
            Tab::Automation => "clock-fill",
        }
    }

    fn tag(self) -> &'static str {
        match self {
            Tab::General => "general",
            Tab::Monitors => "monitors",
            Tab::Profiles => "profiles",
            Tab::Shortcuts => "shortcuts",
            Tab::Automation => "automation",
        }
    }
}

pub(crate) struct SettingsWindow {
    pub(super) model: Entity<AppModel>,
    pub(super) kit: Kit,
    fonts: Fonts,
    backdrop: super::backdrop::Backdrop,
    focus: FocusHandle,
    tab: Tab,
    /// Direction the page slides in from after a tab change.
    nav_slide: f32,
    scroll: ScrollHandle,
    scroll_glide: Option<f32>,
    inputs: Inputs<Self>,
    /// A short confirmation shown at the bottom of the page.
    pub(super) notice: Option<(SharedString, u64)>,
    notice_generation: u64,
    /// Action whose shortcut button is waiting for a key combination.
    pub(super) hotkey_recording: Option<crate::hotkey::Action>,
    /// The last combination pressed while recording had no modifier.
    pub(super) hotkey_rejected: bool,
    /// Location detection is running.
    pub(super) locating: bool,
    _subscriptions: Vec<Subscription>,
}

impl HasInputs for SettingsWindow {
    fn inputs(&self) -> &Inputs<Self> {
        &self.inputs
    }

    fn inputs_mut(&mut self) -> &mut Inputs<Self> {
        &mut self.inputs
    }
}

impl Focusable for SettingsWindow {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl SettingsWindow {
    pub(crate) fn new(
        model: Entity<AppModel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscriptions = vec![
            cx.observe_window_appearance(window, |_, _, cx| cx.notify()),
            // Hardware snapshots and edits from the popup repaint Settings.
            cx.observe(&model, |_, _, cx| cx.notify()),
            // The window's renderer is gone now; return its memory once idle.
            cx.on_release(|_, cx| {
                crate::hotkey::set_suspended(false);
                super::window_closed();
                crate::idle::enter_soon(cx);
            }),
        ];
        window.on_window_should_close(cx, |_, _| {
            super::window_closed();
            true
        });
        let focus = cx.focus_handle();
        window.focus(&focus);
        Self {
            model,
            kit: Kit::default(),
            fonts: Fonts::resolve(cx),
            backdrop: super::backdrop::Backdrop::install(window),
            focus,
            tab: Tab::General,
            nav_slide: 0.0,
            scroll: ScrollHandle::new(),
            scroll_glide: None,
            inputs: Inputs::default(),
            notice: None,
            notice_generation: 0,
            hotkey_recording: None,
            hotkey_rejected: false,
            locating: false,
            _subscriptions: subscriptions,
        }
    }

    /// Wrap a view callback as a kit [`Handler`].
    pub(super) fn h<T: 'static>(
        cx: &Context<Self>,
        f: impl Fn(&mut Self, T, &mut Window, &mut Context<Self>) + 'static,
    ) -> Handler<T> {
        let this = cx.entity().downgrade();
        kit::handler(move |value: T, window, cx| {
            let _ = this.update(cx, |this, cx| f(this, value, window, cx));
        })
    }

    /// Apply a settings change through the shared model (live + persisted).
    pub(super) fn edit(
        &mut self,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut crate::settings::Settings) + 'static,
    ) {
        self.model.update(cx, |model, cx| model.edit(cx, change));
    }

    /// A persistent text input for `id`; `on_commit` runs on Enter and blur.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn input(
        &mut self,
        id: impl Into<SharedString>,
        value: &str,
        placeholder: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
        on_commit: impl Fn(&mut Self, String, InputEvent, &mut Window, &mut Context<Self>) + 'static,
    ) -> Entity<TextInput> {
        Inputs::input(
            self,
            id.into(),
            value,
            placeholder.into(),
            false,
            false,
            window,
            cx,
            on_commit,
        )
    }

    pub(super) fn show_notice(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.notice_generation = self.notice_generation.wrapping_add(1);
        let generation = self.notice_generation;
        self.notice = Some((message.into(), generation));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(4)).await;
            let _ = this.update(cx, |this, cx| {
                if this.notice.as_ref().is_some_and(|(_, g)| *g == generation) {
                    this.notice = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn select_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        if tab == self.tab {
            return;
        }
        if tab == Tab::Automation {
            crate::light_sensor::refresh_availability();
        }
        self.stop_hotkey_recording();
        let from = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
        let to = Tab::ALL.iter().position(|t| *t == tab).unwrap_or(0);
        self.nav_slide = if to > from { 1.0 } else { -1.0 };
        self.tab = tab;
        self.scroll.set_offset(point(px(0.0), px(0.0)));
        self.scroll_glide = None;
        cx.notify();
    }

    /// Move keyboard focus off any text field onto the page.
    pub(super) fn focus_page(&self, window: &mut Window) {
        window.focus(&self.focus);
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.record_hotkey(event, cx) {
            cx.stop_propagation();
        }
    }

    // ----- shell ------------------------------------------------------------

    fn titlebar(&self, k: &Kit) -> AnyElement {
        let theme = &k.theme;
        div()
            .id("settings-titlebar")
            .flex()
            .items_center()
            .h(px(TITLEBAR_HEIGHT))
            .flex_none()
            .window_control_area(WindowControlArea::Drag)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .pl(px(16.0))
                    .flex_1()
                    .child(gpui::img("color/app-icon.png").size(px(16.0)).flex_none())
                    .child(kit::text(
                        crate::i18n::tr("settings-window-title"),
                        12.0,
                        theme.text_secondary,
                    )),
            )
            .child(kit::caption_button(
                k,
                "caption-min",
                "\u{E921}",
                WindowControlArea::Min,
                false,
            ))
            .child(kit::caption_button(
                k,
                "caption-close",
                "\u{E8BB}",
                WindowControlArea::Close,
                true,
            ))
            .into_any_element()
    }

    fn sidebar(&mut self, k: &mut Kit, cx: &mut Context<Self>) -> AnyElement {
        let selected = Tab::ALL.iter().position(|tab| *tab == self.tab);
        let mut list = div().relative().flex().flex_col().gap(px(NAV_ITEM_GAP));
        for tab in Tab::ALL {
            let theme = &k.theme;
            let id: SharedString = format!("nav-{}", tab.tag()).into();
            let key = kit::hover_key(&id);
            let (rest, target) = if self.tab == tab {
                (theme.nav_selected, theme.nav_selected)
            } else {
                (gpui::transparent_black(), theme.subtle_hover)
            };
            let on_click = Self::h(cx, move |this, (), _, cx| this.select_tab(tab, cx));
            list = list.child(
                kit::hover_bg(k, div().id(eid(id)), key, rest, target)
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .h(px(NAV_ITEM_HEIGHT))
                    .px(px(14.0))
                    .rounded(px(kit::CONTROL_RADIUS))
                    .on_click(move |_, window, cx| on_click((), window, cx))
                    .child(icon(tab.icon(), 18.0, k.theme.text_secondary))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(14.0))
                            .text_color(k.theme.text)
                            .child(SharedString::from(tab.label())),
                    ),
            );
        }
        if let Some(index) = selected {
            let pitch = NAV_ITEM_HEIGHT + NAV_ITEM_GAP;
            let (top, height) =
                k.fx.indicator(fx::key("nav-pill"), index as f32 * pitch, 16.0, fx::NORMAL);
            list = list.child(
                div()
                    .absolute()
                    .left(px(0.0))
                    .top(px(top + (NAV_ITEM_HEIGHT - height) / 2.0))
                    .w(px(3.0))
                    .h(px(height))
                    .rounded_full()
                    .bg(k.theme.accent),
            );
        }
        div()
            .id("sidebar-scroll")
            .size_full()
            .overflow_y_scroll()
            .px(px(8.0))
            .pt(px(4.0))
            .child(list)
            .into_any_element()
    }

    /// Turns GPUI's instant wheel jump into a glide. GPUI's own scroll
    /// listener on the same element runs first and has already moved the
    /// offset by the wheel delta; take that back and glide there instead.
    fn on_page_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let delta = f32::from(event.delta.pixel_delta(window.line_height()).y);
        if delta == 0.0 {
            return;
        }
        let offset = self.scroll.offset();
        let current = -(f32::from(offset.y) - delta);
        self.scroll.set_offset(point(offset.x, px(-current)));
        let max = f32::from(self.scroll.max_offset().height).max(0.0);
        let base = match self.scroll_glide {
            Some(target) => target,
            None => {
                let current = current.clamp(0.0, max);
                self.kit.fx.snap(fx::key("settings-page-scroll"), current);
                current
            }
        };
        self.scroll_glide = Some((base - delta).clamp(0.0, max));
        cx.notify();
    }

    fn step_scroll_glide(&mut self, k: &mut Kit) {
        let Some(target) = self.scroll_glide else {
            return;
        };
        let target = target.min(f32::from(self.scroll.max_offset().height).max(0.0));
        let y =
            k.fx.value(fx::key("settings-page-scroll"), target, SCROLL_GLIDE);
        self.scroll
            .set_offset(point(self.scroll.offset().x, px(-y)));
        if (y - target).abs() < 0.5 {
            self.scroll_glide = None;
        }
    }

    fn content(&mut self, k: &mut Kit, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.step_scroll_glide(k);
        let mut rows = vec![kit::page_title(k, self.tab.label())];
        rows.extend(match self.tab {
            Tab::General => self.general_page(k, window, cx),
            Tab::Monitors => self.monitors_page(k, window, cx),
            Tab::Profiles => self.profiles_page(k, window, cx),
            Tab::Shortcuts => self.shortcuts_page(k, window, cx),
            Tab::Automation => self.automation_page(k, window, cx),
        });
        let body = div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .w_full()
            .max_w(px(CONTENT_MAX_WIDTH))
            .children(rows);
        let body = kit::slide_in(
            k,
            format!("{}-appear", self.tab.tag()),
            body.into_any_element(),
            self.nav_slide,
            false,
        );
        let scroller = div()
            .id(eid(format!("{}-scroll", self.tab.tag())))
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .on_scroll_wheel(cx.listener(Self::on_page_wheel))
            .child(
                div()
                    .flex()
                    .justify_center()
                    .px(px(40.0))
                    .pt(px(32.0))
                    .pb(px(48.0))
                    .child(body),
            );
        let notice = self.notice.as_ref().map(|(message, generation)| {
            kit::appear(
                k,
                format!("notice-{generation}"),
                div()
                    .shadow_lg()
                    .rounded(px(kit::CARD_RADIUS))
                    .child(kit::info_bar(k, kit::Severity::Success, message.clone()))
                    .into_any_element(),
            )
        });
        div()
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .rounded_tl(px(12.0))
            .bg(k.theme.layer)
            .overflow_hidden()
            .child(scroller)
            .children(kit::scrollbar(k, &self.scroll))
            .children(notice.map(|notice| {
                div()
                    .absolute()
                    .bottom(px(20.0))
                    .left_0()
                    .right_0()
                    .flex()
                    .justify_center()
                    .child(div().w_full().max_w(px(520.0)).px(px(24.0)).child(notice))
            }))
            .into_any_element()
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_window_title(&crate::i18n::tr("settings-window-title"));
        let mut k = std::mem::take(&mut self.kit);
        let mut theme = Theme::resolve(
            self.model.read(cx).settings.theme,
            window.appearance(),
            self.fonts.clone(),
        );
        if self.backdrop.mica() {
            theme = theme.with_mica();
        }
        self.backdrop.sync(theme.dark, window.appearance());
        k.begin_frame(theme, window);
        k.page_scroll = Some(self.scroll.clone());

        let titlebar = self.titlebar(&k);
        let sidebar = self.sidebar(&mut k, cx);
        let content = self.content(&mut k, window, cx);
        k.end_frame(window);
        let theme = k.theme.clone();
        self.kit = k;

        div()
            .id("settings-root")
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .font_family(theme.font.clone())
            .text_color(theme.text)
            .bg(theme.window_bg)
            .child(titlebar)
            .child(
                // Focus is tracked below the title bar: a focusable element
                // prevents default on mouse down, and GPUI then swallows the
                // non-client click that would drag or press a caption button.
                div()
                    .id("settings-body")
                    .track_focus(&self.focus)
                    .on_key_down(cx.listener(Self::on_key_down))
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .w(px(SIDEBAR_WIDTH))
                            .flex_none()
                            .h_full()
                            .overflow_hidden()
                            .pb(px(4.0))
                            .child(sidebar),
                    )
                    .child(content),
            )
    }
}
