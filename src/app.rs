//! Shared app state on the GPUI thread: the latest monitor snapshot, the
//! live settings and the actions every surface (popup, Settings, tray)
//! performs on them.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::Context;

use crate::levels;
use crate::monitors::{Command, Event, LevelTarget, Monitor, MonitorService, SnapshotReason};
use crate::settings::{MonitorSettings, Profile, ProfileIcon, ProfileLevels, Settings};
use crate::twinkle_import::PendingImport;

/// Persists a settings snapshot off the UI thread (the serial settings
/// writer). Called after every edit.
pub type PersistFn = Arc<dyn Fn(&Settings) + Send + Sync>;

pub struct AppModel {
    pub monitors: Vec<Monitor>,
    pub settings: Settings,
    /// A first hardware snapshot arrived; before that the popup shows a
    /// loading state instead of "No displays found".
    pub loaded: bool,
    /// Twinkle Tray settings found on first run, waiting for the user's
    /// answer in the popup.
    pub import_offer: Option<PendingImport>,
    /// Accepted import whose name-keyed entries resolve on the next snapshot.
    import_pending: Option<PendingImport>,
    /// When the user last set each display, so hardware echoes of earlier
    /// throttled writes cannot drag the slider back mid-drag.
    local_edits: HashMap<String, Instant>,
    service: Arc<MonitorService>,
    persist: PersistFn,
}

/// One row the popup shows.
#[derive(Clone, Debug)]
pub struct MonitorRow {
    pub monitor: Monitor,
    pub settings: MonitorSettings,
    pub name: String,
    /// Slider (normalized) brightness, `None` when unsupported/unknown.
    pub slider: Option<u8>,
}

impl MonitorRow {
    /// Contrast for the flyout: the display answers VCP 0x12 and the user
    /// chose to show it.
    pub fn contrast(&self) -> Option<u8> {
        self.monitor
            .contrast
            .filter(|_| self.settings.show_contrast)
    }
}

impl AppModel {
    pub fn new(settings: Settings, service: Arc<MonitorService>, persist: PersistFn) -> Self {
        service.send(Command::SetThrottle(throttle(&settings)));
        publish_hotkeys(&settings);
        Self {
            monitors: Vec::new(),
            settings,
            loaded: false,
            import_offer: None,
            import_pending: None,
            local_edits: HashMap::new(),
            service,
            persist,
        }
    }

    // ----- hardware events ---------------------------------------------------

