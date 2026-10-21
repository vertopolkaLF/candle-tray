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
//! - Global brightness/off hotkeys map to the three shortcut slots; duplicate
//!   slots/accelerators and unsupported actions are skipped. hotkeyPercent wins
//!   over action offsets; without it, the first imported offset sets the step
//!   and incompatible later offsets are skipped. Old monitor/direction records
//!   follow the legacy v1.16 upgrade (electron.js:1072-1102).
//! - Idle dimming targets zero (electron.js:5534-5540). Clock, sunrise and sunset
//!   schedules accept only a single shared level; per-display differences cannot
//!   be represented without inventing profiles. Legacy animation is approximated
//!   by fixed fade presets, since Candle does not interpolate between events.
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
    hotkey::Hotkey,
    monitors::Monitor,
    settings::{
        Language, ScheduleEntry, ScheduleTarget, ScheduleTime, Settings, Theme, TurnOffSetting,
    },
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
    pub imported_shortcuts: usize,
    pub skipped_shortcuts: usize,
    pub imported_schedule_entries: usize,
    pub skipped_schedule_entries: usize,
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
    // Retain only supported import fields, never unrelated legacy profiles or keys.
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
                        | "hotkeyPercent"
                        | "hotkeys"
                        | "scrollShortcut"
                        | "detectIdleTimeEnabled"
                        | "detectIdleTimeMinutes"
                        | "detectIdleTimeSeconds"
                        | "detectIdleMedia"
                        | "detectIdleCheckFullscreen"
                        | "adjustmentTimes"
                        | "adjustmentTimeIndividualDisplays"
                        | "adjustmentTimeLatitude"
                        | "adjustmentTimeLongitude"
                        | "adjustmentTimeSpeed"
                        | "adjustmentTimeAnimate"
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
/// Legacy numeric inputs can be JSON numbers or strings from HTML inputs.
fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.parse().ok())
        .filter(|n| n.is_finite())
}
fn integer(value: &Value) -> Option<i64> {
    let n = number(value)?;
    (n.fract() == 0.0 && (i32::MIN as f64..=i32::MAX as f64).contains(&n)).then_some(n as i64)
}

/// Electron accelerator names that have an unambiguous Windows equivalent.
fn accelerator(value: &str) -> Option<Hotkey> {
    let mut hotkey = Hotkey {
        ctrl: false,
        alt: false,
        shift: false,
        win: false,
        key: 0,
    };
    for part in value.split('+') {
        let part = part.trim().to_ascii_lowercase();
        match part.as_str() {
            "ctrl" | "control" | "cmdorctrl" | "commandorcontrol" => hotkey.ctrl = true,
            "alt" | "option" => hotkey.alt = true,
            "shift" => hotkey.shift = true,
            "super" | "meta" | "win" => hotkey.win = true,
            key => {
                if hotkey.key != 0 {
                    return None;
                }
                hotkey.key = match key {
                    "up" => 0x26,
                    "down" => 0x28,
                    "left" => 0x25,
                    "right" => 0x27,
                    "home" => 0x24,
                    "end" => 0x23,
                    "pageup" => 0x21,
                    "pagedown" => 0x22,
                    "plus" => 0xBB,
                    "minus" | "-" => 0xBD,
                    "space" => 0x20,
                    "tab" => 0x09,
                    "enter" | "return" => 0x0D,
                    "escape" | "esc" => 0x1B,
                    "backspace" => 0x08,
                    "insert" => 0x2D,
                    "delete" => 0x2E,
                    "numdec" => 0x6E,
                    "numadd" => 0x6B,
                    "numsub" => 0x6D,
                    "nummult" => 0x6A,
                    "numdiv" => 0x6F,
                    "numlock" => 0x90,
                    "scrolllock" => 0x91,
                    "printscreen" => 0x2C,
                    key if key.len() == 1 && key.as_bytes()[0].is_ascii_alphanumeric() => {
                        u16::from(key.as_bytes()[0].to_ascii_uppercase())
                    }
                    key if key.starts_with('f') => {
                        let n: u16 = key[1..].parse().ok()?;
                        if !(1..=24).contains(&n) {
                            return None;
                        }
                        0x6F + n
                    }
                    key if key.starts_with("num") => {
                        let n: u16 = key[3..].parse().ok()?;
                        if n > 9 {
                            return None;
                        }
                        0x60 + n
                    }
                    _ => return None,
                };
            }
        }
    }
    hotkey.is_valid().then_some(hotkey)
}

