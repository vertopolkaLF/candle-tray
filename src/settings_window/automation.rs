//! The Automation page: the time-of-day schedule and dimming when idle.

use gpui::{
    AnyElement, Context, IntoElement, ParentElement, SharedString, Styled, Window, div, px,
};

use super::kit::{self, Button, Kit, Row, SliderRange};
use super::window::SettingsWindow;
use crate::settings::{
    ProfileTrigger, ScheduleEntry, ScheduleTarget, ScheduleTime, TriggerCondition,
};

/// Fade durations offered for schedule changes, in seconds.
const FADES: [u32; 5] = [0, 30, 60, 300, 900];
/// Idle times offered before dimming, in minutes.
const IDLE_MINUTES: [u16; 7] = [1, 2, 5, 10, 15, 30, 60];

const PERCENT: SliderRange = SliderRange {
    min: 0.0,
    max: 100.0,
    step: 1.0,
};

fn tr(key: &str) -> SharedString {
    crate::i18n::tr(key).into()
}

/// `07:30` for minutes after midnight.
fn clock_text(minutes: u16) -> String {
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

/// Parses `7:30`, `07:30` or `7` into minutes after midnight.
pub(super) fn parse_clock(text: &str) -> Option<u16> {
    let text = text.trim();
    let (hours, minutes) = match text.split_once([':', '.']) {
        Some((hours, minutes)) => (hours.trim(), minutes.trim()),
        None => (text, "0"),
    };
    let hours: u16 = hours.parse().ok()?;
    let minutes: u16 = minutes.parse().ok()?;
    (hours < 24 && minutes < 60).then_some(hours * 60 + minutes)
}

/// `+30`, `-15` or `0` minutes.
fn offset_text(offset: i16) -> String {
    if offset > 0 {
        format!("+{offset}")
    } else {
        offset.to_string()
    }
}

pub(super) fn parse_offset(text: &str) -> Option<i16> {
    let text = text.trim().trim_start_matches('+').replace('\u{2212}', "-");
    let offset: i16 = text.trim().parse().ok()?;
    (-720..=720).contains(&offset).then_some(offset)
}

/// The file name of an app as Windows reports it: `C:\Games\Game.exe`
/// and `game` both become `game.exe`.
pub(super) fn normalize_exe(text: &str) -> String {
    let name = text
        .trim()
        .trim_matches('"')
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    if name.is_empty() || name.ends_with(".exe") {
        name
    } else {
        format!("{name}.exe")
    }
}

/// Parses `55.75, 37.62` into latitude and longitude.
pub(super) fn parse_location(text: &str) -> Option<(f64, f64)> {
    let (latitude, longitude) = text.split_once([',', ';'])?;
    let latitude: f64 = latitude.trim().parse().ok()?;
    let longitude: f64 = longitude.trim().parse().ok()?;
    ((-90.0..=90.0).contains(&latitude) && (-180.0..=180.0).contains(&longitude))
        .then_some((latitude, longitude))
}

impl SettingsWindow {
    pub(super) fn automation_page(
        &mut self,
        k: &mut Kit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let s = self.model.read(cx).settings.clone();
        let profile_names: Vec<String> = (0..s.profiles.len())
            .map(|index| self.model.read(cx).profile_name(index))
            .collect();
        let schedule = &s.schedule;
        let mut page = vec![kit::section_heading(k, tr("automation-section-schedule"))];

        let location_text = schedule
            .location()
            .map(|(latitude, longitude)| format!("{latitude}, {longitude}"))
            .unwrap_or_default();
        let location = self.input(
            "schedule-location",
            &location_text,
            "55.75, 37.62",
            window,
            cx,
            |this, text, _, _, cx| {
                let parsed = parse_location(&text);
                if parsed.is_none() && !text.trim().is_empty() {
                    this.show_notice(tr("automation-location-invalid"), cx);
                    return;
                }
                this.edit(cx, move |settings| {
                    settings.schedule.latitude = parsed.map(|(latitude, _)| latitude);
                    settings.schedule.longitude = parsed.map(|(_, longitude)| longitude);
                });
            },
        );
        let location_field = kit::text_field(k, &location, Some(200.0), window, cx);
        let detect = Button::new(
            "schedule-location-detect",
            if self.locating {
                tr("automation-location-detecting")
            } else {
                tr("automation-location-detect")
            },
        )
        .disabled(self.locating)
        .on_click(Self::h(cx, |this, (), _, cx| this.detect_location(cx)))
        .render(k);
        let location_controls = div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(location_field)
            .child(detect)
            .into_any_element();
        let fade_index = FADES
            .iter()
            .position(|seconds| *seconds >= schedule.fade_seconds)
            .unwrap_or(FADES.len() - 1);
        page.push(kit::card_of(k, |k| {
            vec![
                kit::toggle_row(
                    k,
                    "schedule-enabled",
                    tr("automation-schedule"),
                    Some(tr("automation-schedule-description")),
                    schedule.enabled,
                    Self::h(cx, |this, value: bool, _, cx| {
                        this.edit(cx, move |settings| settings.schedule.enabled = value)
                    }),
                ),
                kit::dropdown_row(
                    k,
                    "schedule-fade",
                    tr("automation-fade"),
                    Some(&crate::i18n::tr("automation-fade-description")),
                    FADES
                        .iter()
                        .map(|seconds| match seconds {
                            0 => tr("automation-fade-instant"),
                            s if s % 60 == 0 => crate::i18n::format(
                                "automation-minutes",
                                &[("count", (s / 60).to_string())],
                            )
                            .into(),
                            s => crate::i18n::format(
                                "automation-seconds",
                                &[("count", s.to_string())],
                            )
                            .into(),
                        })
                        .collect(),
                    fade_index as i32,
                    false,
                    Self::h(cx, |this, index: usize, _, cx| {
                        let seconds = FADES[index.min(FADES.len() - 1)];
                        this.edit(cx, move |settings| settings.schedule.fade_seconds = seconds);
                    }),
                ),
                Row::new("schedule-location-row", tr("automation-location"))
                    .description(k, tr("automation-location-description"))
                    .trailing(location_controls)
                    .render(k),
            ]
        }));

        for (index, entry) in schedule.entries.iter().enumerate() {
            page.push(self.schedule_entry(k, index, *entry, &profile_names, window, cx));
        }
        let add = Button::new("schedule-add", tr("automation-add"))
            .accent()
            .with_icon("plus-bold")
            .on_click(Self::h(cx, |this, (), _, cx| {
                this.edit(cx, |settings| {
                    // Evening by default when a morning entry exists.
                    let time = if settings.schedule.entries.is_empty() {
                        ScheduleTime::Clock { minutes: 8 * 60 }
                    } else {
                        ScheduleTime::Clock { minutes: 21 * 60 }
                    };
                    let target = ScheduleTarget::Level {
                        level: if settings.schedule.entries.is_empty() {
                            80
                        } else {
                            30
                        },
                    };
                    settings
                        .schedule
                        .entries
                        .push(ScheduleEntry { time, target });
                });
            }))
            .render(k);
        page.push(div().pt(px(4.0)).into_any_element());
        page.push(kit::card_of(k, |k| {
            vec![
                Row::new("schedule-add-row", tr("automation-add"))
                    .description(k, tr("automation-add-description"))
                    .trailing(add)
                    .render(k),
            ]
        }));

        let idle = s.idle_dim;
        let minutes_index = IDLE_MINUTES
            .iter()
            .position(|minutes| *minutes >= idle.minutes)
            .unwrap_or(IDLE_MINUTES.len() - 1);
        page.push(kit::section_heading(k, tr("automation-section-idle")));
        let level_slider = kit::slider(
            k,
            "idle-level",
            f32::from(idle.level),
            PERCENT,
            200.0,
            Self::h(cx, |this, value: f32, _, cx| {
                let level = value.round().clamp(0.0, 100.0) as u8;
                this.edit(cx, move |settings| settings.idle_dim.level = level);
            }),
        );
        let level = with_value(k, level_slider, idle.level);
        page.push(kit::card_of(k, |k| {
            vec![
                kit::toggle_row(
                    k,
                    "idle-enabled",
                    tr("automation-idle"),
                    Some(tr("automation-idle-description")),
                    idle.enabled,
                    Self::h(cx, |this, value: bool, _, cx| {
                        this.edit(cx, move |settings| settings.idle_dim.enabled = value)
                    }),
                ),
                kit::dropdown_row(
                    k,
                    "idle-minutes",
                    tr("automation-idle-after"),
                    None,
                    IDLE_MINUTES
                        .iter()
                        .map(|minutes| {
                            crate::i18n::format(
                                "automation-minutes",
                                &[("count", minutes.to_string())],
                            )
                            .into()
                        })
                        .collect(),
                    minutes_index as i32,
                    false,
                    Self::h(cx, |this, index: usize, _, cx| {
                        let minutes = IDLE_MINUTES[index.min(IDLE_MINUTES.len() - 1)];
                        this.edit(cx, move |settings| settings.idle_dim.minutes = minutes);
                    }),
                ),
                Row::new("idle-level-row", tr("automation-idle-level"))
                    .description(k, tr("automation-idle-level-description"))
                    .trailing(level)
                    .render(k),
                kit::toggle_row(
                    k,
                    "idle-busy",
                    tr("automation-idle-busy"),
                    Some(tr("automation-idle-busy-description")),
                    idle.respect_busy,
                    Self::h(cx, |this, value: bool, _, cx| {
                        this.edit(cx, move |settings| settings.idle_dim.respect_busy = value)
                    }),
                ),
            ]
        }));
        page.extend(self.triggers_section(k, &s.triggers, &profile_names, window, cx));
        // Only machines with a light sensor (or a setting left on) see it.
        if crate::light_sensor::available_cached() == Some(true) || s.adaptive.enabled {
            page.extend(self.adaptive_section(k, s.adaptive, cx));
        }
        page
    }

    /// Start consent on the UI thread and poll the background location lookup.
    fn detect_location(&mut self, cx: &mut Context<Self>) {
        self.locating = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let mut detection = crate::location::start();
            let result = loop {
                if let Some(result) = detection.poll() {
                    break result;
                }
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(200))
                    .await;
            };
            let _ = this.update(cx, |this, cx| {
                this.locating = false;
                match result {
                    Ok((latitude, longitude)) => {
                        this.edit(cx, move |settings| {
                            settings.schedule.latitude = Some(latitude);
                            settings.schedule.longitude = Some(longitude);
                        });
                        this.show_notice(tr("automation-location-found"), cx);
                    }
                    Err(error) => {
                        crate::logger::error(format!("location detection failed: {error:#}"));
                        let denied = matches!(
                            error.downcast_ref::<crate::location::LocationError>(),
                            Some(crate::location::LocationError::Denied)
                        );
                        this.show_notice(
                            if denied {
                                tr("automation-location-denied")
                            } else {
                                tr("automation-location-failed")
                            },
                            cx,
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Brightness that follows the ambient light sensor.
    fn adaptive_section(
        &mut self,
        k: &mut Kit,
        adaptive: crate::settings::AdaptiveBrightness,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let dark = kit::slider(
            k,
            "adaptive-dark",
            f32::from(adaptive.dark_level),
            PERCENT,
            200.0,
            Self::h(cx, |this, value: f32, _, cx| {
                let level = value.round().clamp(0.0, 100.0) as u8;
                this.edit(cx, move |settings| settings.adaptive.dark_level = level);
            }),
        );
        let dark = with_value(k, dark, adaptive.dark_level);
        let bright = kit::slider(
            k,
            "adaptive-bright",
            f32::from(adaptive.bright_level),
            PERCENT,
            200.0,
            Self::h(cx, |this, value: f32, _, cx| {
                let level = value.round().clamp(0.0, 100.0) as u8;
                this.edit(cx, move |settings| settings.adaptive.bright_level = level);
            }),
        );
        let bright = with_value(k, bright, adaptive.bright_level);
        vec![
            kit::section_heading(k, tr("automation-section-adaptive")),
            kit::card_of(k, |k| {
                vec![
                    kit::toggle_row(
                        k,
                        "adaptive-enabled",
                        tr("automation-adaptive"),
                        Some(tr("automation-adaptive-description")),
                        adaptive.enabled,
                        Self::h(cx, |this, value: bool, _, cx| {
                            this.edit(cx, move |settings| settings.adaptive.enabled = value)
                        }),
                    ),
                    Row::new("adaptive-dark-row", tr("automation-adaptive-dark"))
                        .description(k, tr("automation-adaptive-dark-description"))
                        .trailing(dark)
                        .render(k),
                    Row::new("adaptive-bright-row", tr("automation-adaptive-bright"))
                        .description(k, tr("automation-adaptive-bright-description"))
                        .trailing(bright)
                        .render(k),
                ]
            }),
        ]
    }

    /// Profiles switched on by the foreground app or the power source.
    fn triggers_section(
        &mut self,
        k: &mut Kit,
        triggers: &[ProfileTrigger],
        profile_names: &[String],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut section = vec![kit::section_heading(k, tr("automation-section-triggers"))];
        for (index, trigger) in triggers.iter().enumerate() {
            let kind = match trigger.condition {
                TriggerCondition::App { .. } => 0,
                TriggerCondition::OnBattery => 1,
                TriggerCondition::OnAc => 2,
            };
            let condition = kit::dropdown(
                k,
                format!("trigger-kind-{index}"),
                vec![
                    tr("automation-trigger-app"),
                    tr("automation-trigger-battery"),
                    tr("automation-trigger-ac"),
                ],
                Some(kind),
                false,
                180.0,
                Self::h(cx, move |this, choice: usize, _, cx| {
                    this.edit(cx, move |settings| {
                        if let Some(trigger) = settings.triggers.get_mut(index) {
                            trigger.condition = match choice {
                                1 => TriggerCondition::OnBattery,
                                2 => TriggerCondition::OnAc,
                                _ => TriggerCondition::App { exe: String::new() },
                            };
                        }
                    });
                }),
            );
            let exe_field = match &trigger.condition {
                TriggerCondition::App { exe } => {
                    let input = self.input(
                        format!("trigger-exe-{index}"),
                        exe,
                        "game.exe",
                        window,
                        cx,
                        move |this, text, _, _, cx| {
                            let exe = normalize_exe(&text);
                            this.edit(cx, move |settings| {
                                if let Some(trigger) = settings.triggers.get_mut(index) {
                                    trigger.condition = TriggerCondition::App { exe };
                                }
                            });
                        },
                    );
                    Some(kit::text_field(k, &input, Some(140.0), window, cx))
                }
                _ => None,
            };
            let delete = Button::icon_only(format!("trigger-delete-{index}"), "x-circle-fill")
                .ghost()
                .tooltip(tr("automation-delete"))
                .on_click(Self::h(cx, move |this, (), _, cx| {
                    this.edit(cx, move |settings| {
                        if index < settings.triggers.len() {
                            settings.triggers.remove(index);
                        }
                    });
                }))
                .render(k);
            let when = div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(condition)
                .children(exe_field)
                .child(delete)
                .into_any_element();
            let profile = kit::dropdown(
                k,
                format!("trigger-profile-{index}"),
                profile_names
                    .iter()
                    .map(|name| SharedString::from(name.clone()))
                    .collect(),
                (trigger.profile < profile_names.len()).then_some(trigger.profile),
                false,
                200.0,
                Self::h(cx, move |this, choice: usize, _, cx| {
                    this.edit(cx, move |settings| {
                        if let Some(trigger) = settings.triggers.get_mut(index) {
                            trigger.profile = choice;
                        }
                    });
                }),
            );
            let description = matches!(trigger.condition, TriggerCondition::App { .. })
                .then(|| tr("automation-trigger-app-description"));
            section.push(kit::card_of(k, |k| {
                vec![
                    Row::new(
                        format!("trigger-when-{index}"),
                        crate::i18n::format(
                            "automation-trigger",
                            &[("number", (index + 1).to_string())],
                        ),
                    )
                    .description_opt(k, description)
                    .trailing(when)
                    .render(k),
                    Row::new(
                        format!("trigger-profile-row-{index}"),
                        tr("automation-apply"),
                    )
                    .trailing(profile)
                    .render(k),
                ]
            }));
        }
        let has_profiles = !profile_names.is_empty();
        let add = Button::new("trigger-add", tr("automation-trigger-add"))
            .accent()
            .with_icon("plus-bold")
            .disabled(!has_profiles)
            .on_click(Self::h(cx, |this, (), _, cx| {
                this.edit(cx, |settings| {
                    settings.triggers.push(ProfileTrigger::default())
                });
            }))
            .render(k);
        section.push(kit::card_of(k, |k| {
            vec![
                Row::new("trigger-add-row", tr("automation-trigger-add"))
                    .description(
                        k,
                        if has_profiles {
                            tr("automation-trigger-add-description")
                        } else {
                            tr("automation-trigger-needs-profile")
                        },
                    )
                    .trailing(add)
                    .render(k),
            ]
        }));
        section
    }

    fn schedule_entry(
        &mut self,
        k: &mut Kit,
        index: usize,
        entry: ScheduleEntry,
        profile_names: &[String],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (kind, field_text, placeholder) = match entry.time {
            ScheduleTime::Clock { minutes } => (0, clock_text(minutes), "08:00"),
            ScheduleTime::Sunrise { offset } => (1, offset_text(offset), "0"),
            ScheduleTime::Sunset { offset } => (2, offset_text(offset), "0"),
        };
        let kind_dropdown = kit::dropdown(
            k,
            format!("schedule-kind-{index}"),
            vec![
                tr("automation-at-time"),
                tr("automation-at-sunrise"),
                tr("automation-at-sunset"),
            ],
            Some(kind),
            false,
            140.0,
            Self::h(cx, move |this, choice: usize, _, cx| {
                this.edit(cx, move |settings| {
                    if let Some(entry) = settings.schedule.entries.get_mut(index) {
                        entry.time = match choice {
                            1 => ScheduleTime::Sunrise { offset: 0 },
                            2 => ScheduleTime::Sunset { offset: 0 },
                            _ => ScheduleTime::Clock { minutes: 8 * 60 },
                        };
                    }
                });
            }),
        );
        let time_input = self.input(
            format!("schedule-time-{index}-{kind}"),
            &field_text,
            placeholder,
            window,
            cx,
            move |this, text, _, _, cx| {
                let time = match kind {
                    0 => parse_clock(&text).map(|minutes| ScheduleTime::Clock { minutes }),
                    1 => parse_offset(&text).map(|offset| ScheduleTime::Sunrise { offset }),
                    _ => parse_offset(&text).map(|offset| ScheduleTime::Sunset { offset }),
                };
                let Some(time) = time else {
                    this.show_notice(tr("automation-time-invalid"), cx);
                    return;
                };
                this.edit(cx, move |settings| {
                    if let Some(entry) = settings.schedule.entries.get_mut(index) {
                        entry.time = time;
                    }
                });
            },
        );
        let time_field = kit::text_field(k, &time_input, Some(80.0), window, cx);
        let delete = Button::icon_only(format!("schedule-delete-{index}"), "x-circle-fill")
            .ghost()
            .tooltip(tr("automation-delete"))
            .on_click(Self::h(cx, move |this, (), _, cx| {
                this.edit(cx, move |settings| {
                    if index < settings.schedule.entries.len() {
                        settings.schedule.entries.remove(index);
                    }
                });
            }))
            .render(k);
        let when = div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(kind_dropdown)
            .child(time_field)
            .child(delete)
            .into_any_element();

        // When it fires today, so sunrise and sunset entries show a time.
        let settings_schedule = self.model.read(cx).settings.schedule.clone();
        let today = chrono::Local::now();
        let fires = crate::schedule::occurrence(
            &entry,
            today.date_naive(),
            &chrono::Local,
            settings_schedule.location(),
        );
        let when_description = match (entry.time, fires) {
            (ScheduleTime::Clock { .. }, _) => None,
            (_, Some(at)) => Some(crate::i18n::format(
                "automation-today-at",
                &[("time", at.format("%H:%M").to_string())],
            )),
            (_, None) if settings_schedule.location().is_none() => {
                Some(crate::i18n::tr("automation-needs-location"))
            }
            (_, None) => Some(crate::i18n::tr("automation-no-sun-today")),
        };

        let mut targets = vec![tr("automation-target-level")];
        targets.extend(
            profile_names
                .iter()
                .map(|name| SharedString::from(name.clone())),
        );
        let target_index = match entry.target {
            ScheduleTarget::Level { .. } => 0,
            ScheduleTarget::Profile { index } => index + 1,
        };
        let target_dropdown = kit::dropdown(
            k,
            format!("schedule-target-{index}"),
            targets,
            Some(target_index),
            false,
            200.0,
            Self::h(cx, move |this, choice: usize, _, cx| {
                this.edit(cx, move |settings| {
                    if let Some(entry) = settings.schedule.entries.get_mut(index) {
                        entry.target = match choice {
                            0 => ScheduleTarget::Level { level: 50 },
                            profile => ScheduleTarget::Profile { index: profile - 1 },
                        };
                    }
                });
            }),
        );
        let level = match entry.target {
            ScheduleTarget::Level { level } => {
                let slider = kit::slider(
                    k,
                    format!("schedule-level-{index}"),
                    f32::from(level),
                    PERCENT,
                    200.0,
                    Self::h(cx, move |this, value: f32, _, cx| {
                        let level = value.round().clamp(0.0, 100.0) as u8;
                        this.edit(cx, move |settings| {
                            if let Some(entry) = settings.schedule.entries.get_mut(index) {
                                entry.target = ScheduleTarget::Level { level };
                            }
                        });
                    }),
                );
                Some(with_value(k, slider, level))
            }
            ScheduleTarget::Profile { .. } => None,
        };

        kit::card_of(k, |k| {
            let mut rows = vec![
                Row::new(
                    format!("schedule-when-{index}"),
                    crate::i18n::format("automation-entry", &[("number", (index + 1).to_string())]),
                )
                .description_opt(k, when_description)
                .trailing(when)
                .render(k),
                Row::new(
                    format!("schedule-target-row-{index}"),
                    tr("automation-apply"),
                )
                .trailing(target_dropdown)
                .render(k),
            ];
            if let Some(level) = level {
                rows.push(
                    Row::new(
                        format!("schedule-level-row-{index}"),
                        tr("automation-level"),
                    )
                    .trailing(level)
                    .render(k),
                );
            }
            rows
        })
    }
}

/// A slider with its value to the right, as on the Monitors page.
fn with_value(k: &Kit, slider: AnyElement, value: u8) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(12.0))
        .child(slider)
        .child(div().w(px(36.0)).flex().justify_end().child(kit::text(
            format!("{value}"),
            14.0,
            k.theme.text_secondary,
        )))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_times_offsets_and_locations() {
        assert_eq!(parse_clock("7:30"), Some(450));
        assert_eq!(parse_clock("07.05"), Some(425));
        assert_eq!(parse_clock("21"), Some(1260));
        assert_eq!(parse_clock("24:00"), None);
        assert_eq!(parse_clock("8:60"), None);
        assert_eq!(parse_offset("+30"), Some(30));
        assert_eq!(parse_offset("\u{2212}15"), Some(-15));
        assert_eq!(parse_offset("1000"), None);
        assert_eq!(parse_location("55.75, 37.62"), Some((55.75, 37.62)));
        assert_eq!(parse_location("91, 0"), None);
        assert_eq!(parse_location("55.75"), None);
        assert_eq!(normalize_exe(r#""C:\Games\Game.EXE""#), "game.exe");
        assert_eq!(normalize_exe("vlc"), "vlc.exe");
        assert_eq!(normalize_exe("  "), "");
        assert_eq!(clock_text(450), "07:30");
        assert_eq!(offset_text(-15), "-15");
        assert_eq!(offset_text(30), "+30");
    }
}
