//! Shared, pure action semantics. Percent actions use normalized slider units;
//! Percent operations are explicitly normalized requests. Direct monitor level
//! setters remain hardware-percent APIs; conversion uses `crate::levels`.
use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::{app::MonitorRow, cli::Selector, hotkey::Hotkey};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Control {
    Brightness,
    Sdr,
    Contrast,
    Vcp(u8),
    Input,
    Power,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Operation {
    Set(i32),
    Offset(i32),
    Cycle(Vec<i32>),
}

impl Operation {
    pub fn validate(&self, max: i32, allowed: Option<&[i32]>) -> Result<()> {
        let valid = |v: i32| (0..=max).contains(&v) && allowed.is_none_or(|a| a.contains(&v));
        match self {
            Self::Set(value) => ensure!(valid(*value), "invalid action value: {value}"),
            Self::Offset(_) => {}
            Self::Cycle(values) => {
                ensure!(!values.is_empty(), "cycle needs at least one value");
                ensure!(values.iter().all(|v| valid(*v)), "invalid cycle value");
                let mut unique = values.clone();
                unique.sort_unstable();
                unique.dedup();
                ensure!(unique.len() == values.len(), "duplicate cycle values");
            }
        }
        Ok(())
    }

    /// A cycle starts at its first value if the current value is not listed.
    /// No cursor is mutated, including when validation fails.
    pub fn evaluate(&self, current: Option<i32>, max: i32, allowed: Option<&[i32]>) -> Result<i32> {
        self.validate(max, allowed)?;
        let next = match self {
            Self::Set(value) => *value,
            Self::Offset(delta) => current
                .ok_or_else(|| anyhow::anyhow!("current value unavailable"))?
                .saturating_add(*delta)
                .clamp(0, max),
            Self::Cycle(values) => {
                let index = current.and_then(|v| values.iter().position(|value| *value == v));
                values[index.map_or(0, |i| (i + 1) % values.len())]
            }
        };
        ensure!(
            allowed.is_none_or(|a| a.contains(&next)),
            "unsupported action value: {next}"
        );
        Ok(next)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Action {
    pub selector: Selector,
    pub control: Control,
    pub operation: Operation,
}

impl Default for Action {
    fn default() -> Self {
        Self {
            selector: Selector::All,
            control: Control::Brightness,
            operation: Operation::Offset(10),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Shortcut {
    pub name: String,
    pub hotkey: Option<Hotkey>,
    pub actions: Vec<Action>,
}

/// A validated operation for a worker. The worker evaluates it after previous
/// writes complete, so relative actions and cycles also compose with raw VCP.
/// Only the worker maps normalized brightness to hardware percent.
#[derive(Clone, Debug)]
pub struct Write {
    pub calibration: Vec<crate::levels::CalibrationPoint>,
    pub sdr_primary: bool,
    pub control: Control,
    pub operation: Operation,
    pub min: u8,
    pub max: u8,
    pub allowed: Option<Vec<i32>>,
}

/// Retain exact slider values across ordered actions despite rounding in the
/// hardware range. A fresh hardware read invalidates the worker's exact value.
pub fn brightness_value(
    operation: &Operation,
    hardware: Option<u8>,
    min: u8,
    max: u8,
    previous: Option<(u8, u8, u8)>,
) -> Result<(u8, u8)> {
    let current = previous
        .filter(|(a, b, _)| *a == min && *b == max)
        .map(|(_, _, value)| value)
        .or_else(|| hardware.map(|v| crate::levels::from_hardware(v, min, max)));
    let slider = operation.evaluate(current.map(i32::from), 100, None)? as u8;
    Ok((slider, crate::levels::to_hardware(slider, min, max)))
}

impl Action {
    pub fn validate(&self) -> Result<()> {
        self.selector.validate()?;
        let (max, allowed): (i32, Option<&[i32]>) = match self.control {
            Control::Brightness | Control::Sdr | Control::Contrast => (100, None),
            Control::Input | Control::Vcp(0x60) => (255, Some(&INPUT_VALUES)),
            Control::Power | Control::Vcp(0xd6) => (5, Some(&[1, 2, 3, 4, 5])),
            Control::Vcp(0) => bail!("VCP code must be nonzero"),
            Control::Vcp(_) => (65535, None),
        };
        self.operation.validate(max, allowed)
    }

    /// Unsupported/hidden/unavailable displays are skipped without state edits.
    pub fn plan(&self, row: &MonitorRow) -> Result<Option<Write>> {
        self.validate()?;
        let m = &row.monitor;
        if row.settings.hidden || m.unresponsive {
            return Ok(None);
        }
        let mut control = self.control.clone();
        if let Control::Vcp(code) = control {
            if code == 0x60 {
                control = Control::Input;
            }
            if code == 0xd6 {
                control = Control::Power;
            }
        }
        let supported = match control {
            Control::Brightness => row.slider.is_some(),
            Control::Sdr => m.sdr.is_some(),
            Control::Contrast => row.contrast().is_some(),
            Control::Input => {
                matches!(m.backend, crate::monitors::BrightnessBackend::Ddc { .. })
                    || !m.inputs.is_empty()
            }
            Control::Power => {
                m.supports_ddc_power
                    || matches!(m.backend, crate::monitors::BrightnessBackend::Ddc { .. })
            }
            Control::Vcp(code) => {
                m.vcp_codes.contains(&code)
                    || row.settings.feature_codes().contains(&code)
                    || matches!(m.backend, crate::monitors::BrightnessBackend::Ddc { .. })
            }
        };
        if !supported {
            return Ok(None);
        }
        let allowed = match control {
            Control::Input => Some(INPUT_VALUES.to_vec()),
            Control::Power => Some(vec![1, 2, 3, 4, 5]),
            _ => None,
        };
        if let Some(allowed) = &allowed {
            // A structurally valid value unsupported on this display skips
            // only that display, rather than cancelling other selected rows.
            if allowed.is_empty()
                || self
                    .operation
                    .validate(
                        if control == Control::Input { 255 } else { 5 },
                        Some(allowed),
                    )
                    .is_err()
            {
                return Ok(None);
            }
        }
        Ok(Some(Write {
            calibration: row.settings.calibration.clone(),
            sdr_primary: row.settings.sdr_primary,
            control,
            operation: self.operation.clone(),
            min: row.settings.min,
            max: row.settings.max,
            allowed,
        }))
    }
}

// Vendors use source codes beyond the named MCCS sources. Permit a nonzero
// byte structurally, then restrict it to the monitor's advertised input list.
const INPUT_VALUES: [i32; 255] = {
    let mut values = [0; 255];
    let mut index = 0;
    while index < values.len() {
        values[index] = index as i32 + 1;
        index += 1;
    }
    values
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        monitors::{BrightnessBackend, Monitor, MonitorKind, SdrLevel},
        settings::MonitorSettings,
    };

    fn row() -> MonitorRow {
        MonitorRow {
            monitor: Monitor {
                vcp_values: Default::default(),
                id: "desk".into(),
                model: "TEST".into(),
                device_path: String::new(),
                name: "Desk".into(),
                kind: MonitorKind::External,
                num: 1,
                bounds: None,
                backend: BrightnessBackend::Ddc { vcp: 0x10 },
                brightness: Some(50),
                sdr: None,
                hdr_supported: true,
                contrast: Some(60),
                supports_ddc_power: true,
                inputs: vec![17, 18, 19],
                vcp_codes: vec![0x10, 0x12, 0x60, 0xd6],
                input: Some(17),
                powered_off: false,
                unresponsive: false,
            },
            settings: MonitorSettings::default(),
            name: "Desk".into(),
            slider: Some(50),
        }
    }

    #[test]
    fn selection_support_and_opt_in_are_shared_by_every_caller() {
        let mut row = row();
        let mut action = Action::default();
        assert!(action.plan(&row).unwrap().is_some());
        row.settings.hidden = true;
        assert!(action.plan(&row).unwrap().is_none());
        row.settings.hidden = false;
        row.monitor.unresponsive = true;
        assert!(action.plan(&row).unwrap().is_none());
        row.monitor.unresponsive = false;
        row.slider = None;
        assert!(action.plan(&row).unwrap().is_none());
        row.monitor.sdr = Some(SdrLevel { percent: 40 });
        action.control = Control::Sdr;
        assert!(action.plan(&row).unwrap().is_some());
        action.control = Control::Contrast;
        assert!(action.plan(&row).unwrap().is_none());
        row.settings.show_contrast = true;
        assert!(action.plan(&row).unwrap().is_some());
        action.control = Control::Input;
        action.operation = Operation::Set(19);
        // Script actions explicitly address hardware, even if the input chip
        // is hidden or the display omits the source from its capability list.
        assert!(action.plan(&row).unwrap().is_some());
        row.settings.hidden_inputs = vec![19];
        assert!(action.plan(&row).unwrap().is_some());
        action.control = Control::Vcp(0x60);
        assert!(action.plan(&row).unwrap().is_some());
        action.control = Control::Vcp(0x13);
        assert!(action.plan(&row).unwrap().is_some());
        action.control = Control::Vcp(0x10);
        assert!(action.plan(&row).unwrap().is_some());
        action.control = Control::Power;
        action.operation = Operation::Set(1);
        row.monitor.powered_off = true;
        assert!(action.plan(&row).unwrap().is_some());
        row.monitor.supports_ddc_power = false;
        // The user can explicitly try power before capabilities finish loading.
        assert!(action.plan(&row).unwrap().is_some());
        row.monitor.backend = BrightnessBackend::Unsupported;
        assert!(action.plan(&row).unwrap().is_none());
    }

    #[test]
    fn normalized_cycles_survive_hardware_rounding_and_failed_actions() {
        let (slider, hardware) =
            brightness_value(&Operation::Set(1), Some(50), 20, 80, None).unwrap();
        assert_eq!((slider, hardware), (1, 21));
        let exact = Some((20, 80, slider));
        assert!(
            brightness_value(
                &Operation::Cycle(vec![1, 101]),
                Some(hardware),
                20,
                80,
                exact
            )
            .is_err()
        );
        assert_eq!(
            brightness_value(
                &Operation::Cycle(vec![1, 2, 3]),
                Some(hardware),
                20,
                80,
                exact
            )
            .unwrap(),
            (2, 21)
        );
        assert_eq!(
            brightness_value(&Operation::Offset(1), Some(hardware), 20, 80, exact).unwrap(),
            (2, 21)
        );
        // A new range must discard the previous normalized value.
        assert_eq!(
            brightness_value(&Operation::Offset(1), Some(21), 0, 100, exact).unwrap(),
            (22, 22)
        );
    }
    #[test]
    fn cycle_is_deterministic_and_invalid_values_never_advance() {
        let cycle = Operation::Cycle(vec![10, 40, 90]);
        for (current, next) in [(None, 10), (Some(10), 40), (Some(90), 10), (Some(30), 10)] {
            assert_eq!(cycle.evaluate(current, 100, None).unwrap(), next);
        }
        for values in [vec![], vec![10, 101], vec![10, 10]] {
            assert!(
                Operation::Cycle(values)
                    .evaluate(Some(10), 100, None)
                    .is_err()
            );
        }
        assert_eq!(cycle.evaluate(Some(10), 100, None).unwrap(), 40);
    }
    #[test]
    fn offsets_saturate_and_discrete_controls_reject_invalid_results() {
        assert_eq!(
            Operation::Offset(i32::MAX)
                .evaluate(Some(50), 100, None)
                .unwrap(),
            100
        );
        assert_eq!(
            Operation::Offset(i32::MIN)
                .evaluate(Some(50), 100, None)
                .unwrap(),
            0
        );
        assert!(Operation::Offset(1).evaluate(None, 100, None).is_err());
        assert!(
            Operation::Offset(1)
                .evaluate(Some(18), 255, Some(&[17, 18]))
                .is_err()
        );
        for (control, value) in [
            (Control::Vcp(0), 1),
            (Control::Vcp(1), 65536),
            (Control::Input, 0),
            (Control::Power, 6),
        ] {
            assert!(
                Action {
                    control,
                    operation: Operation::Set(value),
                    ..Action::default()
                }
                .validate()
                .is_err()
            );
        }
    }
}
