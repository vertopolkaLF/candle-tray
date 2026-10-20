//! One-time import of Twinkle Tray JSON, without opening legacy files for writing.
//!
//! Identity mapping (legacy source evidence):
//! - `names`: full `monitor.id` (SettingsWindow.jsx:348,582).
//! - `hideDisplays`: `monitor.key` = `hwid[2]` (SettingsWindow.jsx:1109;
//!   Monitors.js:1289-1293). Trim the first `_` suffix for Candle's MonitorId.
//! - `remaps`: full id OR hardware friendly name (electron.js:2141-2153).
//!   Full-id ranges override name ranges. Name ranges remain pending and resolve
//!   only against a unique hardware name on the first monitor snapshot.
//! - `order`: full ids, either strings or pre-1.18 `{id, order}` records sorted
//!   numerically and deduplicated (electron.js:985-1005,2124-2131).
//! - `known-displays.json`: dictionary keyed by instance, containing full monitor
//!   records (electron.js:1720-1741). `brightness` is already slider-normalized
//!   (electron.js:2792,1784-1786), so copy it without range conversion.
//! - `linkedLevelsActive`, `openAtLogin`, `updateInterval` map directly; interval
//!   999 is legacy's sentinel for 100 ms (electron.js:1027).
//! - theme light/dark/default -> Light/Dark/Auto; language en/ru (including
//!   regional tags) -> En/Ru, all other languages -> System.
//! - sleepAction ps/ddcci/ps_ddcci -> Windows/Ddc/Both (electron.js:3516-3530).
//!
//! Discovery checks roaming app-name directories including appx/arm64 variants
//! (package.json:2,14-19), normal and development JSON names (electron.js:23-25).
//! Portable installations use an arbitrary adjacent `config/`; pass that file
//! explicitly to `load`, since its location cannot be inferred from APPDATA.
//! Calibration curves, gamma/software brightness and SDR-as-main-slider have no
//! MVP equivalent; warnings report these lossy conversions. No import is applied
//! automatically. Retain PendingImport until `resolve` has no unresolved entries;
//! it can be serialized alongside onboarding state if hardware is unavailable.
use crate::{
    monitors::Monitor,
    settings::{Language, Settings, Theme, TurnOffSetting},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PendingImport {
    settings: Value,
    known: Value,
    warnings: Vec<String>,
}
/// Counts refer to this application, not a delta from a previous resolve call.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub monitors: usize,
    pub fields: usize,
    pub unresolved: usize,
    pub skipped: usize,
    pub warnings: Vec<String>,
}
pub fn find() -> Option<PathBuf> {
    let base = directories::BaseDirs::new()?;
    for directory in [
        "twinkle-tray",
        "twinkle-tray-appx",
        "twinkle-tray-arm64",
        "twinkle-tray-portable",
    ] {
        for file in ["settings.json", "settings-dev.json"] {
            let path = base.config_dir().join(directory).join(file);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}
pub fn load(path: &Path) -> Result<PendingImport> {
    let settings: Value = serde_json::from_str(
        &fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?,
    )?;
    ensure!(
        settings.is_object(),
        "Twinkle Tray settings must be an object"
    );
    let known_path = path.with_file_name(
        if path
            .file_name()
            .is_some_and(|name| name == "settings-dev.json")
        {
            "known-displays-dev.json"
        } else {
            "known-displays.json"
        },
    );
    let mut warnings = Vec::new();
    let known = match fs::read_to_string(&known_path) {
        Ok(raw) => match serde_json::from_str::<Value>(&raw) {
            Ok(value) if value.is_object() => value,
            _ => {
                warnings.push("Invalid known-displays JSON; brightness was not imported".into());
                Value::Null
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Value::Null,
        Err(error) => {
            warnings.push(format!("Cannot read known-displays: {error}"));
            Value::Null
        }
    };
    if settings
        .get("remaps")
        .and_then(Value::as_object)
        .is_some_and(|map| {
            map.values().any(|v| {
                v.get("calibration")
                    .and_then(Value::as_array)
                    .is_some_and(|a| !a.is_empty())
            })
        })
    {
        warnings
            .push("Calibration curves are unsupported; only range endpoints are imported".into());
    }
    if settings.get("sdrAsMainSlider").and_then(Value::as_bool) == Some(true)
        || ["sdrAsMainSliderDisplays", "gammaAsMainSliderDisplays"]
            .iter()
            .any(|key| {
                settings
                    .get(key)
                    .and_then(Value::as_object)
                    .is_some_and(|map| map.values().any(|v| v.as_bool() == Some(true)))
            })
    {
        warnings.push(
            "SDR/gamma main-slider modes are unsupported; saved brightness may need adjustment"
                .into(),
        );
    }
    // Retain only MVP import fields, never unrelated legacy profiles or keys.
    let settings = Value::Object(
        settings
            .as_object()
            .expect("validated object")
            .iter()
            .filter(|(key, _)| {
                matches!(
                    key.as_str(),
                    "names"
                        | "hideDisplays"
                        | "remaps"
                        | "order"
                        | "linkedLevelsActive"
                        | "theme"
                        | "language"
                        | "openAtLogin"
                        | "updateInterval"
                        | "sleepAction"
                )
            })
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    );
    let known = known
        .as_object()
        .map(|records| {
            Value::Object(
                records
                    .iter()
                    .map(|(key, value)| {
                        let record = value
                            .as_object()
                            .map(|record| {
                                Value::Object(
                                    record
                                        .iter()
                                        .filter(|(key, _)| {
                                            matches!(key.as_str(), "id" | "key" | "brightness")
                                        })
                                        .map(|(key, value)| (key.clone(), value.clone()))
                                        .collect(),
                                )
                            })
                            .unwrap_or(Value::Null);
                        (key.clone(), record)
                    })
                    .collect(),
            )
        })
        .unwrap_or(Value::Null);
    Ok(PendingImport {
        settings,
        known,
        warnings,
    })
}
/// Extract only device-path ids. A plain display name is never guessed to be an id.
fn path_id(value: &str) -> Option<String> {
    let parts = value.split('#').collect::<Vec<_>>();
    if parts.len() < 3 || !parts[0].to_ascii_uppercase().ends_with("DISPLAY") {
        return None;
    }
    instance(parts[2])
}
fn instance(value: &str) -> Option<String> {
    let key = value.split('_').next()?.trim();
    (!key.is_empty()).then(|| key.to_owned())
}
fn percent(value: &Value) -> Option<u8> {
    let number = value.as_f64()?;
    (number.is_finite() && (0.0..=100.0).contains(&number)).then(|| number.round() as u8)
}
fn entries<'a>(value: &'a Value, key: &str) -> impl Iterator<Item = (&'a String, &'a Value)> {
    value
        .get(key)
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
}
impl PendingImport {
    /// Applies globals and convertible identities; unresolved names stay in self.
    pub fn apply(&self, settings: &mut Settings) -> ImportReport {
        self.resolve(&[], settings)
    }
    /// Reapply on the first snapshot. Direct identity mappings take precedence
    /// over name-based ranges, regardless of JSON key ordering. Ambiguous names
    /// are left pending rather than accidentally targeting duplicate monitors.
    pub fn resolve(&self, monitors: &[Monitor], settings: &mut Settings) -> ImportReport {
        let mut report = ImportReport {
            warnings: self.warnings.clone(),
            ..Default::default()
        };
        let mut touched = BTreeSet::new();
        let raw = &self.settings;
        for (key, target) in [
            ("linkedLevelsActive", &mut settings.link_levels),
            ("openAtLogin", &mut settings.launch_at_startup),
        ] {
            if let Some(value) = raw.get(key).and_then(Value::as_bool) {
                *target = value;
                report.fields += 1;
            }
        }
        if let Some(value) = raw.get("theme").and_then(Value::as_str) {
            settings.theme = match value {
                "light" => Theme::Light,
                "dark" => Theme::Dark,
                _ => Theme::Auto,
            };
            report.fields += 1;
        }
        if let Some(value) = raw.get("language").and_then(Value::as_str) {
            settings.language = match value.to_ascii_lowercase().split(['-', '_']).next() {
                Some("en") => Language::En,
                Some("ru") => Language::Ru,
                _ => Language::System,
            };
            report.fields += 1;
        }
        if let Some(value) = raw
            .get("updateInterval")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n > 0)
        {
            settings.update_interval_ms = if value == 999 { 100 } else { value };
            report.fields += 1;
        }
        if let Some(value) = raw.get("sleepAction").and_then(Value::as_str) {
            if let Some(mode) = match value {
                "ps" => Some(TurnOffSetting::Windows),
                "ddcci" => Some(TurnOffSetting::Ddc),
                "ps_ddcci" => Some(TurnOffSetting::Both),
                _ => None,
            } {
                settings.turn_off = mode;
                report.fields += 1;
            } else {
                report.skipped += 1;
            }
        }
        for (key, value) in entries(raw, "names") {
            if let (Some(id), Some(name)) = (path_id(key), value.as_str()) {
                settings.monitors.entry(id.clone()).or_default().name =
                    (!name.is_empty()).then(|| name.to_owned());
                touched.insert(id);
                report.fields += 1;
            } else {
                report.skipped += 1;
            }
        }
        for (key, value) in entries(raw, "hideDisplays") {
            if let (Some(id), Some(hidden)) = (instance(key), value.as_bool()) {
                settings.monitors.entry(id.clone()).or_default().hidden = hidden;
                touched.insert(id);
                report.fields += 1;
            } else {
                report.skipped += 1;
            }
        }
        // Legacy name matches first, then the new full-id scheme overrides them.
        for direct in [false, true] {
            for (key, value) in entries(raw, "remaps") {
                let id = path_id(key);
                if id.is_some() != direct {
                    continue;
                }
                let id = id.or_else(|| {
                    let mut matches = monitors.iter().filter(|monitor| monitor.name == *key);
                    let first = matches.next()?;
                    matches.next().is_none().then(|| first.id.clone())
                });
                let (Some(min), Some(max)) = (
                    value.get("min").and_then(percent),
                    value.get("max").and_then(percent),
                ) else {
                    report.skipped += 1;
                    continue;
                };
                if min > max {
                    report.skipped += 1;
                    continue;
                }
                if let Some(id) = id {
                    let target = settings.monitors.entry(id.clone()).or_default();
                    target.min = min;
                    target.max = max;
                    touched.insert(id);
                    report.fields += 2;
                } else {
                    report.unresolved += 1;
                }
            }
        }
        if let Some(order) = raw.get("order").and_then(Value::as_array) {
            let mut order = order
                .iter()
                .filter_map(|entry| {
                    if let Some(id) = entry.as_str() {
                        Some((id, 0.0))
                    } else {
                        let position = entry
                            .get("order")
                            .and_then(|v| v.as_f64().or_else(|| v.as_str()?.parse().ok()))
                            .unwrap_or(0.0);
                        Some((entry.get("id")?.as_str()?, position))
                    }
                })
                .collect::<Vec<_>>();
            order.sort_by(|a, b| a.1.total_cmp(&b.1));
            settings.order.clear();
            for (id, _) in order {
                if let Some(id) = path_id(id) {
                    if !settings.order.contains(&id) {
                        settings.order.push(id.clone());
                        touched.insert(id);
                        report.fields += 1;
                    }
                } else {
                    report.skipped += 1;
                }
            }
        }
        if let Some(known) = self.known.as_object() {
            for (key, value) in known {
                let id = value
                    .get("id")
                    .and_then(Value::as_str)
                    .and_then(path_id)
                    .or_else(|| value.get("key").and_then(Value::as_str).and_then(instance))
                    .or_else(|| instance(key));
                if let (Some(id), Some(brightness)) =
                    (id, value.get("brightness").and_then(percent))
                {
                    settings
                        .monitors
                        .entry(id.clone())
                        .or_default()
                        .last_brightness = Some(brightness);
                    touched.insert(id);
                    report.fields += 1;
                } else {
                    report.skipped += 1;
                }
            }
        }
        settings.twinkle_import_done = true;
        report.monitors = touched.len();
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitors::{BrightnessBackend, MonitorKind};
    fn fixture(raw: &str, known: Option<&str>) -> PendingImport {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, raw).unwrap();
        if let Some(known) = known {
            fs::write(dir.path().join("known-displays.json"), known).unwrap();
        }
        load(&path).unwrap()
    }
    fn display(id: &str, name: &str) -> Monitor {
        Monitor {
            id: id.into(),
            name: name.into(),
            model: "DEL123".into(),
            device_path: format!("DISPLAY#DEL123#{id}"),
            kind: MonitorKind::External,
            num: 1,
            bounds: None,
            backend: BrightnessBackend::Ddc { vcp: 0x10 },
            brightness: Some(50),
            sdr: None,
            supports_ddc_power: false,
            powered_off: false,
            unresponsive: false,
        }
    }
    #[test]
    fn current_format_converts_each_distinct_identity_scheme() {
        let import = fixture(
            r##"{
            "names":{"\\\\?\\DISPLAY#DEL123#5&abc&0&UID1":"Desk"},
            "hideDisplays":{"5&abc&0&UID1_0":true},
            "remaps":{"DISPLAY#DEL123#5&abc&0&UID1":{"min":20,"max":80}},
            "order":["DISPLAY#DEL123#5&abc&0&UID2", "DISPLAY#DEL123#5&abc&0&UID1", "DISPLAY#DEL123#5&abc&0&UID1"],
            "linkedLevelsActive":true,"theme":"dark","language":"ru","openAtLogin":false,
            "updateInterval":999,"sleepAction":"ps_ddcci"
        }"##,
            Some(r#"{"5&abc&0&UID1":{"id":"DISPLAY#DEL123#5&abc&0&UID1","brightness":37.5}}"#),
        );
        let mut settings = Settings::default();
        let report = import.apply(&mut settings);
        assert_eq!(report.monitors, 2);
        assert_eq!(report.unresolved, 0);
        assert_eq!(settings.order, ["5&abc&0&UID2", "5&abc&0&UID1"]);
        let monitor = &settings.monitors["5&abc&0&UID1"];
        assert_eq!(monitor.name.as_deref(), Some("Desk"));
        assert!(monitor.hidden);
        assert_eq!((monitor.min, monitor.max), (20, 80));
        assert_eq!(monitor.last_brightness, Some(38));
        assert!(settings.link_levels);
        assert!(!settings.launch_at_startup);
        assert_eq!(settings.language, Language::Ru);
        assert_eq!(settings.theme, Theme::Dark);
        assert_eq!(settings.turn_off, TurnOffSetting::Both);
        assert_eq!(settings.update_interval_ms, 100);
    }
    #[test]
    fn legacy_order_is_sorted_and_name_ranges_wait_for_snapshot() {
        let import = fixture(
            r#"{"order":[{"id":"DISPLAY#DEL#b_0","order":"2"},{"id":"DISPLAY#DEL#a","order":0},{"id":"DISPLAY#DEL#a","order":1}],"remaps":{"Desk":{"min":10,"max":90},"DISPLAY#DEL#a":{"min":20,"max":70}}}"#,
            None,
        );
        let mut settings = Settings::default();
        assert_eq!(import.apply(&mut settings).unresolved, 1);
        assert_eq!(settings.order, ["a", "b"]);
        let monitors = [display("a", "Desk"), display("b", "Other")];
        assert_eq!(import.resolve(&monitors, &mut settings).unresolved, 0);
        assert_eq!(
            (settings.monitors["a"].min, settings.monitors["a"].max),
            (20, 70)
        );
        let restored: PendingImport =
            serde_json::from_str(&serde_json::to_string(&import).unwrap()).unwrap();
        assert_eq!(restored.resolve(&monitors, &mut settings).unresolved, 0);
    }
    #[test]
    fn ambiguous_name_stays_pending_and_invalid_ranges_are_skipped() {
        let import = fixture(
            r#"{"remaps":{"Desk":{"min":0,"max":50},"Invalid":{"min":90,"max":10}},"language":"fr","sleepAction":"unknown"}"#,
            None,
        );
        let mut settings = Settings::default();
        let report = import.resolve(&[display("a", "Desk"), display("b", "Desk")], &mut settings);
        assert_eq!(report.unresolved, 1);
        assert_eq!(report.skipped, 2);
        assert!(settings.monitors.is_empty());
        assert_eq!(settings.language, Language::System);
    }
    #[test]
    fn known_file_corruption_does_not_discard_settings_and_dev_file_is_used() {
        let import = fixture(r#"{"theme":"light"}"#, Some("broken"));
        let mut settings = Settings::default();
        assert_eq!(import.apply(&mut settings).warnings.len(), 1);
        assert_eq!(settings.theme, Theme::Light);
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("settings-dev.json"), "{}").unwrap();
        fs::write(
            dir.path().join("known-displays-dev.json"),
            r#"{"instance_0":{"brightness":42}}"#,
        )
        .unwrap();
        load(&dir.path().join("settings-dev.json"))
            .unwrap()
            .apply(&mut settings);
        assert_eq!(settings.monitors["instance"].last_brightness, Some(42));
        fs::write(dir.path().join("settings-dev.json"), "[]").unwrap();
        assert!(load(&dir.path().join("settings-dev.json")).is_err());
    }
}
