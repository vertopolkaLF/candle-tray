//! The Profiles page: name, icon, shortcut and stored levels of each profile.

use gpui::{
    AnyElement, Context, IntoElement, ParentElement, SharedString, Styled, Window, div, px,
};

use super::kit::{self, Button, Kit, Row, SliderRange};
use super::window::SettingsWindow;
use crate::hotkey::Action;
use crate::popup::ui::assets::profile_glyph;
use crate::settings::{ProfileIcon, ProfileLevels};

fn tr(key: &str) -> SharedString {
    crate::i18n::tr(key).into()
}

fn icon_label(icon: ProfileIcon) -> SharedString {
    tr(match icon {
        ProfileIcon::Sun => "profile-icon-sun",
        ProfileIcon::Moon => "profile-icon-moon",
        ProfileIcon::Lightbulb => "profile-icon-lightbulb",
        ProfileIcon::Book => "profile-icon-book",
        ProfileIcon::Movie => "profile-icon-movie",
        ProfileIcon::Game => "profile-icon-game",
        ProfileIcon::Code => "profile-icon-code",
        ProfileIcon::Work => "profile-icon-work",
        ProfileIcon::Coffee => "profile-icon-coffee",
        ProfileIcon::Home => "profile-icon-home",
        ProfileIcon::Star => "profile-icon-star",
        ProfileIcon::Heart => "profile-icon-heart",
    })
}

/// Which stored level a slider edits.
#[derive(Clone, Copy)]
enum Level {
    Brightness,
    Contrast,
    Sdr,
}

