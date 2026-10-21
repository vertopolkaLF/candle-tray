//! Shared app state on the GPUI thread: the latest monitor snapshot, the
//! live settings and the actions every surface (popup, Settings, tray)
//! performs on them.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::Context;

use crate::hotkey::{Action, Hotkey};
use crate::levels;
use crate::monitors::{Command, Event, LevelTarget, Monitor, MonitorService, SnapshotReason};
use crate::settings::{
    MonitorSettings, Profile, ProfileIcon, ProfileLevels, ScheduleTarget, Settings,
};
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
    pending_cli: Vec<crate::cli::Command>,
    /// When the user last set each display, so hardware echoes of earlier
    /// throttled writes cannot drag the slider back mid-drag.
    local_edits: HashMap<String, Instant>,
    /// Set while the displays are dimmed for inactivity: the displays that
    /// were dimmed. Their remembered levels come back on the next input.
    idle_dimmed: Option<Vec<String>>,
    /// The schedule entry applied last and when it fired (Unix seconds).
    schedule_applied: Option<(usize, i64)>,
    /// A schedule change fading in.
    fade: Option<Fade>,
    /// The app trigger in effect and the levels from before it, restored
    /// when its app leaves the foreground. Dropped when the user changes
    /// levels in the meantime.
    app_trigger: Option<(usize, BTreeMap<String, ProfileLevels>)>,
    /// The last smoothed ambient light reading, applied again whenever the
    /// sensor may lead once more (after idle, an app trigger, edits).
    ambient_lux: Option<f32>,
    /// The app last reported in the foreground.
    foreground: Option<String>,
    /// Last reported power source (on battery); the first report only sets it.
    on_battery: Option<bool>,
    service: Arc<MonitorService>,
    persist: PersistFn,
}

/// Brightness moving from one level to another over time.
struct Fade {
    started: Instant,
    duration: Duration,
    steps: Vec<FadeStep>,
}

struct FadeStep {
    id: String,
    from: u8,
    to: u8,
    /// Level written last.
    current: u8,
}

/// Pause before turning displays off from a shortcut or the tray menu.
const TURN_OFF_DELAY: Duration = Duration::from_millis(500);