/// Slot 0/1/2 is brightness up/down/off. The old dictionary used monitor/direction.
fn shortcut_action(value: &Value, step: u8) -> Option<(usize, Option<u8>)> {
    let offset = if let Some(actions) = value.get("actions") {
        let actions = actions.as_array()?;
        if actions.len() != 1 {
            return None;
        }
        let action = &actions[0];
        match action.get("type")?.as_str()? {
            "off" => return Some((2, None)),
            "offset"
                if action.get("target")?.as_str()? == "brightness"
                    && action.get("allMonitors")?.as_bool()? =>
            {
                integer(action.get("value")?)?
            }
            _ => return None,
        }
    } else {
        match value.get("monitor")?.as_str()? {
            "turn_off_displays" => return Some((2, None)),
            "all" => integer(value.get("direction")?)? * i64::from(step),
            _ => return None,
        }
    };
    if offset == 0 || offset.unsigned_abs() > 100 {
        return None;
    }
    Some((usize::from(offset < 0), Some(offset.unsigned_abs() as u8)))
}

fn import_shortcuts(raw: &Value, settings: &mut Settings, report: &mut ImportReport) {
    let explicit_step = raw
        .get("hotkeyPercent")
        .and_then(number)
        .map(|n| n.round().clamp(1.0, 100.0) as u8);
    if let Some(step) = explicit_step {
        settings.shortcut_step = step;
        report.fields += 1;
    }
    let Some(hotkeys) = raw.get("hotkeys") else {
        return;
    };
    let values: Vec<_> = match hotkeys {
        Value::Array(values) => values.iter().collect(),
        Value::Object(values) => values.values().collect(),
        _ => vec![hotkeys],
    };
    let candidates: Vec<_> = values
        .iter()
        .map(|value| {
            let hotkey = accelerator(value.get("accelerator")?.as_str()?)?;
            let (slot, step) = shortcut_action(value, explicit_step.unwrap_or(10))?;
            Some((hotkey, slot, step))
        })
        .collect();
    let mut inferred_step = None;
    for candidate in &candidates {
        let Some((hotkey, slot, step)) = candidate else {
            report.skipped_shortcuts += 1;
            continue;
        };
        // Candle stores one binding per action and RegisterHotKey claims a combo once.
        if candidates
            .iter()
            .flatten()
            .filter(|(key, action, _)| key == hotkey || action == slot)
            .count()
            != 1
        {
            report.skipped_shortcuts += 1;
            continue;
        }
        if explicit_step.is_none()
            && let Some(step) = step
        {
            if inferred_step.is_some_and(|previous| previous != *step) {
                report.skipped_shortcuts += 1;
                continue;
            }
            inferred_step = Some(*step);
            settings.shortcut_step = *step;
        }
        if step.is_some_and(|step| step != settings.shortcut_step) {
            report
                .warnings
                .push("Shortcut offset uses the shared hotkeyPercent step".into());
        }
        match slot {
            0 => settings.shortcuts.brightness_up = Some(*hotkey),
            1 => settings.shortcuts.brightness_down = Some(*hotkey),
            _ => settings.shortcuts.turn_off = Some(*hotkey),
        }
        report.imported_shortcuts += 1;
        report.fields += 1;
    }
    report.skipped += report.skipped_shortcuts;
}