impl SettingsWindow {
    pub(super) fn profiles_page(
        &mut self,
        k: &mut Kit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let model = self.model.read(cx);
        let percent = SliderRange {
            min: 0.0,
            max: 100.0,
            step: f32::from(model.settings.slider_step()),
        };
        let profiles = model.settings.profiles.clone();
        let can_save = model.can_save_profile();
        let count = profiles.len();
        let mut page = Vec::new();
        if profiles.is_empty() {
            page.push(kit::card_of(k, |k| {
                vec![
                    Row::new("profiles-empty", tr("profiles-empty"))
                        .description(k, tr("profiles-empty-description"))
                        .render(k),
                ]
            }));
        }
        for (index, profile) in profiles.into_iter().enumerate() {
            let (name, entries) = {
                let model = self.model.read(cx);
                let connected: Vec<String> = model.monitors.iter().map(|m| m.id.clone()).collect();
                let entries: Vec<(String, String, bool, ProfileLevels)> = profile
                    .levels
                    .iter()
                    .map(|(id, levels)| {
                        (
                            id.clone(),
                            model.display_name(id),
                            connected.contains(id),
                            *levels,
                        )
                    })
                    .collect();
                (model.profile_name(index), entries)
            };
            page.push(kit::section_heading(k, name.clone()));

            let input = self.input(
                format!("profile-name-{index}"),
                &profile.name,
                name,
                window,
                cx,
                move |this, text, _, _, cx| {
                    this.model
                        .update(cx, |model, cx| model.rename_profile(index, text, cx));
                },
            );
            let name_field = kit::text_field(k, &input, Some(240.0), window, cx);

            let selected = ProfileIcon::ALL
                .iter()
                .position(|icon| *icon == profile.icon)
                .unwrap_or(0);
            let icons = kit::icon_choices(
                k,
                &format!("profile-icon-{index}"),
                ProfileIcon::ALL
                    .iter()
                    .map(|icon| (profile_glyph(*icon), icon_label(*icon)))
                    .collect(),
                selected,
                Self::h(cx, move |this, choice: usize, _, cx| {
                    let icon = ProfileIcon::ALL.get(choice).copied().unwrap_or_default();
                    this.model
                        .update(cx, |model, cx| model.set_profile_icon(index, icon, cx));
                }),
            );

            let shortcut = self.hotkey_row(
                k,
                Action::Profile(index),
                profile.hotkey,
                tr("profiles-shortcut"),
                Some(tr("profiles-shortcut-description")),
                cx,
            );

            let mut level_rows = Vec::new();
            for (id, display, connected, levels) in entries {
                let mut add = |level: Level, value: Option<u8>, label: &str| {
                    let Some(value) = value else {
                        return;
                    };
                    let level_id = id.clone();
                    let slider = kit::slider(
                        k,
                        format!("profile-{index}-{id}-{label}"),
                        f32::from(value),
                        percent,
                        200.0,
                        Self::h(cx, move |this, value: f32, _, cx| {
                            let value = value.round().clamp(0.0, 100.0) as u8;
                            this.model.update(cx, |model, cx| {
                                model.set_profile_level(
                                    index,
                                    &level_id,
                                    |levels| match level {
                                        Level::Brightness => levels.brightness = Some(value),
                                        Level::Contrast => levels.contrast = Some(value),
                                        Level::Sdr => levels.sdr = Some(value),
                                    },
                                    cx,
                                )
                            });
                        }),
                    );
                    let trailing = div()
                        .flex()
                        .items_center()
                        .gap(px(12.0))
                        .child(slider)
                        .child(div().w(px(36.0)).flex().justify_end().child(kit::text(
                            format!("{value}"),
                            14.0,
                            k.theme.text_secondary,
                        )))
                        .into_any_element();
                    let mut row = Row::new(
                        format!("profile-{index}-{id}-{label}-row"),
                        format!("{display} · {}", crate::i18n::tr(label)),
                    );
                    if !connected {
                        row = row.description(k, tr("profiles-disconnected"));
                    }
                    level_rows.push(row.trailing(trailing).render(k));
                };
                add(Level::Brightness, levels.brightness, "profiles-brightness");
                add(Level::Contrast, levels.contrast, "profiles-contrast");
                add(Level::Sdr, levels.sdr, "popup-sdr");
            }

            let apply = Button::new(format!("profile-apply-{index}"), tr("profiles-apply"))
                .accent()
                .on_click(Self::h(cx, move |this, (), _, cx| {
                    this.model
                        .update(cx, |model, cx| model.apply_profile(index, cx));
                }))
                .render(k);
            let recapture = Button::new(
                format!("profile-recapture-{index}"),
                tr("profiles-recapture"),
            )
            .disabled(!can_save)
            .on_click(Self::h(cx, move |this, (), _, cx| {
                this.model
                    .update(cx, |model, cx| model.recapture_profile(index, cx));
            }))
            .render(k);
            let down = Button::new(format!("profile-down-{index}"), tr("monitors-move-down"))
                .disabled(index + 1 == count)
                .on_click(Self::h(cx, move |this, (), _, cx| {
                    this.model
                        .update(cx, |model, cx| model.move_profile(index, index + 1, cx));
                }))
                .render(k);
            let up = Button::new(format!("profile-up-{index}"), tr("monitors-move-up"))
                .disabled(index == 0)
                .on_click(Self::h(cx, move |this, (), _, cx| {
                    this.model
                        .update(cx, |model, cx| model.move_profile(index, index - 1, cx));
                }))
                .render(k);
            let delete = Button::new(format!("profile-delete-{index}"), tr("profiles-delete"))
                .danger()
                .on_click(Self::h(cx, move |this, (), _, cx| {
                    this.stop_hotkey_recording();
                    this.model
                        .update(cx, |model, cx| model.delete_profile(index, cx));
                }))
                .render(k);
            let actions = div()
                .flex()
                .flex_wrap()
                .gap(px(8.0))
                .child(apply)
                .child(recapture)
                .child(up)
                .child(down)
                .child(delete)
                .into_any_element();

            page.push(kit::card_of(k, |k| {
                let mut rows = vec![
                    Row::new(format!("profile-name-row-{index}"), tr("profiles-name"))
                        .trailing(name_field)
                        .render(k),
                    Row::new(format!("profile-icon-row-{index}"), tr("profiles-icon"))
                        .detail(div().pt(px(8.0)).child(icons).into_any_element())
                        .render(k),
                    shortcut,
                ];
                rows.extend(level_rows);
                rows.push(
                    Row::new(format!("profile-actions-{index}"), tr("profiles-manage"))
                        .detail(div().pt(px(8.0)).child(actions).into_any_element())
                        .render(k),
                );
                rows
            }));
        }

        let add = Button::new("profiles-add", tr("profiles-add"))
            .accent()
            .with_icon("plus-bold")
            .disabled(!can_save)
            .on_click(Self::h(cx, |this, (), _, cx| {
                this.model.update(cx, |model, cx| {
                    model.save_profile(cx);
                });
            }))
            .render(k);
        page.push(div().pt(px(12.0)).into_any_element());
        page.push(kit::card_of(k, |k| {
            vec![
                Row::new("profiles-add-row", tr("profiles-add"))
                    .description(k, tr("profiles-add-description"))
                    .trailing(add)
                    .render(k),
            ]
        }));
        page
    }
}
