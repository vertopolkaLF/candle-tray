//! The Shortcuts page and the shortcut recorder every page shares.

use gpui::{AnyElement, Context, IntoElement, KeyDownEvent, SharedString, Window};

use super::kit::{self, Button, Kit, Row};
use super::window::SettingsWindow;
use crate::hotkey::{Action, Hotkey, Status};
use crate::settings::TurnOffSetting;

/// Percent steps offered for the brightness shortcuts.
const STEPS: [u8; 5] = [1, 2, 5, 10, 20];

fn tr(key: &str) -> SharedString {
    crate::i18n::tr(key).into()
}

impl SettingsWindow {
    pub(super) fn shortcuts_page(
        &mut self,
        k: &mut Kit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let s = self.model.read(cx).settings.clone();
        let step_index = STEPS
            .iter()
            .position(|step| *step >= s.shortcut_step)
            .unwrap_or(STEPS.len() - 1);
        let brightness = kit::card_of(k, |k| {
            vec![
                self.hotkey_row(
                    k,
                    Action::BrightnessUp,
                    s.shortcuts.brightness_up,
                    tr("shortcuts-brightness-up"),
                    None,
                    cx,
                ),
                self.hotkey_row(
                    k,
                    Action::BrightnessDown,
                    s.shortcuts.brightness_down,
                    tr("shortcuts-brightness-down"),
                    None,
                    cx,
                ),
                kit::dropdown_row(
                    k,
                    "shortcuts-step",
                    tr("shortcuts-step"),
                    Some(&crate::i18n::tr("shortcuts-step-description")),
                    STEPS.iter().map(|step| format!("{step}%").into()).collect(),
                    step_index as i32,
                    false,
                    Self::h(cx, |this, index: usize, _, cx| {
                        let step = STEPS[index.min(STEPS.len() - 1)];
                        this.edit(cx, move |settings| settings.shortcut_step = step);
                    }),
                ),
                kit::toggle_row(
                    k,
                    "show-osd",
                    tr("features-osd"),
                    Some(tr("features-osd-help")),
                    s.show_osd,
                    Self::h(cx, |this, value: bool, _, cx| {
                        this.edit(cx, move |s| s.show_osd = value)
                    }),
                ),
            ]
        });

        let turn_off_index = match s.turn_off {
            TurnOffSetting::Windows => 0,
            TurnOffSetting::Ddc => 1,
            TurnOffSetting::Both => 2,
        };
        let turn_off = kit::card_of(k, |k| {
            let mut rows = vec![
                self.hotkey_row(
                    k,
                    Action::TurnOff,
                    s.shortcuts.turn_off,
                    tr("shortcuts-turn-off"),
                    Some(tr("shortcuts-turn-off-description")),
                    cx,
                ),
                Row::new("shortcuts-turn-off-mode-row", tr("settings-turn-off-mode"))
                    .trailing(kit::segmented(
                        k,
                        "shortcuts-turn-off-mode",
                        &[
                            crate::i18n::tr("settings-turn-off-windows").as_str(),
                            crate::i18n::tr("settings-turn-off-ddc").as_str(),
                            crate::i18n::tr("settings-turn-off-both").as_str(),
                        ],
                        turn_off_index,
                        false,
                        Self::h(cx, |this, index: usize, _, cx| {
                            let mode = match index {
                                1 => TurnOffSetting::Ddc,
                                2 => TurnOffSetting::Both,
                                _ => TurnOffSetting::Windows,
                            };
                            this.edit(cx, move |settings| settings.turn_off = mode);
                        }),
                    ))
                    .render(k),
            ];
            if s.turn_off != TurnOffSetting::Windows {
                rows.push(kit::dropdown_row(
                    k,
                    "ddc-power-mode",
                    tr("features-power"),
                    Some(&tr("features-power-help")),
                    vec![
                        tr("features-power-standby"),
                        tr("features-power-off"),
                        tr("features-power-compat"),
                    ],
                    match s.ddc_power_off {
                        5 => 1,
                        6 => 2,
                        _ => 0,
                    },
                    false,
                    Self::h(cx, |this, index: usize, _, cx| {
                        this.edit(cx, move |s| {
                            s.ddc_power_off = match index {
                                1 => 5,
                                2 => 6,
                                _ => 4,
                            }
                        })
                    }),
                ));
            }
            rows
        });

        let mouse = kit::card_of(k, |k| {
            let mut rows = vec![kit::toggle_row(
                k,
                "shortcuts-tray-scroll",
                tr("shortcuts-tray-scroll"),
                Some(tr("shortcuts-tray-scroll-description")),
                s.tray_scroll,
                Self::h(cx, |this, value: bool, _, cx| {
                    this.edit(cx, move |settings| settings.tray_scroll = value)
                }),
            )];
            if s.tray_scroll {
                rows.push(kit::toggle_row(
                    k,
                    "invert-scroll",
                    tr("features-invert-scroll"),
                    None,
                    s.invert_scroll,
                    Self::h(cx, |this, value: bool, _, cx| {
                        this.edit(cx, move |s| s.invert_scroll = value)
                    }),
                ));
            }
            rows
        });

        let mut page = vec![
            kit::section_heading(k, tr("shortcuts-section-brightness")),
            brightness,
            kit::section_heading(k, tr("shortcuts-section-power")),
            turn_off,
            kit::section_heading(k, tr("shortcuts-section-mouse")),
            mouse,
            kit::caption(k, tr("shortcuts-profiles-note")),
        ];
        page.extend(self.action_shortcuts_page(k, window, cx));
        page
    }