    pub fn handle_monitor_event(&mut self, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::Snapshot { monitors, reason } => {
                self.monitors = monitors;
                self.loaded = true;
                if let Some(pending) = self.import_pending.take() {
                    let before = self.settings.clone();
                    let report = pending.resolve(&self.monitors, &mut self.settings);
                    crate::logger::info(format!("Twinkle Tray import resolved: {report:?}"));
                    self.apply_runtime_effects(&before);
                    self.save();
                }
                if self.settings.restore_levels {
                    let targets = restore_targets(&self.monitors, &self.settings, &reason);
                    if !targets.is_empty() {
                        self.service.send(Command::Apply(targets));
                    }
                }
                // Remember levels of displays seen for the first time, so
                // the next resume restores what the user saw.
                if self.record_unknown_levels() {
                    self.save();
                }
            }
            Event::Updated(monitor) => {
                if let Some(slot) = self.monitors.iter_mut().find(|m| m.id == monitor.id) {
                    // Levels the user set moments ago win over echoes of
                    // older writes still draining through the throttle.
                    let recent = self
                        .local_edits
                        .get(&monitor.id)
                        .is_some_and(|at| at.elapsed() < LOCAL_EDIT_HOLD);
                    let (brightness, sdr, contrast) = (slot.brightness, slot.sdr, slot.contrast);
                    *slot = monitor;
                    if recent || slot.brightness.is_none() {
                        slot.brightness = brightness.or(slot.brightness);
                    }
                    if recent && slot.sdr.is_some() {
                        slot.sdr = sdr.or(slot.sdr);
                    }
                    if recent && slot.contrast.is_some() {
                        slot.contrast = contrast.or(slot.contrast);
                    }
                } else {
                    self.monitors.push(monitor);
                    self.monitors.sort_by_key(|m| m.num);
                }
            }
        }
        cx.notify();
    }

    fn record_unknown_levels(&mut self) -> bool {
        let mut changed = false;
        for monitor in &self.monitors {
            let entry = self
                .settings
                .monitors
                .entry(monitor.id.clone())
                .or_default();
            if entry.last_brightness.is_none()
                && let Some(hardware) = monitor.brightness
            {
                entry.last_brightness = Some(levels::from_hardware(hardware, entry.min, entry.max));
                changed = true;
            }
            if entry.last_sdr.is_none()
                && let Some(sdr) = monitor.sdr
            {
                entry.last_sdr = Some(sdr.percent);
                changed = true;
            }
            if entry.last_contrast.is_none()
                && let Some(contrast) = monitor.contrast
            {
                entry.last_contrast = Some(contrast);
                changed = true;
            }
        }
        changed
    }

    // ----- view data ---------------------------------------------------------

    /// Visible displays in the user's order.
    pub fn rows(&self) -> Vec<MonitorRow> {
        let mut rows = self.all_rows();
        rows.retain(|row| !row.settings.hidden);
        rows
    }

    /// Every connected display in the user's order, hidden ones included.
    pub fn all_rows(&self) -> Vec<MonitorRow> {
        let mut rows: Vec<MonitorRow> = self
            .monitors
            .iter()
            .map(|monitor| {
                let settings = self.settings.monitor(&monitor.id);
                let name = settings
                    .name
                    .clone()
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or_else(|| monitor.name.clone());
                let slider = monitor
                    .brightness
                    .map(|hardware| levels::from_hardware(hardware, settings.min, settings.max));
                MonitorRow {
                    monitor: monitor.clone(),
                    settings,
                    name,
                    slider,
                }
            })
            .collect();
        let order = &self.settings.order;
        rows.sort_by_key(|row| {
            (
                order
                    .iter()
                    .position(|id| *id == row.monitor.id)
                    .unwrap_or(usize::MAX),
                row.monitor.num,
            )
        });
        rows
    }

    /// Slider level of the primary display (the one at the desktop origin).
    pub fn primary_level(&self) -> Option<u8> {
        self.all_rows()
            .into_iter()
            .find(|row| row.monitor.bounds.is_some_and(|b| b.x == 0 && b.y == 0))
            .and_then(|row| row.slider)
    }

    // ----- actions -----------------------------------------------------------

    /// Set one display from its slider value (0..=100, normalized).
    pub fn set_brightness(&mut self, id: &str, slider: u8, cx: &mut Context<Self>) {
        let entry = self.settings.monitors.entry(id.to_owned()).or_default();
        let hardware = levels::to_hardware(slider, entry.min, entry.max);
        entry.last_brightness = Some(slider);
        if let Some(monitor) = self.monitors.iter_mut().find(|m| m.id == id) {
            monitor.brightness = Some(hardware);
        }
        self.local_edits.insert(id.to_owned(), Instant::now());
        self.service.send(Command::SetBrightness {
            id: id.to_owned(),
            percent: hardware,
        });
        self.save();
        cx.notify();
    }

    /// The "All displays" slider: every visible controllable display.
    pub fn set_all(&mut self, slider: u8, cx: &mut Context<Self>) {
        let ids: Vec<String> = self
            .rows()
            .into_iter()
            .filter(|row| row.slider.is_some() && !row.monitor.unresponsive)
            .map(|row| row.monitor.id)
            .collect();
        for id in ids {
            let entry = self.settings.monitors.entry(id.clone()).or_default();
            let hardware = levels::to_hardware(slider, entry.min, entry.max);
            entry.last_brightness = Some(slider);
            if let Some(monitor) = self.monitors.iter_mut().find(|m| m.id == id) {
                monitor.brightness = Some(hardware);
            }
            self.local_edits.insert(id.clone(), Instant::now());
            self.service.send(Command::SetBrightness {
                id,
                percent: hardware,
            });
        }
        self.save();
        cx.notify();
    }

    pub fn set_sdr(&mut self, id: &str, percent: u8, cx: &mut Context<Self>) {
        let percent = percent.min(100);
        self.settings
            .monitors
            .entry(id.to_owned())
            .or_default()
            .last_sdr = Some(percent);
        if let Some(sdr) = self
            .monitors
            .iter_mut()
            .find(|m| m.id == id)
            .and_then(|m| m.sdr.as_mut())
        {
            sdr.percent = percent;
        }
        self.local_edits.insert(id.to_owned(), Instant::now());
        self.service.send(Command::SetSdr {
            id: id.to_owned(),
            percent,
        });
        self.save();
        cx.notify();
    }

    pub fn set_contrast(&mut self, id: &str, percent: u8, cx: &mut Context<Self>) {
        self.apply_contrast(id, percent);
        self.save();
        cx.notify();
    }

    /// The linked contrast slider: every visible display showing contrast.
    pub fn set_contrast_all(&mut self, percent: u8, cx: &mut Context<Self>) {
        let ids: Vec<String> = self
            .rows()
            .into_iter()
            .filter(|row| row.contrast().is_some() && !row.monitor.unresponsive)
            .map(|row| row.monitor.id)
            .collect();
        for id in ids {
            self.apply_contrast(&id, percent);
        }
        self.save();
        cx.notify();
    }

    fn apply_contrast(&mut self, id: &str, percent: u8) {
        let percent = percent.min(100);
        self.settings
            .monitors
            .entry(id.to_owned())
            .or_default()
            .last_contrast = Some(percent);
        if let Some(monitor) = self.monitors.iter_mut().find(|m| m.id == id) {
            monitor.contrast = Some(percent);
        }
        self.local_edits.insert(id.to_owned(), Instant::now());
        self.service.send(Command::SetContrast {
            id: id.to_owned(),
            percent,
        });
    }

    /// Offer a one-time Twinkle Tray import (first run only).
    pub fn offer_import(&mut self, pending: PendingImport, cx: &mut Context<Self>) {
        if !self.settings.twinkle_import_done {
            self.import_offer = Some(pending);
            cx.notify();
        }
    }

    /// The user answered the import offer, or imported from Settings.
    pub fn answer_import(&mut self, accept: bool, cx: &mut Context<Self>) {
        let offer = self.import_offer.take();
        if accept && let Some(pending) = offer {
            self.run_import(pending, cx);
        }
        self.edit(cx, |settings| settings.twinkle_import_done = true);
    }

    /// Apply an import now; names resolve against the current (or next)
    /// snapshot.
    pub fn run_import(&mut self, pending: PendingImport, cx: &mut Context<Self>) -> usize {
        let before = self.settings.clone();
        let report = if self.loaded {
            pending.resolve(&self.monitors, &mut self.settings)
        } else {
            let report = pending.apply(&mut self.settings);
            self.import_pending = Some(pending);
            report
        };
        crate::logger::info(format!("Twinkle Tray import: {report:?}"));
        self.settings.twinkle_import_done = true;
        self.apply_runtime_effects(&before);
        self.save();
        cx.notify();
        report.monitors
    }

    /// Move a display `delta` places in the user's order.
    pub fn move_monitor(&mut self, id: &str, delta: isize, cx: &mut Context<Self>) {
        let mut ids: Vec<String> = self
            .all_rows()
            .into_iter()
            .map(|row| row.monitor.id)
            .collect();
        // Keep the place of displays that are remembered but disconnected.
        for known in &self.settings.order {
            if !ids.contains(known) {
                ids.push(known.clone());
            }
        }
        let Some(from) = ids.iter().position(|known| known == id) else {
            return;
        };
        let to = (from as isize + delta).clamp(0, ids.len() as isize - 1) as usize;
        if to == from {
            return;
        }
        let moved = ids.remove(from);
        ids.insert(to, moved);
        self.edit(cx, move |settings| settings.order = ids);
    }

    pub fn toggle_link_levels(&mut self, cx: &mut Context<Self>) {
        self.edit(cx, |settings| settings.link_levels = !settings.link_levels);
    }

    /// Switch one display on or off through DDC/CI.
    pub fn set_power(&mut self, id: &str, on: bool, cx: &mut Context<Self>) {
        if let Some(monitor) = self.monitors.iter_mut().find(|m| m.id == id) {
            monitor.powered_off = !on;
        }
        self.service.send(Command::SetPower {
            id: id.to_owned(),
            on,
        });
        cx.notify();
    }

    /// Switch HDR on or off for one display.
    pub fn set_hdr(&mut self, id: &str, on: bool) {
        self.service.send(Command::SetHdr {
            id: id.to_owned(),
            on,
        });
    }

    // ----- profiles ---------------------------------------------------------

    /// Current levels of every visible display, as a profile would save them.
    fn current_levels(&self) -> std::collections::BTreeMap<String, ProfileLevels> {
        self.rows()
            .into_iter()
            .filter(|row| !row.monitor.unresponsive)
            .filter_map(|row| {
                let levels = ProfileLevels {
                    brightness: row.slider,
                    sdr: row.monitor.sdr.map(|sdr| sdr.percent),
                    contrast: row.contrast(),
                };
                (levels != ProfileLevels::default()).then(|| (row.monitor.id.clone(), levels))
            })
            .collect()
    }

    /// Whether the bottom bar can save a profile right now.
    pub fn can_save_profile(&self) -> bool {
        !self.current_levels().is_empty()
    }

    /// Save the current levels as a new profile; returns its index.
    pub fn save_profile(&mut self, cx: &mut Context<Self>) -> Option<usize> {
        let levels = self.current_levels();
        if levels.is_empty() {
            return None;
        }
        // A fresh glyph tells the new profile apart until the user picks one.
        let icon = ProfileIcon::ALL
            .into_iter()
            .find(|icon| self.settings.profiles.iter().all(|t| t.icon != *icon))
            .unwrap_or_default();
        self.edit(cx, |settings| {
            settings.profiles.push(Profile {
                icon,
                levels,
                ..Default::default()
            })
        });
        Some(self.settings.profiles.len() - 1)
    }

    /// Apply a profile to the connected displays it knows.
    pub fn apply_profile(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(profile) = self.settings.profiles.get(index).cloned() else {
            return;
        };
        let mut targets = Vec::new();
        for (id, saved) in &profile.levels {
            let Some(monitor) = self
                .monitors
                .iter_mut()
                .find(|m| m.id == *id && !m.unresponsive)
            else {
                continue;
            };
            let entry = self.settings.monitors.entry(id.clone()).or_default();
            let mut target = LevelTarget {
                id: id.clone(),
                brightness: None,
                sdr: None,
                contrast: None,
            };
            if let Some(slider) = saved.brightness.filter(|_| monitor.brightness.is_some()) {
                let hardware = levels::to_hardware(slider, entry.min, entry.max);
                monitor.brightness = Some(hardware);
                entry.last_brightness = Some(slider);
                target.brightness = Some(hardware);
            }
            if let (Some(percent), Some(sdr)) = (saved.sdr, monitor.sdr.as_mut()) {
                let percent = percent.min(100);
                sdr.percent = percent;
                entry.last_sdr = Some(percent);
                target.sdr = Some(percent);
            }
            if let Some(percent) = saved.contrast.filter(|_| monitor.contrast.is_some()) {
                let percent = percent.min(100);
                monitor.contrast = Some(percent);
                entry.last_contrast = Some(percent);
                target.contrast = Some(percent);
            }
            if target.brightness.is_some() || target.sdr.is_some() || target.contrast.is_some() {
                self.local_edits.insert(id.clone(), Instant::now());
                targets.push(target);
            }
        }
        if !targets.is_empty() {
            self.service.send(Command::Apply(targets));
            self.save();
            cx.notify();
        }
    }

    /// The connected displays a profile knows all show its levels.
    pub fn profile_active(&self, profile: &Profile) -> bool {
        let mut matched = false;
        for (id, saved) in &profile.levels {
            let Some(monitor) = self.monitors.iter().find(|m| m.id == *id) else {
                continue;
            };
            let entry = self.settings.monitor(id);
            let brightness = monitor
                .brightness
                .map(|hardware| levels::from_hardware(hardware, entry.min, entry.max));
            let same = |saved: Option<u8>, current: Option<u8>| {
                saved.is_none() || current.is_none() || saved == current
            };
            if !same(saved.brightness, brightness)
                || !same(saved.sdr, monitor.sdr.map(|sdr| sdr.percent))
                || !same(saved.contrast, monitor.contrast)
            {
                return false;
            }
            matched = true;
        }
        matched
    }

    pub fn set_profile_icon(&mut self, index: usize, icon: ProfileIcon, cx: &mut Context<Self>) {
        self.edit(cx, |settings| {
            if let Some(profile) = settings.profiles.get_mut(index) {
                profile.icon = icon;
            }
        });
    }

    /// The profile's name, or its numbered default.
    pub fn profile_name(&self, index: usize) -> String {
        self.settings
            .profiles
            .get(index)
            .map(|profile| profile.name.trim())
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| {
                crate::i18n::format(
                    "profiles-default-name",
                    &[("number", (index + 1).to_string())],
                )
            })
    }

    /// Display name for a profile entry, connected or not.
    pub fn display_name(&self, id: &str) -> String {
        self.all_rows()
            .into_iter()
            .find(|row| row.monitor.id == id)
            .map(|row| row.name)
            .or_else(|| self.settings.monitors.get(id).and_then(|m| m.name.clone()))
            .unwrap_or_else(|| id.to_owned())
    }

    pub fn rename_profile(&mut self, index: usize, name: String, cx: &mut Context<Self>) {
        self.edit(cx, |settings| {
            if let Some(profile) = settings.profiles.get_mut(index) {
                profile.name = name.trim().to_owned();
            }
        });
    }

    /// Give a profile a shortcut, taking it away from any other profile.
    pub fn set_profile_hotkey(
        &mut self,
        index: usize,
        hotkey: Option<crate::hotkey::Hotkey>,
        cx: &mut Context<Self>,
    ) {
        self.edit(cx, |settings| {
            for (other, profile) in settings.profiles.iter_mut().enumerate() {
                if other == index {
                    profile.hotkey = hotkey;
                } else if hotkey.is_some() && profile.hotkey == hotkey {
                    profile.hotkey = None;
                }
            }
        });
    }

    /// Edit one stored level of a profile without touching the hardware.
    pub fn set_profile_level(
        &mut self,
        index: usize,
        id: &str,
        change: impl FnOnce(&mut ProfileLevels),
        cx: &mut Context<Self>,
    ) {
        let id = id.to_owned();
        self.edit(cx, move |settings| {
            if let Some(levels) = settings
                .profiles
                .get_mut(index)
                .and_then(|profile| profile.levels.get_mut(&id))
            {
                change(levels);
            }
        });
    }

    /// Replace a profile's levels with the current ones.
    pub fn recapture_profile(&mut self, index: usize, cx: &mut Context<Self>) {
        let levels = self.current_levels();
        if levels.is_empty() {
            return;
        }
        self.edit(cx, |settings| {
            if let Some(profile) = settings.profiles.get_mut(index) {
                profile.levels = levels;
            }
        });
    }

    /// Move a profile to another place in the bottom bar.
    pub fn move_profile(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        self.edit(cx, |settings| {
            let profiles = &mut settings.profiles;
            if from < profiles.len() && to < profiles.len() && from != to {
                let moved = profiles.remove(from);
                profiles.insert(to, moved);
            }
        });
    }

    pub fn delete_profile(&mut self, index: usize, cx: &mut Context<Self>) {
        self.edit(cx, |settings| {
            if index < settings.profiles.len() {
                settings.profiles.remove(index);
            }
        });
    }

    pub fn refresh(&self) {
        self.service.send(Command::Refresh);
    }

    /// Pick up levels changed outside the app (monitor buttons, other tools).
    pub fn read_levels(&self) {
        self.service.send(Command::ReadLevels);
    }

    /// Apply a settings change live, then persist it.
    pub fn edit(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut Settings)) {
        let before = self.settings.clone();
        change(&mut self.settings);
        self.apply_runtime_effects(&before);
        self.save();
        cx.notify();
    }

    /// Settings were replaced from outside this model (import).
    pub fn replace_settings(&mut self, settings: Settings, cx: &mut Context<Self>) {
        let before = std::mem::replace(&mut self.settings, settings);
        self.apply_runtime_effects(&before);
        self.save();
        cx.notify();
    }

    /// Another thread changed the stored settings; nothing is persisted here.
    pub fn reload_settings(&mut self, cx: &mut Context<Self>) {
        cx.notify();
    }

    fn apply_runtime_effects(&self, before: &Settings) {
        let now = &self.settings;
        publish_hotkeys(now);
        if now.update_interval_ms != before.update_interval_ms {
            self.service.send(Command::SetThrottle(throttle(now)));
        }
        if now.popup_material != before.popup_material {
            crate::popup::apply_popup_appearance(now.popup_material);
        }
        if now.tray_icon != before.tray_icon {
            crate::tray::set_icon(now.tray_icon);
        }
        if now.language != before.language {
            crate::i18n::set_language(now.language);
            crate::tray::refresh_menu();
        }
        if now.launch_at_startup != before.launch_at_startup
            && let Err(error) = crate::autostart::set_enabled(now.launch_at_startup)
        {
            crate::logger::error(format!("could not update startup registration: {error:#}"));
        }
        let min_max_changed = now.monitors.iter().any(|(id, entry)| {
            before
                .monitors
                .get(id)
                .is_none_or(|old| old.min != entry.min || old.max != entry.max)
        });
        // A new range moves the hardware level under the same slider value.
        if min_max_changed {
            let targets: Vec<LevelTarget> = self
                .monitors
                .iter()
                .filter_map(|monitor| {
                    let entry = now.monitors.get(&monitor.id)?;
                    let old = before
                        .monitors
                        .get(&monitor.id)
                        .cloned()
                        .unwrap_or_default();
                    if old.min == entry.min && old.max == entry.max {
                        return None;
                    }
                    let slider = entry.last_brightness?;
                    Some(LevelTarget {
                        id: monitor.id.clone(),
                        brightness: Some(levels::to_hardware(slider, entry.min, entry.max)),
                        sdr: None,
                        contrast: None,
                    })
                })
                .collect();
            if !targets.is_empty() {
                self.service.send(Command::Apply(targets));
            }
        }
    }

    fn save(&self) {
        (self.persist)(&self.settings);
    }
}