fn schedule_time(entry: &Value) -> Option<ScheduleTime> {
    if entry
        .get("useSunCalc")
        .map(Value::as_bool)
        .unwrap_or(Some(false))?
    {
        let offset = entry.get("offset").map(integer).unwrap_or(Some(0))?;
        let offset = i16::try_from(offset)
            .ok()
            .filter(|n| (-1440..=1440).contains(n))?;
        return match entry.get("sunCalc")?.as_str()? {
            "sunrise" => Some(ScheduleTime::Sunrise { offset }),
            "sunset" => Some(ScheduleTime::Sunset { offset }),
            _ => None,
        };
    }
    let (hour, minute) = if let Some(time) = entry.get("time") {
        let (hour, minute) = time.as_str()?.split_once(':')?;
        (hour.parse::<i64>().ok()?, minute.parse::<i64>().ok()?)
    } else {
        // Pre-1.15 schedules used a 12-hour clock (Utils.upgradeAdjustmentTimes).
        let hour = integer(entry.get("hour")?)?;
        if !(1..=12).contains(&hour) {
            return None;
        }
        let hour = hour % 12
            + match entry.get("am")?.as_str()?.to_ascii_lowercase().as_str() {
                "am" => 0,
                "pm" => 12,
                _ => return None,
            };
        (hour, integer(entry.get("minute")?)?)
    };
    ((0..24).contains(&hour) && (0..60).contains(&minute)).then(|| ScheduleTime::Clock {
        minutes: (hour * 60 + minute) as u16,
    })
}
fn schedule_entry(entry: &Value, individual: bool) -> Option<ScheduleEntry> {
    let time = schedule_time(entry)?;
    let level = if individual {
        let monitors = entry.get("monitors")?.as_object()?;
        let mut levels = monitors.values().map(number);
        let first = levels.next()??;
        if !levels.all(|level| level == Some(first)) {
            return None;
        }
        percent(&Value::from(first))?
    } else {
        percent(&Value::from(number(entry.get("brightness")?)?))?
    };
    Some(ScheduleEntry {
        time,
        target: ScheduleTarget::Level { level },
    })
}
fn import_automation(raw: &Value, settings: &mut Settings, report: &mut ImportReport) {
    for (key, target) in [
        ("scrollShortcut", &mut settings.tray_scroll),
        ("detectIdleTimeEnabled", &mut settings.idle_dim.enabled),
    ] {
        if let Some(value) = raw.get(key).and_then(Value::as_bool) {
            *target = value;
            report.fields += 1;
        }
    }
    if [
        "detectIdleTimeEnabled",
        "detectIdleTimeMinutes",
        "detectIdleTimeSeconds",
    ]
    .iter()
    .any(|key| raw.get(key).is_some())
    {
        // electron.js dims both instant and transition paths to zero.
        settings.idle_dim.level = 0;
        report.fields += 1;
    }
    let minutes = raw
        .get("detectIdleTimeMinutes")
        .and_then(number)
        .filter(|n| *n >= 0.0);
    let seconds = raw
        .get("detectIdleTimeSeconds")
        .and_then(number)
        .filter(|n| *n >= 0.0);
    if minutes.is_some() || seconds.is_some() {
        settings.idle_dim.minutes = (minutes.unwrap_or(0.0) + seconds.unwrap_or(0.0) / 60.0)
            .ceil()
            .clamp(1.0, u16::MAX as f64) as u16;
        report.fields += 1;
    }
    let busy: Vec<_> = ["detectIdleMedia", "detectIdleCheckFullscreen"]
        .iter()
        .filter_map(|key| raw.get(key).and_then(Value::as_bool))
        .collect();
    if !busy.is_empty() {
        settings.idle_dim.respect_busy = busy.iter().any(|value| *value);
        report.fields += 1;
    }
    if let (Some(latitude), Some(longitude)) = (
        raw.get("adjustmentTimeLatitude").and_then(number),
        raw.get("adjustmentTimeLongitude").and_then(number),
    ) && (-90.0..=90.0).contains(&latitude)
        && (-180.0..=180.0).contains(&longitude)
        && (latitude != 0.0 || longitude != 0.0)
    {
        settings.schedule.latitude = Some(latitude);
        settings.schedule.longitude = Some(longitude);
        report.fields += 2;
    }
    if raw.get("adjustmentTimeAnimate").is_some() || raw.get("adjustmentTimeSpeed").is_some() {
        // Legacy animation interpolates between events, rather than using a fixed
        // duration. Preserve the speed order using Candle's available fade presets.
        settings.schedule.fade_seconds =
            if raw.get("adjustmentTimeAnimate").and_then(Value::as_bool) != Some(true) {
                0
            } else {
                match raw.get("adjustmentTimeSpeed").and_then(Value::as_str) {
                    Some("instant") => 0,
                    Some("fastest" | "faster" | "fast") => 30,
                    Some("slow") => 300,
                    Some("slowest") => 900,
                    _ => 60,
                }
            };
        report.fields += 1;
    }
    if let Some(entries) = raw.get("adjustmentTimes") {
        let individual = raw
            .get("adjustmentTimeIndividualDisplays")
            .and_then(Value::as_bool)
            == Some(true);
        let mut imported = Vec::new();
        if let Some(entries) = entries.as_array() {
            for entry in entries {
                if let Some(entry) = schedule_entry(entry, individual) {
                    imported.push(entry);
                } else {
                    report.skipped_schedule_entries += 1;
                }
            }
        } else {
            report.skipped_schedule_entries += 1;
        }
        report.imported_schedule_entries = imported.len();
        report.fields += imported.len();
        report.skipped += report.skipped_schedule_entries;
        if !imported.is_empty() {
            settings.schedule.entries = imported;
            settings.schedule.enabled = true;
        }
    }
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
        import_shortcuts(raw, settings, &mut report);
        import_automation(raw, settings, &mut report);
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
            contrast: None,
            hdr_supported: false,
            supports_ddc_power: false,
            inputs: Vec::new(),
            input: None,
            powered_off: false,
            unresponsive: false,
        }
    }
    #[test]
    fn electron_accelerators_map_to_windows_keys() {
        for (name, key) in [
            ("Up", 0x26),
            ("Down", 0x28),
            ("Left", 0x25),
            ("Right", 0x27),
            ("Home", 0x24),
            ("End", 0x23),
            ("PageUp", 0x21),
            ("PageDown", 0x22),
            ("Plus", 0xBB),
            ("Minus", 0xBD),
            ("A", 0x41),
            ("z", 0x5A),
            ("0", 0x30),
            ("9", 0x39),
            ("F1", 0x70),
            ("F24", 0x87),
            ("num0", 0x60),
            ("num9", 0x69),
            ("numadd", 0x6B),
            ("numsub", 0x6D),
            ("nummult", 0x6A),
            ("numdiv", 0x6F),
            ("numdec", 0x6E),
            ("Space", 0x20),
            ("Return", 0x0D),
            ("Delete", 0x2E),
        ] {
            let parsed = accelerator(&format!("Ctrl+Alt+{name}")).unwrap();
            assert_eq!(parsed.key, key, "{name}");
            assert!(parsed.ctrl && parsed.alt);
        }
        for modifier in ["Ctrl", "Control", "CmdOrCtrl", "CommandOrControl"] {
            assert!(accelerator(&format!("{modifier}+A")).unwrap().ctrl);
        }
        for modifier in ["Super", "Meta", "Win"] {
            assert!(accelerator(&format!("{modifier}+A")).unwrap().win);
        }
        let parsed = accelerator("Control+Option+Shift+Meta+F13").unwrap();
        assert!(parsed.ctrl && parsed.alt && parsed.shift && parsed.win);
        assert!(accelerator("F13").is_some());
        for invalid in [
            "",
            "Ctrl",
            "Ctrl+",
            "Ctrl++",
            "Ctrl+A+B",
            "Ctrl+F25",
            "Ctrl+num10",
            "Shift+A",
            "F1",
            "Command+A",
            "BrightnessUp",
            "Ctrl+Unknown",
        ] {
            assert!(accelerator(invalid).is_none(), "{invalid}");
        }
    }
    #[test]
    fn modern_shortcuts_and_explicit_step_are_imported() {
        let import = fixture(
            r#"{"hotkeyPercent":200,"hotkeys":[
            {"accelerator":"Ctrl+Up","actions":[{"type":"offset","target":"brightness","allMonitors":true,"value":5}]},
            {"accelerator":"Ctrl+Down","actions":[{"type":"offset","target":"brightness","allMonitors":true,"value":-5}]},
            {"accelerator":"Ctrl+End","actions":[{"type":"off"}]}
        ]}"#,
            None,
        );
        let mut settings = Settings::default();
        let report = import.apply(&mut settings);
        assert_eq!(settings.shortcut_step, 100);
        assert_eq!(settings.shortcuts.brightness_up, accelerator("Ctrl+Up"));
        assert_eq!(settings.shortcuts.brightness_down, accelerator("Ctrl+Down"));
        assert_eq!(settings.shortcuts.turn_off, accelerator("Ctrl+End"));
        assert_eq!(
            (report.imported_shortcuts, report.skipped_shortcuts),
            (3, 0)
        );
        assert_eq!(report.warnings.len(), 2);
        fixture(r#"{"hotkeyPercent":-10}"#, None).apply(&mut settings);
        assert_eq!(settings.shortcut_step, 1);
    }
    #[test]
    fn old_hotkey_dictionary_and_inferred_steps_are_supported() {
        let import = fixture(
            r#"{"hotkeyPercent":7,"hotkeys":{
            "up":{"accelerator":"Alt+Up","monitor":"all","direction":1},
            "down":{"accelerator":"Alt+Down","monitor":"all","direction":-1},
            "off":{"accelerator":"Alt+End","monitor":"turn_off_displays"},
            "display":{"accelerator":"Alt+A","monitor":"DISPLAY#one","direction":1}
        }}"#,
            None,
        );
        let mut settings = Settings::default();
        let report = import.apply(&mut settings);
        assert_eq!(
            (report.imported_shortcuts, report.skipped_shortcuts),
            (3, 1)
        );
        assert_eq!(settings.shortcut_step, 7);
        let import = fixture(
            r#"{"hotkeys":[
            {"accelerator":"Ctrl+Up","actions":[{"type":"offset","target":"brightness","allMonitors":true,"value":"17"}]},
            {"accelerator":"Ctrl+Down","actions":[{"type":"offset","target":"brightness","allMonitors":true,"value":-20}]}
        ]}"#,
            None,
        );
        let mut settings = Settings::default();
        let report = import.apply(&mut settings);
        assert_eq!(settings.shortcut_step, 17);
        assert_eq!(
            (report.imported_shortcuts, report.skipped_shortcuts),
            (1, 1)
        );
        assert!(settings.shortcuts.brightness_down.is_none());
    }
    #[test]
    fn ambiguous_and_unsupported_shortcuts_are_counted() {
        let import = fixture(
            r#"{"hotkeys":[
            {"accelerator":"Ctrl+A","actions":[{"type":"off"}]},
            {"accelerator":"Ctrl+B","actions":[{"type":"off"}]},
            {"accelerator":"Ctrl+C","actions":[{"type":"off"},{"type":"off"}]},
            {"accelerator":"Ctrl+D","actions":[{"type":"offset","target":"contrast","allMonitors":true,"value":5}]},
            {"accelerator":"Ctrl+E","actions":[{"type":"offset","target":"brightness","allMonitors":false,"value":5}]},
            {"accelerator":"BrightnessUp","actions":[{"type":"off"}]},
            {"accelerator":"Ctrl+F","actions":[{"type":"offset","target":"brightness","allMonitors":true,"value":0}]},
            {"accelerator":"Ctrl+G","actions":[{"type":"offset","target":"brightness","allMonitors":true,"value":101}]},
            {"accelerator":"Ctrl+H","actions":[{"type":"set"}]},
            null
        ]}"#,
            None,
        );
        let mut settings = Settings::default();
        let report = import.apply(&mut settings);
        assert_eq!(
            (
                report.imported_shortcuts,
                report.skipped_shortcuts,
                report.skipped
            ),
            (0, 10, 10)
        );
        assert_eq!(settings.shortcuts, Default::default());
        let report = fixture(r#"{"hotkeys":true}"#, None).apply(&mut settings);
        assert_eq!(report.skipped_shortcuts, 1);
    }
    #[test]
    fn duplicate_accelerators_do_not_claim_multiple_actions() {
        let import = fixture(
            r#"{"hotkeys":[
            {"accelerator":"Ctrl+A","actions":[{"type":"off"}]},
            {"accelerator":"Control+A","actions":[{"type":"offset","target":"brightness","allMonitors":true,"value":10}]}
        ]}"#,
            None,
        );
        let report = import.apply(&mut Settings::default());
        assert_eq!(
            (report.imported_shortcuts, report.skipped_shortcuts),
            (0, 2)
        );
    }
    #[test]
    fn idle_and_scroll_settings_apply_and_round_up() {
        let mut settings = Settings::default();
        fixture(r#"{"scrollShortcut":false,"detectIdleTimeEnabled":true,"detectIdleTimeMinutes":"2","detectIdleTimeSeconds":"1","detectIdleMedia":false,"detectIdleCheckFullscreen":true}"#, None).apply(&mut settings);
        assert!(!settings.tray_scroll);
        assert!(settings.idle_dim.enabled);
        assert_eq!(settings.idle_dim.minutes, 3);
        assert_eq!(settings.idle_dim.level, 0);
        assert!(settings.idle_dim.respect_busy);
        fixture(r#"{"detectIdleTimeEnabled":false,"detectIdleTimeMinutes":0,"detectIdleTimeSeconds":0,"detectIdleMedia":false,"detectIdleCheckFullscreen":false}"#, None).apply(&mut settings);
        assert!(!settings.idle_dim.enabled);
        assert!(!settings.idle_dim.respect_busy);
        assert_eq!(settings.idle_dim.minutes, 1);
        fixture(
            r#"{"detectIdleTimeSeconds":61,"detectIdleMedia":true}"#,
            None,
        )
        .apply(&mut settings);
        assert_eq!(settings.idle_dim.minutes, 2);
        assert!(settings.idle_dim.respect_busy);
    }
    #[test]
    fn schedules_import_clock_sun_and_legacy_times_and_reapply_without_duplicates() {
        let import = fixture(
            r#"{"adjustmentTimes":[
            {"time":"08:15","brightness":75},
            {"useSunCalc":true,"sunCalc":"sunrise","offset":"-30","brightness":50},
            {"useSunCalc":true,"sunCalc":"sunset","offset":10,"brightness":20},
            {"hour":"12","minute":5,"am":"AM","brightness":0},
            {"hour":1,"minute":"30","am":"pm","brightness":65},
            {"useSunCalc":true,"sunCalc":"dawn","brightness":30},
            {"time":"25:00","brightness":20},
            {"time":"12:30","brightness":101},
            {"useSunCalc":true,"sunCalc":"sunrise","offset":1441,"brightness":50}
        ],"adjustmentTimeLatitude":"55.75","adjustmentTimeLongitude":"37.62","adjustmentTimeAnimate":true,"adjustmentTimeSpeed":"slow"}"#,
            None,
        );
        let mut settings = Settings::default();
        let report = import.apply(&mut settings);
        assert_eq!(
            (
                report.imported_schedule_entries,
                report.skipped_schedule_entries
            ),
            (5, 4)
        );
        assert!(settings.schedule.enabled);
        assert_eq!(settings.schedule.latitude, Some(55.75));
        assert_eq!(settings.schedule.longitude, Some(37.62));
        assert_eq!(settings.schedule.fade_seconds, 300);
        assert_eq!(
            settings
                .schedule
                .entries
                .iter()
                .map(|entry| entry.time)
                .collect::<Vec<_>>(),
            [
                ScheduleTime::Clock { minutes: 495 },
                ScheduleTime::Sunrise { offset: -30 },
                ScheduleTime::Sunset { offset: 10 },
                ScheduleTime::Clock { minutes: 5 },
                ScheduleTime::Clock { minutes: 810 },
            ]
        );
        assert_eq!(
            settings.schedule.entries[0].target,
            ScheduleTarget::Level { level: 75 }
        );
        assert_eq!(import.resolve(&[], &mut settings), report);
        assert_eq!(settings.schedule.entries.len(), 5);
    }
    #[test]
    fn individual_schedule_requires_identical_valid_monitor_levels() {
        let import = fixture(
            r#"{"adjustmentTimeIndividualDisplays":true,"adjustmentTimes":[
            {"time":"10:00","monitors":{"a":30,"b":30},"brightness":80},
            {"time":"11:00","monitors":{"a":20,"b":30}},
            {"time":"12:00","monitors":{}},
            {"time":"13:00","monitors":{"a":-1,"b":30}},
            {"time":"14:00","monitors":{"a":30.1,"b":30.2}}
        ]}"#,
            None,
        );
        let mut settings = Settings::default();
        let report = import.apply(&mut settings);
        assert_eq!(
            (
                report.imported_schedule_entries,
                report.skipped_schedule_entries
            ),
            (1, 4)
        );
        assert_eq!(
            settings.schedule.entries[0].target,
            ScheduleTarget::Level { level: 30 }
        );
        assert!(settings.profiles.is_empty());
    }
    #[test]
    fn fade_presets_location_and_empty_schedule_are_conservative() {
        for (speed, seconds) in [
            ("instant", 0),
            ("fastest", 30),
            ("faster", 30),
            ("normal", 60),
            ("slow", 300),
            ("slowest", 900),
        ] {
            let mut settings = Settings::default();
            let raw = serde_json::json!({"adjustmentTimeAnimate":true,"adjustmentTimeSpeed":speed});
            fixture(&raw.to_string(), None).apply(&mut settings);
            assert_eq!(settings.schedule.fade_seconds, seconds);
        }
        let mut settings = Settings::default();
        fixture(r#"{"adjustmentTimeAnimate":false,"adjustmentTimeSpeed":"slowest","adjustmentTimeLatitude":0,"adjustmentTimeLongitude":0,"adjustmentTimes":[]}"#, None).apply(&mut settings);
        assert_eq!(settings.schedule.fade_seconds, 0);
        assert_eq!(settings.schedule.latitude, None);
        assert!(!settings.schedule.enabled);
        fixture(
            r#"{"adjustmentTimeLatitude":0,"adjustmentTimeLongitude":37}"#,
            None,
        )
        .apply(&mut settings);
        assert_eq!(settings.schedule.latitude, Some(0.0));
        fixture(
            r#"{"adjustmentTimeLatitude":91,"adjustmentTimeLongitude":180}"#,
            None,
        )
        .apply(&mut settings);
        assert_eq!(settings.schedule.latitude, Some(0.0));
        let report = fixture(r#"{"adjustmentTimes":{}}"#, None).apply(&mut settings);
        assert_eq!(report.skipped_schedule_entries, 1);
    }
    #[test]
    fn absent_or_invalid_new_fields_preserve_existing_settings() {
        let mut settings = Settings::default();
        settings.shortcuts.turn_off = accelerator("Ctrl+End");
        settings.schedule.entries.push(ScheduleEntry::default());
        settings.schedule.enabled = true;
        let previous = settings.clone();
        let report = fixture(r#"{"hotkeys":[],"hotkeyPercent":"invalid","scrollShortcut":null,"detectIdleTimeMinutes":-1,"detectIdleTimeSeconds":"oops","adjustmentTimes":[{"time":"9223372036854775807:00","brightness":50},{"time":"12:00","brightness":50,"useSunCalc":"unknown"}]}"#, None).apply(&mut settings);
        assert_eq!(report.skipped_schedule_entries, 2);
        assert_eq!(settings.shortcuts, previous.shortcuts);
        assert_eq!(settings.shortcut_step, previous.shortcut_step);
        assert_eq!(settings.tray_scroll, previous.tray_scroll);
        assert_eq!(settings.idle_dim.minutes, previous.idle_dim.minutes);
        assert_eq!(settings.schedule, previous.schedule);
        let restored: PendingImport = serde_json::from_str(
            &serde_json::to_string(&fixture(
                r#"{"hotkeyPercent":12,"scrollShortcut":false}"#,
                None,
            ))
            .unwrap(),
        )
        .unwrap();
        restored.apply(&mut settings);
        assert_eq!(settings.shortcut_step, 12);
        assert!(!settings.tray_scroll);
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
