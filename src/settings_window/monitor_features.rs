//! Calibration and additional DDC controls, using the existing live settings kit.
use super::{
    input::HasInputs,
    kit::{self, Button, Kit, Row},
    window::SettingsWindow,
};
use crate::app::MonitorRow;
use gpui::{
    AnyElement, Context, IntoElement, ParentElement, SharedString, Styled, Window, div, px,
};

fn tr(key: &str) -> SharedString {
    crate::i18n::tr(key).into()
}

pub(crate) fn parse_curve(
    text: &str,
    min: u8,
    max: u8,
) -> anyhow::Result<Vec<crate::levels::CalibrationPoint>> {
    let points = if text.trim().is_empty() {
        Vec::new()
    } else {
        text.split(',')
            .map(|pair| {
                let (slider, hardware) = pair
                    .trim()
                    .split_once(':')
                    .ok_or_else(|| anyhow::anyhow!("expected slider:hardware"))?;
                Ok(crate::levels::CalibrationPoint {
                    slider: slider.trim().parse()?,
                    hardware: hardware.trim().parse()?,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?
    };
    anyhow::ensure!(
        crate::levels::valid_curve(&points, min, max),
        "calibration must be strictly increasing inside min/max"
    );
    Ok(points)
}

impl SettingsWindow {
    /// Rows that shape the brightness slider: the SDR choice on HDR displays
    /// that also have hardware brightness, and the calibration curve.
    pub(super) fn monitor_slider_rows(
        &mut self,
        row: &MonitorRow,
        k: &mut Kit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let id = row.monitor.id.clone();
        let mut rows = Vec::new();
        if row.monitor.sdr.is_some() && row.monitor.brightness.is_some() {
            let target = id.clone();
            rows.push(kit::toggle_row(
                k,
                format!("sdr-primary-{id}"),
                tr("features-sdr-primary"),
                Some(tr("features-sdr-primary-help")),
                row.settings.sdr_primary,
                Self::h(cx, move |this, value: bool, _, cx| {
                    let id = target.clone();
                    this.edit(cx, move |s| {
                        s.monitors.entry(id).or_default().sdr_primary = value
                    });
                }),
            ));
        }
        let curve = row
            .settings
            .calibration
            .iter()
            .map(|p| format!("{}:{}", p.slider, p.hardware))
            .collect::<Vec<_>>()
            .join(", ");
        let target = id.clone();
        let (min, max) = (row.settings.min, row.settings.max);
        let input = self.input(
            format!("calibration-{id}"),
            &curve,
            "25:10, 50:30, 75:60",
            window,
            cx,
            move |this, text, _, _, cx| match parse_curve(&text, min, max) {
                Ok(curve) => {
                    let id = target.clone();
                    this.edit(cx, move |s| {
                        s.monitors.entry(id).or_default().calibration = curve
                    });
                }
                Err(_) => this.show_error(tr("features-curve-invalid"), cx),
            },
        );
        rows.push(
            Row::new(format!("calibration-row-{id}"), tr("features-calibration"))
                .description(k, tr("features-calibration-help"))
                .trailing(kit::text_field(k, &input, Some(240.0), window, cx))
                .render(k),
        );
        rows
    }

    /// DDC/CI extras: the volume slider and user-defined VCP sliders.
    pub(super) fn monitor_ddc_rows(
        &mut self,
        row: &MonitorRow,
        k: &mut Kit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let ddc = matches!(
            row.monitor.backend,
            crate::monitors::BrightnessBackend::Ddc { .. }
        ) || !row.monitor.vcp_codes.is_empty();
        if !ddc {
            return Vec::new();
        }
        let id = row.monitor.id.clone();
        let mut rows = Vec::new();
        let target = id.clone();
        rows.push(kit::toggle_row(
            k,
            format!("volume-{id}"),
            tr("features-volume"),
            Some(tr("features-volume-help")),
            row.settings.show_volume,
            Self::h(cx, move |this, value: bool, _, cx| {
                let id = target.clone();
                this.edit(cx, move |s| {
                    s.monitors.entry(id).or_default().show_volume = value
                });
            }),
        ));

        let target = id.clone();
        let code_input = self.input(
            format!("new-vcp-{id}"),
            "",
            "62",
            window,
            cx,
            |_, _, _, _, _| {},
        );
        let input_state = code_input.clone();
        let add = Button::new(format!("add-vcp-{id}"), tr("features-add"))
            .with_icon("plus-bold")
            .on_click(Self::h(cx, move |this, (), _, cx| {
                let text = input_state.read(cx).text().to_string();
                let code = u8::from_str_radix(
                    text.trim()
                        .trim_start_matches("0x")
                        .trim_start_matches("0X"),
                    16,
                )
                .ok()
                .filter(|c| *c != 0);
                if let Some(code) = code {
                    let id = target.clone();
                    this.edit(cx, move |s| {
                        s.monitors
                            .entry(id)
                            .or_default()
                            .vcp_controls
                            .entry(format!("{code:02X}"))
                            .or_default();
                    });
                } else {
                    this.show_error(tr("features-code-invalid"), cx);
                }
            }))
            .render(k);
        let mut help = crate::i18n::tr("features-vcp-help");
        if !row.monitor.vcp_codes.is_empty() {
            let codes = row
                .monitor
                .vcp_codes
                .iter()
                .map(|code| format!("{code:02X}"))
                .collect::<Vec<_>>()
                .join(", ");
            help = format!(
                "{help} {}",
                crate::i18n::format("features-detected", &[("codes", codes)])
            );
        }
        rows.push(
            Row::new(format!("new-vcp-row-{id}"), tr("features-vcp"))
                .description(k, help)
                .trailing(kit::text_field(k, &code_input, Some(72.0), window, cx))
                .trailing(add)
                .render(k),
        );

        for (code, feature) in &row.settings.vcp_controls {
            rows.push(self.vcp_control_row(k, &id, code, feature, window, cx));
        }
        rows
    }

    /// One user-defined VCP slider: name and range below the title, the
    /// on/off switch and removal at the end.
    fn vcp_control_row(
        &mut self,
        k: &mut Kit,
        id: &str,
        code: &str,
        feature: &crate::settings::VcpControl,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (target, key) = (id.to_owned(), code.to_owned());
        let name = self.input(
            format!("vcp-name-{id}-{code}"),
            &feature.name,
            format!("VCP 0x{code}"),
            window,
            cx,
            move |this, text, _, _, cx| {
                let (id, key) = (target.clone(), key.clone());
                this.edit(cx, move |s| {
                    if let Some(feature) =
                        s.monitors.entry(id).or_default().vcp_controls.get_mut(&key)
                    {
                        feature.name = text;
                    }
                });
            },
        );
        let mut fields = div()
            .pt(px(8.0))
            .flex()
            .flex_wrap()
            .gap(px(8.0))
            .child(kit::field(
                k,
                tr("monitors-rename"),
                kit::text_field(k, &name, Some(160.0), window, cx),
            ));
        for is_min in [true, false] {
            let (target, key) = (id.to_owned(), code.to_owned());
            let value = if is_min { feature.min } else { feature.max };
            let field = self.input(
                format!("vcp-range-{id}-{code}-{is_min}"),
                &value.to_string(),
                if is_min { "0" } else { "100" },
                window,
                cx,
                move |this, text, _, _, cx| {
                    if let Ok(value) = text.trim().parse::<u16>() {
                        let (id, key) = (target.clone(), key.clone());
                        this.edit(cx, move |s| {
                            if let Some(feature) =
                                s.monitors.entry(id).or_default().vcp_controls.get_mut(&key)
                            {
                                if is_min && value < feature.max {
                                    feature.min = value;
                                } else if !is_min && value > feature.min {
                                    feature.max = value;
                                }
                            }
                        });
                    }
                },
            );
            fields = fields.child(kit::field(
                k,
                tr(if is_min {
                    "monitors-min"
                } else {
                    "monitors-max"
                }),
                kit::text_field(k, &field, Some(72.0), window, cx),
            ));
        }
        let (target, key) = (id.to_owned(), code.to_owned());
        let enabled = kit::toggle(
            k,
            format!("vcp-enabled-{id}-{code}"),
            feature.enabled,
            false,
            Self::h(cx, move |this, value: bool, _, cx| {
                let (id, key) = (target.clone(), key.clone());
                this.edit(cx, move |s| {
                    if let Some(feature) =
                        s.monitors.entry(id).or_default().vcp_controls.get_mut(&key)
                    {
                        feature.enabled = value;
                    }
                });
            }),
        );
        let (target, key) = (id.to_owned(), code.to_owned());
        let delete = Button::icon_only(format!("delete-vcp-{id}-{code}"), "x-circle-fill")
            .ghost()
            .tooltip(tr("features-delete"))
            .on_click(Self::h(cx, move |this, (), _, cx| {
                let (id, key) = (target.clone(), key.clone());
                this.inputs_mut().remove_prefixed("vcp-");
                this.edit(cx, move |s| {
                    s.monitors.entry(id).or_default().vcp_controls.remove(&key);
                });
            }))
            .render(k);
        let title = if feature.name.trim().is_empty() {
            format!("VCP 0x{code}")
        } else {
            feature.name.clone()
        };
        Row::new(format!("vcp-row-{id}-{code}"), title)
            .description(k, format!("VCP 0x{code}"))
            .detail(fields.into_any_element())
            .trailing(enabled)
            .trailing(delete)
            .render(k)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calibration_editor_rejects_non_monotone_or_out_of_range_points() {
        assert_eq!(parse_curve("25:15, 75:70", 10, 90).unwrap().len(), 2);
        assert!(parse_curve("", 10, 90).unwrap().is_empty());
        for text in [
            "25",
            "50:20, 25:30",
            "25:50, 75:40",
            "0:10",
            "100:90",
            "25:5",
            "50:100",
            "25:15,25:20",
        ] {
            assert!(parse_curve(text, 10, 90).is_err(), "{text}");
        }
    }
}
