//! The General and Monitors pages.

use gpui::{
    AnyElement, Context, IntoElement, ParentElement, SharedString, Styled, Window, div, px,
};

use super::kit::{self, Button, Kit, Row, SliderRange};
use super::window::SettingsWindow;
use crate::monitors::{BrightnessBackend, MonitorKind};
use crate::settings::{
    BackdropBrightness, Language, MonitorSettings, PopupMaterial, SLIDER_STEPS, Theme, TrayIcon,
};

/// VCP codes displays use for brightness, offered as overrides.
const BRIGHTNESS_VCPS: [u8; 4] = [0x10, 0x13, 0x6B, 0x12];

fn vcp_label(code: u8) -> String {
    let name = match code {
        0x10 => " · Luminance",
        0x13 => " · Backlight",
        0x6B => " · Backlight (white)",
        0x12 => " · Contrast",
        _ => "",
    };
    format!("0x{code:02X}{name}")
}

/// Update intervals offered while dragging, in milliseconds.
const UPDATE_INTERVALS: [u32; 5] = [50, 100, 250, 500, 1000];

fn tr(key: &str) -> SharedString {
    crate::i18n::tr(key).into()
}

impl SettingsWindow {
    pub(super) fn general_page(
        &mut self,
        k: &mut Kit,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let s = self.model.read(cx).settings.clone();

        let language_labels = [
            crate::i18n::tr("settings-language-system"),
            "English".to_owned(),
            "Русский".to_owned(),
        ];
        let language_index = match s.language {
            Language::System => 0,
            Language::En => 1,
            Language::Ru => 2,
        };
        let theme_labels = [
            crate::i18n::tr("settings-theme-auto"),
            crate::i18n::tr("settings-theme-light"),
            crate::i18n::tr("settings-theme-dark"),
        ];
        let theme_index = match s.theme {
            Theme::Auto => 0,
            Theme::Light => 1,
            Theme::Dark => 2,
        };
        let material_labels = [
            crate::i18n::tr("settings-material-acrylic"),
            crate::i18n::tr("settings-material-mica"),
            crate::i18n::tr("settings-material-solid"),
        ];
        let material_index = match s.popup_material {
            PopupMaterial::Acrylic => 0,
            PopupMaterial::Mica => 1,
            PopupMaterial::Solid => 2,
        };
        let tray_icon_choices: Vec<(&'static str, SharedString)> = TrayIcon::ALL
            .iter()
            .map(|icon| {
                let label = tr(match icon {
                    TrayIcon::Fire => "settings-tray-icon-fire",
                    TrayIcon::Sun => "settings-tray-icon-sun",
                    TrayIcon::BrightnessHigh => "settings-tray-icon-brightness-high",
                    TrayIcon::BrightnessLow => "settings-tray-icon-brightness-low",
                    TrayIcon::Lightbulb => "settings-tray-icon-lightbulb",
                });
                (crate::tray::glyph_asset(*icon), label)
            })
            .collect();
        let tray_icon_index = TrayIcon::ALL
            .iter()
            .position(|icon| *icon == s.tray_icon)
            .unwrap_or(0);
        let brightness_labels = [
            crate::i18n::tr("settings-backdrop-brightness-darker"),
            crate::i18n::tr("settings-backdrop-brightness-normal"),
            crate::i18n::tr("settings-backdrop-brightness-brighter"),
        ];
        let brightness_index = match s.backdrop_brightness {
            BackdropBrightness::Darker => 0,
            BackdropBrightness::Normal => 1,
            BackdropBrightness::Brighter => 2,
        };
        let appearance = kit::card_of(k, |k| {
            vec![
                kit::dropdown_row(
                    k,
                    "general-language",
                    tr("settings-language"),
                    None,
                    language_labels.iter().map(|l| l.clone().into()).collect(),
                    language_index,
                    false,
                    Self::h(cx, |this, index: usize, _, cx| {
                        let language = match index {
                            1 => Language::En,
                            2 => Language::Ru,
                            _ => Language::System,
                        };
                        this.edit(cx, move |settings| settings.language = language);
                    }),
                ),
                kit::dropdown_row(
                    k,
                    "general-theme",
                    tr("settings-theme"),
                    None,
                    theme_labels.iter().map(|l| l.clone().into()).collect(),
                    theme_index,
                    false,
                    Self::h(cx, |this, index: usize, _, cx| {
                        let theme = match index {
                            1 => Theme::Light,
                            2 => Theme::Dark,
                            _ => Theme::Auto,
                        };
                        this.edit(cx, move |settings| settings.theme = theme);
                        // The popup resolves its palette from settings.
                        crate::popup::send_command(crate::popup::PopupCommand::AppearanceChanged);
                    }),
                ),
                kit::dropdown_row(
                    k,
                    "general-material",
                    tr("settings-material"),
                    Some(&crate::i18n::tr("settings-material-description")),
                    material_labels.iter().map(|l| l.clone().into()).collect(),
                    material_index,
                    false,
                    Self::h(cx, |this, index: usize, _, cx| {
                        let material = match index {
                            1 => PopupMaterial::Mica,
                            2 => PopupMaterial::Solid,
                            _ => PopupMaterial::Acrylic,
                        };
                        this.edit(cx, move |settings| settings.popup_material = material);
                    }),
                ),
                kit::dropdown_row(
                    k,
                    "general-backdrop-brightness",
                    tr("settings-backdrop-brightness"),
                    Some(&crate::i18n::tr("settings-backdrop-brightness-description")),
                    brightness_labels.iter().map(|l| l.clone().into()).collect(),
                    brightness_index,
                    s.popup_material == PopupMaterial::Solid,
                    Self::h(cx, |this, index: usize, _, cx| {
                        let brightness = match index {
                            1 => BackdropBrightness::Normal,
                            2 => BackdropBrightness::Brighter,
                            _ => BackdropBrightness::Darker,
                        };
                        this.edit(cx, move |settings| {
                            settings.backdrop_brightness = brightness
                        });
                        crate::popup::send_command(crate::popup::PopupCommand::AppearanceChanged);
                    }),
                ),
                Row::new("row-general-tray-icon", tr("settings-tray-icon"))
                    .trailing(kit::icon_choices(
                        k,
                        "general-tray-icon",
                        tray_icon_choices,
                        tray_icon_index,
                        Self::h(cx, |this, index: usize, _, cx| {
                            let icon = TrayIcon::ALL.get(index).copied().unwrap_or_default();
                            this.edit(cx, move |settings| settings.tray_icon = icon);
                        }),
                    ))
                    .render(k),
            ]
        });

        let behavior = kit::card_of(k, |k| {
            let interval_index = UPDATE_INTERVALS
                .iter()
                .position(|ms| *ms >= s.update_interval_ms)
                .unwrap_or(UPDATE_INTERVALS.len() - 1);
            vec![
                kit::toggle_row(
                    k,
                    "general-startup",
                    tr("settings-launch-at-startup"),
                    None,
                    s.launch_at_startup,
                    Self::h(cx, |this, value: bool, _, cx| {
                        this.edit(cx, move |settings| settings.launch_at_startup = value)
                    }),
                ),
                kit::toggle_row(
                    k,
                    "general-restore",
                    tr("settings-restore-levels"),
                    None,
                    s.restore_levels,
                    Self::h(cx, |this, value: bool, _, cx| {
                        this.edit(cx, move |settings| settings.restore_levels = value)
                    }),
                ),
                kit::dropdown_row(
                    k,
                    "general-interval",
                    tr("settings-update-interval"),
                    Some(&crate::i18n::tr("settings-update-interval-description")),
                    UPDATE_INTERVALS
                        .iter()
                        .map(|ms| {
                            crate::i18n::format("settings-ms", &[("ms", ms.to_string())]).into()
                        })
                        .collect(),
                    interval_index as i32,
                    false,
                    Self::h(cx, |this, index: usize, _, cx| {
                        let ms = UPDATE_INTERVALS[index.min(UPDATE_INTERVALS.len() - 1)];
                        this.edit(cx, move |settings| settings.update_interval_ms = ms);
                    }),
                ),
                kit::dropdown_row(
                    k,
                    "general-slider-step",
                    tr("settings-slider-step"),
                    Some(&crate::i18n::tr("settings-slider-step-description")),
                    SLIDER_STEPS
                        .iter()
                        .map(|step| step.to_string().into())
                        .collect(),
                    SLIDER_STEPS
                        .iter()
                        .position(|step| *step == s.slider_step())
                        .unwrap_or(0) as i32,
                    false,
                    Self::h(cx, |this, index: usize, _, cx| {
                        let step = SLIDER_STEPS[index.min(SLIDER_STEPS.len() - 1)];
                        this.edit(cx, move |settings| settings.slider_step = step);
                    }),
                ),
            ]
        });

        let import = kit::card_of(k, |k| {
            let button = Button::new("general-import", tr("settings-import-button"))
                .on_click(Self::h(cx, |this, (), _, cx| this.import_twinkle(cx)))
                .render(k);
            vec![
                Row::new("general-import-row", tr("settings-import-twinkle"))
                    .description(k, tr("settings-import-description"))
                    .trailing(button)
                    .render(k),
            ]
        });

        vec![
            kit::section_heading(k, tr("settings-section-appearance")),
            appearance,
            kit::section_heading(k, tr("settings-section-behavior")),
            behavior,
            kit::section_heading(k, tr("settings-section-migration")),
            import,
        ]
    }

    fn import_twinkle(&mut self, cx: &mut Context<Self>) {
        let Some(path) = crate::twinkle_import::find() else {
            self.show_notice(tr("settings-import-none"), cx);
            return;
        };
        match crate::twinkle_import::load(&path) {
            Ok(pending) => {
                let count = self
                    .model
                    .update(cx, |model, cx| model.run_import(pending, cx));
                self.show_notice(
                    crate::i18n::format("settings-import-done", &[("count", count.to_string())]),
                    cx,
                );
            }
            Err(error) => {
                crate::logger::error(format!("Twinkle Tray import failed: {error:#}"));
                self.show_notice(tr("settings-import-failed"), cx);
            }
        }
    }

    pub(super) fn monitors_page(
        &mut self,
        k: &mut Kit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let rows = self.model.read(cx).all_rows();
        let mut page = Vec::new();
        if rows.is_empty() {
            page.push(kit::card_of(k, |k| {
                vec![Row::new("monitors-empty", tr("popup-no-displays")).render(k)]
            }));
            return page;
        }
        let count = rows.len();
        for (index, row) in rows.into_iter().enumerate() {
            let id = row.monitor.id.clone();
            let heading = if row.settings.name.is_some() {
                format!("{} ({})", row.name, row.monitor.name)
            } else {
                row.name.clone()
            };
            page.push(kit::section_heading(k, heading));

            let name_id = id.clone();
            let input = self.input(
                format!("name-{id}"),
                row.settings.name.as_deref().unwrap_or(""),
                row.monitor.name.clone(),
                window,
                cx,
                move |this, text, _, _, cx| {
                    let id = name_id.clone();
                    let name = Some(text.trim().to_owned()).filter(|name| !name.is_empty());
                    this.edit(cx, move |settings| {
                        settings.monitors.entry(id).or_default().name = name;
                    });
                },
            );
            let name_field = kit::text_field(k, &input, Some(240.0), window, cx);

            let hide_id = id.clone();
            let hidden = kit::toggle_row(
                k,
                format!("hide-{id}"),
                tr("monitors-hidden"),
                Some(tr("monitors-hidden-description")),
                row.settings.hidden,
                Self::h(cx, move |this, value: bool, _, cx| {
                    let id = hide_id.clone();
                    this.edit(cx, move |settings| {
                        settings.monitors.entry(id).or_default().hidden = value;
                    });
                }),
            );

            // Only displays that answer VCP 0x12 get the option.
            let contrast =
                (row.monitor.contrast.is_some() || row.settings.show_contrast).then(|| {
                    let contrast_id = id.clone();
                    kit::toggle_row(
                        k,
                        format!("contrast-{id}"),
                        tr("monitors-contrast"),
                        Some(tr("monitors-contrast-description")),
                        row.settings.show_contrast,
                        Self::h(cx, move |this, value: bool, _, cx| {
                            let id = contrast_id.clone();
                            this.edit(cx, move |settings| {
                                settings.monitors.entry(id).or_default().show_contrast = value;
                            });
                        }),
                    )
                });

            // Only displays that list input sources (VCP 0x60) get the option.
            let inputs = (row.monitor.inputs.len() > 1 || row.settings.show_inputs).then(|| {
                let inputs_id = id.clone();
                let available = row
                    .monitor
                    .inputs
                    .iter()
                    .map(|value| row.settings.input_name(*value))
                    .collect::<Vec<_>>()
                    .join(", ");
                let description = if available.is_empty() {
                    crate::i18n::tr("monitors-inputs-description")
                } else {
                    format!(
                        "{} {}",
                        crate::i18n::tr("monitors-inputs-description"),
                        crate::i18n::format("monitors-inputs-available", &[("inputs", available)])
                    )
                };
                kit::toggle_row(
                    k,
                    format!("inputs-{id}"),
                    tr("monitors-inputs"),
                    Some(description.into()),
                    row.settings.show_inputs,
                    Self::h(cx, move |this, value: bool, _, cx| {
                        let id = inputs_id.clone();
                        this.edit(cx, move |settings| {
                            settings.monitors.entry(id).or_default().show_inputs = value;
                        });
                    }),
                )
            });

            // One row per input source while switching is on: rename, hide.
            let mut input_rows = Vec::new();
            if row.settings.show_inputs {
                for value in row.monitor.inputs.clone() {
                    let name_id = id.clone();
                    let input = self.input(
                        format!("input-name-{id}-{value}"),
                        row.settings
                            .input_names
                            .get(&MonitorSettings::input_key(value))
                            .map(String::as_str)
                            .unwrap_or(""),
                        crate::monitors::input_label(value),
                        window,
                        cx,
                        move |this, text, _, _, cx| {
                            let id = name_id.clone();
                            let name = text.trim().to_owned();
                            this.edit(cx, move |settings| {
                                let names =
                                    &mut settings.monitors.entry(id).or_default().input_names;
                                if name.is_empty() {
                                    names.remove(&MonitorSettings::input_key(value));
                                } else {
                                    names.insert(MonitorSettings::input_key(value), name);
                                }
                            });
                        },
                    );
                    let field = kit::text_field(k, &input, Some(160.0), window, cx);
                    let visible_id = id.clone();
                    let visible = kit::toggle(
                        k,
                        format!("input-visible-{id}-{value}"),
                        !row.settings.hidden_inputs.contains(&value),
                        false,
                        Self::h(cx, move |this, shown: bool, _, cx| {
                            let id = visible_id.clone();
                            this.edit(cx, move |settings| {
                                let hidden =
                                    &mut settings.monitors.entry(id).or_default().hidden_inputs;
                                hidden.retain(|known| *known != value);
                                if !shown {
                                    hidden.push(value);
                                }
                            });
                        }),
                    );
                    let mut input_row = Row::new(
                        format!("input-row-{id}-{value}"),
                        crate::monitors::input_label(value),
                    );
                    if row.monitor.input == Some(value) {
                        input_row = input_row.description(k, tr("monitors-input-current"));
                    }
                    input_rows.push(
                        input_row
                            .trailing(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(12.0))
                                    .child(field)
                                    .child(visible)
                                    .into_any_element(),
                            )
                            .render(k),
                    );
                }
            }

            let min_id = id.clone();
            let min = row.settings.min;
            let max = row.settings.max;
            let min_slider = kit::slider(
                k,
                format!("min-{id}"),
                f32::from(min),
                SliderRange {
                    min: 0.0,
                    max: 100.0,
                    step: 1.0,
                },
                200.0,
                Self::h(cx, move |this, value: f32, _, cx| {
                    let id = min_id.clone();
                    let value = (value.round() as u8).min(max.saturating_sub(1));
                    this.edit(cx, move |settings| {
                        settings.monitors.entry(id).or_default().min = value;
                    });
                }),
            );
            let max_id = id.clone();
            let max_slider = kit::slider(
                k,
                format!("max-{id}"),
                f32::from(max),
                SliderRange {
                    min: 0.0,
                    max: 100.0,
                    step: 1.0,
                },
                200.0,
                Self::h(cx, move |this, value: f32, _, cx| {
                    let id = max_id.clone();
                    let value = (value.round() as u8).max(min.saturating_add(1)).min(100);
                    this.edit(cx, move |settings| {
                        settings.monitors.entry(id).or_default().max = value;
                    });
                }),
            );
            let value_label = |k: &Kit, value: u8| {
                div()
                    .w(px(36.0))
                    .flex()
                    .justify_end()
                    .child(kit::text(format!("{value}"), 14.0, k.theme.text_secondary))
                    .into_any_element()
            };
            let with_value = |k: &Kit, slider: AnyElement, value: u8| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .child(slider)
                    .child(value_label(k, value))
                    .into_any_element()
            };
            let min_row = Row::new(format!("min-row-{id}"), tr("monitors-min"))
                .description(k, tr("monitors-min-description"))
                .trailing(with_value(k, min_slider, min))
                .render(k);
            let max_row = Row::new(format!("max-row-{id}"), tr("monitors-max"))
                .description(k, tr("monitors-max-description"))
                .trailing(with_value(k, max_slider, max))
                .render(k);

            let up_id = id.clone();
            let down_id = id.clone();
            let up = Button::new(format!("up-{id}"), tr("monitors-move-up"))
                .disabled(index == 0)
                .on_click(Self::h(cx, move |this, (), _, cx| {
                    let id = up_id.clone();
                    this.model
                        .update(cx, |model, cx| model.move_monitor(&id, -1, cx));
                }))
                .render(k);
            let down = Button::new(format!("down-{id}"), tr("monitors-move-down"))
                .disabled(index + 1 == count)
                .on_click(Self::h(cx, move |this, (), _, cx| {
                    let id = down_id.clone();
                    this.model
                        .update(cx, |model, cx| model.move_monitor(&id, 1, cx));
                }))
                .render(k);
            let order = div()
                .flex()
                .gap(px(8.0))
                .child(up)
                .child(down)
                .into_any_element();

            let backend = match row.monitor.backend {
                BrightnessBackend::Ddc { vcp } => {
                    crate::i18n::format("monitors-backend-ddc", &[("vcp", format!("0x{vcp:02X}"))])
                }
                BrightnessBackend::Wmi => crate::i18n::tr("monitors-backend-wmi"),
                BrightnessBackend::Unsupported => crate::i18n::tr("monitors-backend-unsupported"),
            };
            let kind = match row.monitor.kind {
                MonitorKind::Internal => crate::i18n::tr("monitors-kind-internal"),
                MonitorKind::External => crate::i18n::tr("monitors-kind-external"),
            };
            let mut details = format!("{kind} · {backend} · {}", row.monitor.model);
            if row.monitor.sdr.is_some() {
                details.push_str(" · HDR");
            }
            let details_text = kit::text(details, 14.0, k.theme.text_secondary).into_any_element();

            // DDC/CI displays may answer brightness on a nonstandard code.
            let vcp_row = (matches!(row.monitor.backend, BrightnessBackend::Ddc { .. })
                || (row.monitor.kind == MonitorKind::External
                    && (!row.monitor.vcp_codes.is_empty()
                        || row.monitor.backend == BrightnessBackend::Unsupported))
                || row.settings.brightness_vcp.is_some())
            .then(|| {
                let mut codes: Vec<u8> = BRIGHTNESS_VCPS
                    .iter()
                    .copied()
                    .filter(|code| {
                        row.monitor.vcp_codes.is_empty() || row.monitor.vcp_codes.contains(code)
                    })
                    .collect();
                // Then every other code the display lists: some answer
                // brightness only on a vendor code such as 0xE1.
                for code in row.monitor.vcp_codes.iter().copied() {
                    if !codes.contains(&code) {
                        codes.push(code);
                    }
                }
                if let Some(code) = row.settings.brightness_vcp
                    && !codes.contains(&code)
                {
                    codes.push(code);
                }
                let mut labels = vec![tr("monitors-vcp-auto")];
                labels.extend(
                    codes
                        .iter()
                        .map(|code| SharedString::from(vcp_label(*code))),
                );
                let selected = row
                    .settings
                    .brightness_vcp
                    .and_then(|code| codes.iter().position(|known| *known == code))
                    .map_or(0, |index| index + 1);
                let vcp_id = id.clone();
                kit::dropdown_row(
                    k,
                    &format!("vcp-{id}"),
                    tr("monitors-vcp"),
                    Some(&crate::i18n::tr("monitors-vcp-description")),
                    labels,
                    selected as i32,
                    false,
                    Self::h(cx, move |this, index: usize, _, cx| {
                        let id = vcp_id.clone();
                        let vcp = index.checked_sub(1).and_then(|i| codes.get(i).copied());
                        this.edit(cx, move |settings| {
                            settings.monitors.entry(id).or_default().brightness_vcp = vcp;
                        });
                    }),
                )
            });

            let card = kit::card_of(k, |k| {
                let mut rows = vec![
                    Row::new(format!("name-row-{id}"), tr("monitors-rename"))
                        .trailing(name_field)
                        .render(k),
                    hidden,
                    min_row,
                    max_row,
                ];
                rows.extend(contrast);
                rows.extend(inputs);
                rows.extend(input_rows);
                rows.extend(vcp_row);
                rows.push(
                    Row::new(format!("order-row-{id}"), tr("monitors-order"))
                        .trailing(order)
                        .render(k),
                );
                rows.push(
                    Row::new(format!("info-row-{id}"), tr("monitors-info"))
                        .trailing(details_text)
                        .render(k),
                );
                rows
            });
            page.push(card);
        }
        page.push(
            div()
                .pt(px(8.0))
                .child(kit::caption(k, tr("monitors-footnote")))
                .into_any_element(),
        );
        page
    }
}