    /// Records a shortcut: the button waits for the next combination, Esc
    /// cancels and Backspace or Delete clears it.
    pub(super) fn hotkey_row(
        &self,
        k: &mut Kit,
        action: Action,
        current: Option<Hotkey>,
        title: SharedString,
        description: Option<SharedString>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = action_key(action);
        let recording = self.hotkey_recording == Some(action);
        let label: SharedString = if recording {
            tr("profiles-shortcut-press")
        } else if let Some(hotkey) = current {
            hotkey.label().into()
        } else {
            tr("profiles-shortcut-not-set")
        };
        let mut button = Button::new(format!("hotkey-{key}"), label).on_click(Self::h(
            cx,
            move |this, (), window, cx| {
                if this.hotkey_recording == Some(action) {
                    this.stop_hotkey_recording();
                } else {
                    this.hotkey_recording = Some(action);
                    this.hotkey_rejected = false;
                    crate::hotkey::set_suspended(true);
                    // Text fields would otherwise consume the keystrokes.
                    this.focus_page(window);
                }
                cx.notify();
            },
        ));
        if recording {
            button = button.accent();
        }
        let mut row = Row::new(format!("hotkey-row-{key}"), title).description_opt(k, description);
        let detail = if recording && self.hotkey_rejected {
            Some((
                crate::i18n::tr("profiles-shortcut-needs-modifier"),
                k.theme.caution,
            ))
        } else if recording {
            Some((
                crate::i18n::tr("profiles-shortcut-hint"),
                k.theme.text_secondary,
            ))
        } else if current.is_some() && crate::hotkey::status(action) == Status::Unavailable {
            Some((crate::i18n::tr("profiles-shortcut-in-use"), k.theme.caution))
        } else {
            None
        };
        if let Some((text, color)) = detail {
            row = row.detail(kit::text(text, 12.0, color).into_any_element());
        }
        if current.is_some() && !recording {
            row = row.trailing(
                Button::icon_only(format!("hotkey-clear-{key}"), "x-circle-fill")
                    .ghost()
                    .tooltip(tr("profiles-shortcut-clear"))
                    .on_click(Self::h(cx, move |this, (), _, cx| {
                        this.model
                            .update(cx, |model, cx| model.set_hotkey(action, None, cx));
                    }))
                    .render(k),
            );
        }
        row.trailing(button.render(k)).render(k)
    }

    pub(super) fn stop_hotkey_recording(&mut self) {
        self.hotkey_recording = None;
        self.hotkey_rejected = false;
        crate::hotkey::set_suspended(false);
    }

    /// Consumes a key press while a shortcut button is recording.
    pub(super) fn record_hotkey(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let Some(action) = self.hotkey_recording else {
            return false;
        };
        let modifiers = &event.keystroke.modifiers;
        let bare = !(modifiers.control || modifiers.alt || modifiers.shift || modifiers.platform);
        match event.keystroke.key.as_str() {
            "escape" if bare => self.stop_hotkey_recording(),
            "backspace" | "delete" if bare => {
                self.stop_hotkey_recording();
                self.model
                    .update(cx, |model, cx| model.set_hotkey(action, None, cx));
            }
            _ => match crate::hotkey::pressed_now().filter(Hotkey::is_valid) {
                Some(hotkey) => {
                    self.stop_hotkey_recording();
                    self.model
                        .update(cx, |model, cx| model.set_hotkey(action, Some(hotkey), cx));
                }
                None => self.hotkey_rejected = true,
            },
        }
        cx.notify();
        true
    }
}

/// Stable element id fragment for an action.
fn action_key(action: Action) -> String {
    match action {
        Action::Profile(index) => format!("profile-{index}"),
        Action::Sequence(index) => format!("sequence-{index}"),
        Action::BrightnessUp => "brightness-up".into(),
        Action::BrightnessDown => "brightness-down".into(),
        Action::TurnOff => "turn-off".into(),
    }
}