/// How long a local edit outranks hardware reports for the same display.
const LOCAL_EDIT_HOLD: Duration = Duration::from_millis(1500);

/// Hand the profile shortcuts to the tray thread, which registers them.
fn publish_hotkeys(settings: &Settings) {
    crate::hotkey::set_desired(
        settings
            .profiles
            .iter()
            .map(|profile| profile.hotkey)
            .collect(),
    );
}

fn throttle(settings: &Settings) -> std::time::Duration {
    std::time::Duration::from_millis(u64::from(settings.update_interval_ms.clamp(16, 2000)))
}

/// Levels to write after a snapshot: the remembered level of every display
/// whose hardware differs from it. A plain refresh never overrides what the
/// user set with the monitor's own buttons.
pub fn restore_targets(
    monitors: &[Monitor],
    settings: &Settings,
    reason: &SnapshotReason,
) -> Vec<LevelTarget> {
    if *reason == SnapshotReason::Requested {
        return Vec::new();
    }
    monitors
        .iter()
        .filter(|monitor| !monitor.unresponsive)
        .filter_map(|monitor| {
            let entry = settings.monitors.get(&monitor.id)?;
            let brightness = entry
                .last_brightness
                .map(|slider| levels::to_hardware(slider, entry.min, entry.max))
                .filter(|hardware| monitor.brightness.is_some_and(|now| now != *hardware));
            let sdr = entry
                .last_sdr
                .filter(|percent| monitor.sdr.is_some_and(|now| now.percent != *percent));
            let contrast = entry
                .last_contrast
                .filter(|percent| monitor.contrast.is_some_and(|now| now != *percent));
            (brightness.is_some() || sdr.is_some() || contrast.is_some()).then(|| LevelTarget {
                id: monitor.id.clone(),
                brightness,
                sdr,
                contrast,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitors::{BrightnessBackend, MonitorKind, SdrLevel};

    fn monitor(id: &str, brightness: Option<u8>, sdr: Option<u8>) -> Monitor {
        Monitor {
            id: id.into(),
            model: "DEL41D9".into(),
            device_path: String::new(),
            name: "Dell".into(),
            kind: MonitorKind::External,
            num: 1,
            bounds: None,
            backend: BrightnessBackend::Ddc { vcp: 0x10 },
            brightness,
            sdr: sdr.map(|percent| SdrLevel { percent }),
            contrast: None,
            hdr_supported: false,
            supports_ddc_power: false,
            powered_off: false,
            unresponsive: false,
        }
    }

    fn settings_with(id: &str, entry: MonitorSettings) -> Settings {
        let mut settings = Settings::default();
        settings.monitors.insert(id.into(), entry);
        settings
    }

    #[test]
    fn restore_writes_only_displays_that_drifted() {
        let settings = settings_with(
            "a",
            MonitorSettings {
                last_brightness: Some(50),
                min: 20,
                max: 80,
                ..Default::default()
            },
        );
        // 50 % of 20..80 is 50 hardware.
        let same = restore_targets(
            &[monitor("a", Some(50), None)],
            &settings,
            &SnapshotReason::Resumed,
        );
        assert!(same.is_empty());
        let drifted = restore_targets(
            &[monitor("a", Some(100), None)],
            &settings,
            &SnapshotReason::Resumed,
        );
        assert_eq!(drifted.len(), 1);
        assert_eq!(drifted[0].brightness, Some(50));
    }

    #[test]
    fn manual_refresh_never_restores() {
        let settings = settings_with(
            "a",
            MonitorSettings {
                last_brightness: Some(10),
                ..Default::default()
            },
        );
        let targets = restore_targets(
            &[monitor("a", Some(90), None)],
            &settings,
            &SnapshotReason::Requested,
        );
        assert!(targets.is_empty());
    }

    #[test]
    fn sdr_restores_independently() {
        let settings = settings_with(
            "a",
            MonitorSettings {
                last_sdr: Some(30),
                ..Default::default()
            },
        );
        let targets = restore_targets(
            &[monitor("a", Some(40), Some(60))],
            &settings,
            &SnapshotReason::DisplaysChanged,
        );
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].brightness, None);
        assert_eq!(targets[0].sdr, Some(30));
    }

    #[test]
    fn contrast_restores_independently() {
        let settings = settings_with(
            "a",
            MonitorSettings {
                last_contrast: Some(70),
                ..Default::default()
            },
        );
        let mut drifted = monitor("a", Some(40), None);
        drifted.contrast = Some(50);
        let targets = restore_targets(&[drifted], &settings, &SnapshotReason::Resumed);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].brightness, None);
        assert_eq!(targets[0].contrast, Some(70));
        // Displays without a contrast control are never written.
        let unsupported = restore_targets(
            &[monitor("a", Some(40), None)],
            &settings,
            &SnapshotReason::Resumed,
        );
        assert!(unsupported.is_empty());
    }
}