/// One row the popup shows.
#[derive(Clone, Debug)]
pub struct MonitorRow {
    pub monitor: Monitor,
    pub settings: MonitorSettings,
    pub name: String,
    /// The display's one level slider: normalized brightness, or the SDR
    /// white level while HDR is on. `None` when unsupported/unknown.
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
        for (id, entry) in &settings.monitors {
            if entry.brightness_vcp.is_some() {
                service.send(Command::SetBrightnessVcp {
                    id: id.clone(),
                    vcp: entry.brightness_vcp,
                });
            }
        }
        publish_hotkeys(&settings);
        crate::tray::set_scroll_enabled(settings.tray_scroll);
        crate::light_sensor::set_enabled(settings.adaptive.enabled);
        Self {
            monitors: Vec::new(),
            settings,
            loaded: false,
            import_offer: None,
            import_pending: None,
            pending_cli: Vec::new(),
            local_edits: HashMap::new(),
            idle_dimmed: None,
            schedule_applied: None,
            fade: None,
            app_trigger: None,
            on_battery: None,
            foreground: None,
            ambient_lux: None,
            service,
            persist,
        }
    }

    // ----- hardware events ---------------------------------------------------

    pub fn handle_monitor_event(&mut self, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::Snapshot { monitors, reason } => {
                let previous = std::mem::replace(&mut self.monitors, monitors);
                for old in &previous {
                    self.carry_level(old);
                }
                let first = !self.loaded;
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
                // Restoring brightened dimmed displays; dim them again on the
                // next tick if the user is still away. Without restoring they
                // stay dimmed and must come back on input.
                if self.settings.restore_levels
                    && self.idle_dimmed.is_some()
                    && reason != SnapshotReason::Requested
                {
                    self.idle_dimmed = None;
                }
                // An app trigger seen before the displays were known applies
                // now, before the schedule (which waits while it holds).
                if first && let Some(exe) = self.foreground.clone() {
                    self.on_foreground(&exe, cx);
                }
                // The schedule applies at startup and catches up after sleep.
                if self.run_schedule(false) {
                    self.save();
                }
                // Remember levels of displays seen for the first time, so
                // the next resume restores what the user saw.
                if self.record_unknown_levels() {
                    self.save();
                }
                if first && self.apply_adaptive() {
                    self.save();
                }
                for command in std::mem::take(&mut self.pending_cli) {
                    self.run_cli(command, cx);
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
                    let old = Monitor {
                        brightness,
                        sdr,
                        ..slot.clone()
                    };
                    self.carry_level(&old);
                } else {
                    self.monitors.push(monitor);
                    self.monitors.sort_by_key(|m| m.num);
                }
            }
        }
        cx.notify();
    }

    /// HDR switched on or off since `old` (from here or from Windows): the
    /// slider keeps its value, now driving the other level.
    fn carry_level(&mut self, old: &Monitor) {
        let Some(monitor) = self.monitors.iter().find(|m| m.id == old.id) else {
            return;
        };
        if monitor.sdr.is_some() == old.sdr.is_some() || monitor.unresponsive {
            return;
        }
        let entry = self.settings.monitor(&old.id);
        let Some(slider) = slider_level(old, &entry).or(entry.last_brightness) else {
            return;
        };
        crate::logger::info(format!(
            "HDR switched on {}; carrying level {slider}",
            old.id
        ));
        let id = old.id.clone();
        self.apply_level(&id, slider);
        self.save();
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
                && let Some(slider) = slider_level(monitor, entry)
            {
                entry.last_brightness = Some(slider);
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
                let slider = slider_level(monitor, &settings);
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

    /// Name and level of every visible display with a level, for the tray
    /// tooltip.
    pub fn tooltip_levels(&self) -> Vec<(String, u8)> {
        self.rows()
            .into_iter()
            .filter_map(|row| Some((row.name, row.slider?)))
            .collect()
    }

    // ----- actions -----------------------------------------------------------

    /// Dispatch a script command on the GPUI thread. Startup commands wait
    /// for the first snapshot, after restore/schedule initialization.
    pub fn run_cli(&mut self, command: crate::cli::Command, cx: &mut Context<Self>) {
        use crate::cli::Command as Cli;
        if command == Cli::Show {
            crate::popup::show_near_cursor();
            return;
        }
        if !self.loaded {
            self.pending_cli.push(command);
            return;
        }
        let result = (|| -> anyhow::Result<()> {
            let selector = match &command {
                Cli::Set(_, selector)
                | Cli::Offset(_, selector)
                | Cli::Contrast(_, selector)
                | Cli::Input(_, selector) => selector,
                Cli::Profile(query) => {
                    let names = (0..self.settings.profiles.len())
                        .map(|index| self.profile_name(index))
                        .collect::<Vec<_>>();
                    self.apply_profile(crate::cli::resolve_profile(query, &names)?, cx);
                    return Ok(());
                }
                Cli::Off => {
                    self.turn_off(cx);
                    return Ok(());
                }
                Cli::Show => unreachable!(),
            };
            let rows = self.rows();
            let entries = rows
                .iter()
                .map(|row| (row.monitor.id.as_str(), row.name.as_str()))
                .collect::<Vec<_>>();
            let selected = selector.resolve(&entries)?;
            for index in selected {
                let row = &rows[index];
                let id = &row.monitor.id;
                if row.monitor.unresponsive || row.monitor.powered_off {
                    crate::logger::error(format!("CLI: display unavailable: {id}"));
                    continue;
                }
                match &command {
                    Cli::Set(value, _) if row.slider.is_some() => {
                        self.set_brightness(id, *value, cx)
                    }
                    Cli::Offset(delta, _) if row.slider.is_some() => {
                        let current = i32::from(row.slider.expect("checked slider"));
                        self.set_brightness(
                            id,
                            current.saturating_add(*delta).clamp(0, 100) as u8,
                            cx,
                        );
                    }
                    Cli::Contrast(value, _) if row.contrast().is_some() => {
                        self.set_contrast(id, *value, cx)
                    }
                    Cli::Input(value, _) => self.set_input(id, *value, cx),
                    _ => crate::logger::error(format!("CLI: requested control unavailable: {id}")),
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            crate::logger::error(format!("CLI command failed: {error:#}"));
        }
    }

    /// Set one display from its slider value (0..=100, normalized).
    pub fn set_brightness(&mut self, id: &str, slider: u8, cx: &mut Context<Self>) {
        self.interrupt_automation();
        self.apply_level(id, slider);
        self.save();
        cx.notify();
    }

    /// Remember a slider value and write it, unless the display is dimmed
    /// for inactivity: then it lands when the user comes back.
    fn apply_level(&mut self, id: &str, slider: u8) {
        let slider = slider.min(100);
        self.settings
            .monitors
            .entry(id.to_owned())
            .or_default()
            .last_brightness = Some(slider);
        if !self
            .idle_dimmed
            .as_ref()
            .is_some_and(|dimmed| dimmed.iter().any(|d| d == id))
        {
            self.write_level(id, slider);
        }
    }

    /// Write a slider value to the level it drives: the SDR white level
    /// while HDR is on, the hardware brightness otherwise. Nothing is
    /// remembered.
    fn write_level(&mut self, id: &str, slider: u8) {
        let slider = slider.min(100);
        let entry = self.settings.monitor(id);
        let Some(monitor) = self.monitors.iter_mut().find(|m| m.id == id) else {
            return;
        };
        self.local_edits.insert(id.to_owned(), Instant::now());
        if let Some(sdr) = monitor.sdr.as_mut() {
            sdr.percent = slider;
            self.service.send(Command::SetSdr {
                id: id.to_owned(),
                percent: slider,
            });
        } else {
            let hardware = levels::to_hardware(slider, entry.min, entry.max);
            monitor.brightness = Some(hardware);
            self.service.send(Command::SetBrightness {
                id: id.to_owned(),
                percent: hardware,
            });
        }
    }

    /// The "All displays" slider: every visible controllable display.
    pub fn set_all(&mut self, slider: u8, cx: &mut Context<Self>) {
        self.interrupt_automation();
        for id in self.controllable_ids() {
            self.apply_level(&id, slider);
        }
        self.save();
        cx.notify();
    }

    /// Visible displays whose level can be set right now.
    fn controllable_ids(&self) -> Vec<String> {
        self.rows()
            .into_iter()
            .filter(|row| {
                row.slider.is_some() && !row.monitor.unresponsive && !row.monitor.powered_off
            })
            .map(|row| row.monitor.id)
            .collect()
    }

    /// Current slider value of a connected display.
    fn slider_of(&self, id: &str) -> Option<u8> {
        let monitor = self.monitors.iter().find(|m| m.id == id)?;
        slider_level(monitor, &self.settings.monitor(id))
    }

    /// Move every visible display by `delta` percent (shortcuts, tray wheel).
    pub fn step_all(&mut self, delta: i32, cx: &mut Context<Self>) {
        self.interrupt_automation();
        for id in self.controllable_ids() {
            if let Some(current) = self.slider_of(&id) {
                let next = (i32::from(current) + delta).clamp(0, 100) as u8;
                if next != current {
                    self.apply_level(&id, next);
                }
            }
        }
        self.save();
        cx.notify();
    }

    /// The wheel turned over the tray icon.
    pub fn scroll_all(&mut self, notches: i32, cx: &mut Context<Self>) {
        if self.settings.tray_scroll {
            // A notch moves as far as it does over a popup slider.
            let step = i32::from(self.settings.slider_step());
            self.step_all(notches * step, cx);
        }
    }

    /// A global shortcut was pressed.
    pub fn run_shortcut(&mut self, action: Action, cx: &mut Context<Self>) {
        let step = i32::from(self.settings.shortcut_step.clamp(1, 100));
        match action {
            Action::Profile(index) => self.apply_profile(index, cx),
            Action::BrightnessUp => self.step_all(step, cx),
            Action::BrightnessDown => self.step_all(-step, cx),
            Action::TurnOff => self.turn_off(cx),
        }
    }

    /// Turn the displays off the way the settings say. Waits a moment
    /// first, as legacy did: releasing the shortcut's keys would otherwise
    /// count as input and wake the displays at once.
    pub fn turn_off(&mut self, cx: &mut Context<Self>) {
        let mode = self.settings.turn_off.into();
        let service = Arc::clone(&self.service);
        cx.spawn(async move |_, cx| {
            cx.background_executor().timer(TURN_OFF_DELAY).await;
            service.send(Command::TurnOff(mode));
        })
        .detach();
    }

    /// Switch a display to another input (DDC/CI VCP 0x60).
    pub fn set_input(&mut self, id: &str, value: u8, cx: &mut Context<Self>) {
        if let Some(monitor) = self.monitors.iter_mut().find(|m| m.id == id) {
            monitor.input = Some(value);
        }
        self.service.send(Command::SetInput {
            id: id.to_owned(),
            value,
        });
        cx.notify();
    }

    pub fn set_contrast(&mut self, id: &str, percent: u8, cx: &mut Context<Self>) {
        self.interrupt_automation();
        self.apply_contrast(id, percent);
        self.save();
        cx.notify();
    }

    /// The linked contrast slider: every visible display showing contrast.
    pub fn set_contrast_all(&mut self, percent: u8, cx: &mut Context<Self>) {
        self.interrupt_automation();
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
                    sdr: None,
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
        self.interrupt_automation();
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
            // Profiles saved before the shared slider may hold only `sdr`.
            if let Some(slider) = saved.brightness.or(saved.sdr).map(|v| v.min(100)) {
                if let Some(sdr) = monitor.sdr.as_mut() {
                    sdr.percent = slider;
                    entry.last_brightness = Some(slider);
                    target.sdr = Some(slider);
                } else if monitor.brightness.is_some() {
                    let hardware = levels::to_hardware(slider, entry.min, entry.max);
                    monitor.brightness = Some(hardware);
                    entry.last_brightness = Some(slider);
                    target.brightness = Some(hardware);
                }
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
            let same = |saved: Option<u8>, current: Option<u8>| {
                saved.is_none() || current.is_none() || saved == current
            };
            if !same(
                saved.brightness.or(saved.sdr),
                slider_level(monitor, &entry),
            ) || !same(saved.contrast, monitor.contrast)
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

    /// Give an action a shortcut, taking it away from any other action.
    pub fn set_hotkey(&mut self, action: Action, hotkey: Option<Hotkey>, cx: &mut Context<Self>) {
        self.edit(cx, |settings| {
            for (known, slot) in settings.hotkeys_mut() {
                if known == action {
                    *slot = hotkey;
                } else if hotkey.is_some() && *slot == hotkey {
                    *slot = None;
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
        self.edit(cx, |settings| settings.move_profile(from, to));
    }

    pub fn delete_profile(&mut self, index: usize, cx: &mut Context<Self>) {
        self.edit(cx, |settings| settings.delete_profile(index));
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
        self.automation_changed(&before, cx);
        self.save();
        cx.notify();
    }

    /// Settings were replaced from outside this model (import).
    pub fn replace_settings(&mut self, settings: Settings, cx: &mut Context<Self>) {
        let before = std::mem::replace(&mut self.settings, settings);
        self.apply_runtime_effects(&before);
        self.automation_changed(&before, cx);
        self.save();
        cx.notify();
    }

    // ----- automation --------------------------------------------------------

    /// One pass of idle dimming, the schedule and its fade. Returns how
    /// soon to run again: often while dimmed (to brighten the moment the
    /// user is back) or fading, rarely otherwise.
    pub fn automation_tick(&mut self, cx: &mut Context<Self>) -> Duration {
        if !self.loaded {
            return Duration::from_secs(1);
        }
        let mut changed = self.check_idle();
        changed |= self.run_schedule(true);
        changed |= self.step_fade();
        if changed {
            self.save();
            cx.notify();
        }
        if self.idle_dimmed.is_some() {
            Duration::from_millis(250)
        } else if self.fade.is_some() {
            Duration::from_secs(1)
        } else {
            Duration::from_secs(5)
        }
    }

    /// Dim after the configured time without input; brighten on input.
    fn check_idle(&mut self) -> bool {
        let config = self.settings.idle_dim;
        let idle = crate::automation::idle_time();
        let threshold = Duration::from_secs(u64::from(config.minutes.max(1)) * 60);
        if self.idle_dimmed.is_some() {
            if !config.enabled || idle < threshold {
                self.wake_displays();
                self.apply_adaptive();
                return true;
            }
            return false;
        }
        if !config.enabled
            || idle < threshold
            || (config.respect_busy && crate::automation::screen_kept_on())
        {
            return false;
        }
        // Whatever was fading stays where it got to.
        self.freeze_fade();
        let level = config.level.min(100);
        let mut dimmed = Vec::new();
        for id in self.controllable_ids() {
            if self.slider_of(&id).is_some_and(|current| current > level) {
                self.write_level(&id, level);
                dimmed.push(id);
            }
        }
        crate::logger::info(format!("Idle: dimmed {} displays", dimmed.len()));
        self.idle_dimmed = Some(dimmed);
        true
    }

    /// Bring dimmed displays back to their remembered levels.
    fn wake_displays(&mut self) {
        for id in self.idle_dimmed.take().unwrap_or_default() {
            if let Some(slider) = self.settings.monitor(&id).last_brightness {
                self.write_level(&id, slider);
            }
        }
    }

    /// The user took over: stop fading and leave idle dimming.
    fn interrupt_automation(&mut self) {
        self.freeze_fade();
        self.wake_displays();
        // The user's own levels stay when the triggering app goes away.
        self.app_trigger = None;
    }

    /// Another app came to the foreground (`exe` like `game.exe`).
    pub fn on_foreground(&mut self, exe: &str, cx: &mut Context<Self>) {
        self.foreground = Some(exe.to_owned());
        // Applied with the first snapshot, once the displays are known.
        if !self.loaded {
            return;
        }
        let matched = self
            .settings
            .triggers
            .iter()
            .position(|trigger| trigger.matches_app(exe));
        let active = self.app_trigger.as_ref().map(|(index, _)| *index);
        if matched == active {
            return;
        }
        // Leaving a triggering app (for another one or none) restores the
        // levels from before the first trigger.
        let before = self.app_trigger.take().map(|(_, levels)| levels);
        match matched {
            Some(index) => {
                let levels = before.unwrap_or_else(|| self.remembered_levels());
                let profile = self.settings.triggers[index].profile;
                crate::logger::info(format!("Trigger: {exe} applies profile {profile}"));
                self.apply_profile(profile, cx);
                self.app_trigger = Some((index, levels));
            }
            None => {
                if let Some(levels) = before {
                    crate::logger::info(format!("Trigger: {exe} restores levels"));
                    self.restore_levels(&levels);
                    self.apply_adaptive();
                    self.save();
                    cx.notify();
                }
            }
        }
    }

    /// Triggers were edited: keep, move or end the app trigger in effect.
    fn triggers_changed(&mut self, before: &Settings, cx: &mut Context<Self>) {
        let exe = self.foreground.clone().unwrap_or_default();
        let Some((index, levels)) = self.app_trigger.take() else {
            // A new trigger for the app in front applies right away.
            self.on_foreground(&exe, cx);
            return;
        };
        let old_profile = before.triggers.get(index).map(|trigger| trigger.profile);
        match self
            .settings
            .triggers
            .iter()
            .position(|trigger| trigger.matches_app(&exe))
        {
            Some(matched) => {
                let profile = self.settings.triggers[matched].profile;
                if Some(profile) != old_profile {
                    self.apply_profile(profile, cx);
                }
                self.app_trigger = Some((matched, levels));
            }
            None => self.restore_levels(&levels),
        }
    }

    /// A new smoothed reading from the ambient light sensor.
    pub fn on_ambient_light(&mut self, lux: f32, cx: &mut Context<Self>) {
        self.ambient_lux = Some(lux);
        if self.apply_adaptive() {
            self.save();
            cx.notify();
        }
    }

    /// Set the displays from the last light reading, unless something else
    /// holds them. Returns whether any level changed.
    fn apply_adaptive(&mut self) -> bool {
        let config = self.settings.adaptive;
        let Some(lux) = self.ambient_lux else {
            return false;
        };
        // Dimmed for inactivity or held by an app trigger: those win.
        if !config.enabled
            || !self.loaded
            || self.idle_dimmed.is_some()
            || self.app_trigger.is_some()
        {
            return false;
        }
        let level = crate::light_sensor::lux_to_level(lux, config.dark_level, config.bright_level);
        let differ: Vec<String> = self
            .controllable_ids()
            .into_iter()
            .filter(|id| self.slider_of(id) != Some(level))
            .collect();
        if differ.is_empty() {
            return false;
        }
        crate::logger::info(format!("Adaptive brightness: {lux:.0} lux -> {level}"));
        // The sensor takes over from a schedule fade.
        self.freeze_fade();
        for id in differ {
            self.apply_level(&id, level);
        }
        true
    }

    /// The computer switched between battery and mains power.
    pub fn on_power_source(&mut self, on_battery: bool, cx: &mut Context<Self>) {
        // The state at startup is not a switch.
        if self
            .on_battery
            .replace(on_battery)
            .is_none_or(|was| was == on_battery)
        {
            return;
        }
        let wanted = if on_battery {
            crate::settings::TriggerCondition::OnBattery
        } else {
            crate::settings::TriggerCondition::OnAc
        };
        if let Some(profile) = self
            .settings
            .triggers
            .iter()
            .find(|trigger| trigger.condition == wanted)
            .map(|trigger| trigger.profile)
        {
            crate::logger::info(format!(
                "Trigger: power source (battery: {on_battery}) applies profile {profile}"
            ));
            self.apply_profile(profile, cx);
        }
    }

    /// The levels the user chose for every controllable display, as a
    /// profile would store them.
    fn remembered_levels(&self) -> BTreeMap<String, ProfileLevels> {
        self.controllable_ids()
            .into_iter()
            .map(|id| {
                let entry = self.settings.monitor(&id);
                let has_contrast = self
                    .monitors
                    .iter()
                    .any(|m| m.id == id && m.contrast.is_some());
                let levels = ProfileLevels {
                    brightness: entry.last_brightness.or_else(|| self.slider_of(&id)),
                    sdr: None,
                    contrast: entry.last_contrast.filter(|_| has_contrast),
                };
                (id, levels)
            })
            .collect()
    }

    fn restore_levels(&mut self, levels: &BTreeMap<String, ProfileLevels>) {
        let connected = self.controllable_ids();
        for (id, saved) in levels {
            if !connected.contains(id) {
                continue;
            }
            if let Some(slider) = saved.brightness {
                self.apply_level(id, slider);
            }
            if let Some(contrast) = saved.contrast {
                self.apply_contrast(id, contrast);
            }
        }
    }

    /// Stop a fade, remembering the levels it reached.
    fn freeze_fade(&mut self) {
        for step in self.fade.take().map(|fade| fade.steps).unwrap_or_default() {
            self.settings
                .monitors
                .entry(step.id)
                .or_default()
                .last_brightness = Some(step.current);
        }
    }

    /// Apply the schedule entry in effect if it is not applied yet. `live`
    /// fades the change in; startup and catching up after sleep apply at
    /// once.
    fn run_schedule(&mut self, live: bool) -> bool {
        let schedule = &self.settings.schedule;
        if !schedule.enabled || !self.loaded {
            self.schedule_applied = None;
            return false;
        }
        let Some((index, at)) = crate::schedule::active(
            &schedule.entries,
            &chrono::Local::now(),
            schedule.location(),
        ) else {
            return false;
        };
        let key = (index, at.timestamp());
        // While dimmed for inactivity or while an app trigger holds its
        // profile, the entry waits.
        if self.schedule_applied == Some(key)
            || self.idle_dimmed.is_some()
            || self.app_trigger.is_some()
        {
            return false;
        }
        let fade = if live && self.schedule_applied.is_some() {
            Duration::from_secs(u64::from(schedule.fade_seconds))
        } else {
            Duration::ZERO
        };
        let target = schedule.entries[index].target;
        crate::logger::info(format!("Schedule: applying entry {index} ({target:?})"));
        self.schedule_applied = Some(key);
        self.apply_schedule_target(target, fade);
        true
    }

    fn apply_schedule_target(&mut self, target: ScheduleTarget, fade: Duration) {
        self.freeze_fade();
        let mut levels: Vec<(String, u8)> = Vec::new();
        match target {
            ScheduleTarget::Level { level } => {
                for id in self.controllable_ids() {
                    levels.push((id, level.min(100)));
                }
            }
            ScheduleTarget::Profile { index } => {
                let Some(profile) = self.settings.profiles.get(index).cloned() else {
                    return;
                };
                let connected = self.controllable_ids();
                for (id, saved) in &profile.levels {
                    if let Some(slider) = saved.brightness.or(saved.sdr)
                        && connected.contains(id)
                    {
                        levels.push((id.clone(), slider.min(100)));
                    }
                    let has_contrast = self
                        .monitors
                        .iter()
                        .any(|m| m.id == *id && m.contrast.is_some() && !m.unresponsive);
                    if let Some(contrast) = saved.contrast.filter(|_| has_contrast) {
                        self.apply_contrast(id, contrast);
                    }
                }
            }
        }
        let mut steps = Vec::new();
        for (id, to) in levels {
            let from = match self.slider_of(&id) {
                Some(from) if !fade.is_zero() => from,
                _ => {
                    self.apply_level(&id, to);
                    continue;
                }
            };
            // Remember the destination: a restart mid-fade restores it.
            self.settings
                .monitors
                .entry(id.clone())
                .or_default()
                .last_brightness = Some(to);
            if from != to {
                steps.push(FadeStep {
                    id,
                    from,
                    to,
                    current: from,
                });
            }
        }
        if !steps.is_empty() {
            self.fade = Some(Fade {
                started: Instant::now(),
                duration: fade,
                steps,
            });
            self.step_fade();
        }
    }

    fn step_fade(&mut self) -> bool {
        let Some(fade) = &mut self.fade else {
            return false;
        };
        let progress = fade.started.elapsed().as_secs_f32() / fade.duration.as_secs_f32();
        let mut writes = Vec::new();
        for step in &mut fade.steps {
            let level = crate::schedule::blend(step.from, step.to, progress);
            if level != step.current {
                step.current = level;
                writes.push((step.id.clone(), level));
            }
        }
        if progress >= 1.0 {
            self.fade = None;
        }
        for (id, level) in &writes {
            self.write_level(id, *level);
        }
        !writes.is_empty()
    }

    /// Settings edits that start or stop automation take effect now.
    fn automation_changed(&mut self, before: &Settings, cx: &mut Context<Self>) {
        let (now, then) = (&self.settings.schedule, &before.schedule);
        if now != then {
            let stop = !now.enabled || now.entries.is_empty();
            // The entry in effect was edited or removed: apply what is in
            // effect now, even if it fired at the same moment.
            let edited = self
                .schedule_applied
                .is_some_and(|(index, _)| then.entries.get(index) != now.entries.get(index));
            // Nothing computable is in effect any more (e.g. a sun entry
            // lost its location): a fade toward the old target must stop.
            let none_active =
                crate::schedule::active(&now.entries, &chrono::Local::now(), now.location())
                    .is_none();
            if stop || none_active {
                self.freeze_fade();
                self.schedule_applied = None;
            }
            if edited {
                self.schedule_applied = Some((usize::MAX, 0));
            }
            self.run_schedule(true);
        }
        if self.settings.triggers != before.triggers {
            self.triggers_changed(before, cx);
        }
        if self.settings.adaptive != before.adaptive {
            self.apply_adaptive();
        }
        if before.idle_dim.enabled && !self.settings.idle_dim.enabled {
            self.wake_displays();
        }
    }

    /// Another thread changed the stored settings; nothing is persisted here.
    pub fn reload_settings(&mut self, cx: &mut Context<Self>) {
        cx.notify();
    }

    fn apply_runtime_effects(&self, before: &Settings) {
        let now = &self.settings;
        publish_hotkeys(now);
        crate::tray::set_scroll_enabled(now.tray_scroll);
        crate::light_sensor::set_enabled(now.adaptive.enabled);
        if now.update_interval_ms != before.update_interval_ms {
            self.service.send(Command::SetThrottle(throttle(now)));
        }
        for id in now.monitors.keys().chain(
            before
                .monitors
                .keys()
                .filter(|id| !now.monitors.contains_key(*id)),
        ) {
            let vcp = now.monitors.get(id).and_then(|entry| entry.brightness_vcp);
            let old = before
                .monitors
                .get(id)
                .and_then(|entry| entry.brightness_vcp);
            if vcp != old {
                self.service.send(Command::SetBrightnessVcp {
                    id: id.clone(),
                    vcp,
                });
            }
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

/// The value of a display's level slider: its SDR white level while HDR is
/// on, its normalized hardware brightness otherwise.
fn slider_level(monitor: &Monitor, settings: &MonitorSettings) -> Option<u8> {
    match monitor.sdr {
        Some(sdr) => Some(sdr.percent),
        None => monitor
            .brightness
            .map(|hardware| levels::from_hardware(hardware, settings.min, settings.max)),
    }
}

/// How long a local edit outranks hardware reports for the same display.
const LOCAL_EDIT_HOLD: Duration = Duration::from_millis(1500);

/// Hand every shortcut to the tray thread, which registers them.
fn publish_hotkeys(settings: &Settings) {
    crate::hotkey::set_desired(settings.hotkeys());
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
            // The slider drives SDR while HDR is on; the backlight stays put.
            let (brightness, sdr) = match monitor.sdr {
                Some(now) => (
                    None,
                    entry
                        .last_brightness
                        .filter(|slider| now.percent != *slider),
                ),
                None => (
                    entry
                        .last_brightness
                        .map(|slider| levels::to_hardware(slider, entry.min, entry.max))
                        .filter(|hardware| monitor.brightness.is_some_and(|now| now != *hardware)),
                    None,
                ),
            };
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
            inputs: Vec::new(),
            vcp_codes: Vec::new(),
            input: None,
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
    fn hdr_restores_the_slider_to_sdr_only() {
        let settings = settings_with(
            "a",
            MonitorSettings {
                last_brightness: Some(30),
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
