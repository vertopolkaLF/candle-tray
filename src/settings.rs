//! Persistent settings (`settings.toml` in the app's config directory).
//!
//! Every field applies live: changing a setting must update the running app
//! immediately, with no restart.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    sync::{Arc, Mutex, mpsc},
    thread,
};

use crate::hotkey::{Action, Hotkey};
use crate::monitors::{MonitorId, TurnOffMode};

pub const CURRENT_VERSION: u32 = 1;
pub const SLIDER_STEPS: [u8; 4] = [1, 2, 5, 10];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    #[default]
    System,
    En,
    Ru,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    #[default]
    Auto,
    Light,
    Dark,
}

/// Popup backdrop. Acrylic and Mica need Windows 11; Windows 10 falls back
/// to `Solid`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PopupMaterial {
    #[default]
    Acrylic,
    Mica,
    Solid,
}

/// How much the wallpaper lightens the dark Acrylic or Mica popup.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackdropBrightness {
    Darker,
    #[default]
    Normal,
    Brighter,
}

/// Glyph shown in the notification area.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrayIcon {
    #[default]
    Fire,
    Sun,
    BrightnessHigh,
    BrightnessLow,
    Lightbulb,
}

impl TrayIcon {
    pub const ALL: [Self; 5] = [
        Self::Fire,
        Self::Sun,
        Self::BrightnessHigh,
        Self::BrightnessLow,
        Self::Lightbulb,
    ];
}

/// Glyph of a level profile in the popup's bottom bar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileIcon {
    #[default]
    Sun,
    Moon,
    Lightbulb,
    Book,
    Movie,
    Game,
    Code,
    Work,
    Coffee,
    Home,
    Star,
    Heart,
}

impl ProfileIcon {
    pub const ALL: [Self; 12] = [
        Self::Sun,
        Self::Moon,
        Self::Lightbulb,
        Self::Book,
        Self::Movie,
        Self::Game,
        Self::Code,
        Self::Work,
        Self::Coffee,
        Self::Home,
        Self::Star,
        Self::Heart,
    ];
}

/// Levels a profile stores for one display; `None` leaves that level alone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProfileLevels {
    /// Slider (normalized) brightness.
    pub brightness: Option<u8>,
    /// SDR white level percent.
    pub sdr: Option<u8>,
    /// Contrast percent.
    pub contrast: Option<u8>,
}

/// Saved levels applied in one click from the popup's bottom bar.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    /// User-chosen name; empty shows a numbered default.
    pub name: String,
    pub icon: ProfileIcon,
    /// System-wide shortcut that applies the profile.
    pub hotkey: Option<crate::hotkey::Hotkey>,
    pub levels: BTreeMap<MonitorId, ProfileLevels>,
}

/// System-wide shortcuts that are not tied to a profile.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Shortcuts {
    /// Raise every visible display by [`Settings::shortcut_step`].
    pub brightness_up: Option<crate::hotkey::Hotkey>,
    /// Lower every visible display by [`Settings::shortcut_step`].
    pub brightness_down: Option<crate::hotkey::Hotkey>,
    /// Turn the displays off the way [`Settings::turn_off`] says.
    pub turn_off: Option<crate::hotkey::Hotkey>,
}

/// Dim the displays after a while without keyboard or mouse input.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct IdleDim {
    pub enabled: bool,
    /// Minutes without input before dimming.
    pub minutes: u16,
    /// Slider level the displays dim to; darker displays stay as they are.
    pub level: u8,
    /// Stay bright while an app keeps the screen on (video, presentations)
    /// or runs full screen.
    pub respect_busy: bool,
}

impl Default for IdleDim {
    fn default() -> Self {
        Self {
            enabled: false,
            minutes: 5,
            level: 10,
            respect_busy: true,
        }
    }
}

/// When a schedule entry fires.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "at", rename_all = "snake_case")]
pub enum ScheduleTime {
    /// Minutes after local midnight.
    Clock { minutes: u16 },
    /// Minutes after (or before, when negative) local sunrise.
    Sunrise { offset: i16 },
    /// Minutes after (or before, when negative) local sunset.
    Sunset { offset: i16 },
}

impl Default for ScheduleTime {
    fn default() -> Self {
        Self::Clock { minutes: 8 * 60 }
    }
}

/// What a schedule entry applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScheduleTarget {
    /// One slider level for every visible display.
    Level { level: u8 },
    /// A profile, by its index in [`Settings::profiles`].
    Profile { index: usize },
}

impl Default for ScheduleTarget {
    fn default() -> Self {
        Self::Level { level: 80 }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScheduleEntry {
    pub time: ScheduleTime,
    pub target: ScheduleTarget,
}

/// Follow the ambient light sensor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AdaptiveBrightness {
    pub enabled: bool,
    /// Slider level in a dark room (about 5 lux).
    pub dark_level: u8,
    /// Slider level in bright light (about 1000 lux and more).
    pub bright_level: u8,
}

impl Default for AdaptiveBrightness {
    fn default() -> Self {
        Self {
            enabled: false,
            dark_level: 20,
            bright_level: 100,
        }
    }
}

/// What switches a profile on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "when", rename_all = "snake_case")]
pub enum TriggerCondition {
    /// An app comes to the foreground; leaving it restores the levels from
    /// before. `exe` is the image file name, like `game.exe`.
    App { exe: String },
    /// The computer switches to battery power.
    OnBattery,
    /// The computer is plugged in.
    OnAc,
}

impl Default for TriggerCondition {
    fn default() -> Self {
        Self::App { exe: String::new() }
    }
}

/// Applies a profile when its condition comes true.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProfileTrigger {
    pub condition: TriggerCondition,
    /// Index in [`Settings::profiles`].
    pub profile: usize,
}

impl ProfileTrigger {
    /// The trigger is about this foreground app (case-insensitive).
    pub fn matches_app(&self, exe: &str) -> bool {
        match &self.condition {
            TriggerCondition::App { exe: wanted } => {
                let wanted = wanted.trim();
                !wanted.is_empty() && wanted.eq_ignore_ascii_case(exe.trim())
            }
            _ => false,
        }
    }
}

/// Levels that change by time of day.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Schedule {
    pub enabled: bool,
    pub entries: Vec<ScheduleEntry>,
    /// Needed for sunrise and sunset entries.
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    /// How long a change that fires while the app runs takes to fade in.
    pub fade_seconds: u32,
}

impl Default for Schedule {
    fn default() -> Self {
        Self {
            enabled: false,
            entries: Vec::new(),
            latitude: None,
            longitude: None,
            fade_seconds: 60,
        }
    }
}

impl Schedule {
    pub fn location(&self) -> Option<(f64, f64)> {
        let (latitude, longitude) = (self.latitude?, self.longitude?);
        ((-90.0..=90.0).contains(&latitude) && (-180.0..=180.0).contains(&longitude))
            .then_some((latitude, longitude))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOffSetting {
    #[default]
    Windows,
    Ddc,
    Both,
}

impl From<TurnOffSetting> for TurnOffMode {
    fn from(value: TurnOffSetting) -> Self {
        match value {
            TurnOffSetting::Windows => TurnOffMode::Windows,
            TurnOffSetting::Ddc => TurnOffMode::Ddc,
            TurnOffSetting::Both => TurnOffMode::Both,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MonitorSettings {
    /// User-chosen display name; `None` shows the hardware name.
    pub name: Option<String>,
    pub hidden: bool,
    /// Preferred DDC/CI brightness VCP code; None uses automatic detection.
    /// Unanswered overrides fall back to the normal brightness candidates.
    pub brightness_vcp: Option<u8>,
    /// Hardware percent the slider's 0 maps to.
    pub min: u8,
    /// Hardware percent the slider's 100 maps to.
    pub max: u8,
    /// Last level slider value, restored on startup/resume and carried
    /// across HDR switches: normalized brightness, or the SDR white level
    /// while HDR is on.
    pub last_brightness: Option<u8>,
    /// Show a contrast slider in the flyout (DDC/CI displays only).
    pub show_contrast: bool,
    /// Last contrast percent, restored the same way.
    pub last_contrast: Option<u8>,
    /// Show input source buttons in the flyout (DDC/CI VCP 0x60).
    pub show_inputs: bool,
    /// User names of input sources, keyed by the VCP 0x60 value in hex
    /// ("0F"); TOML keys must be strings.
    pub input_names: BTreeMap<String, String>,
    /// Input sources left out of the flyout.
    pub hidden_inputs: Vec<u8>,
}

impl Default for MonitorSettings {
    fn default() -> Self {
        Self {
            name: None,
            hidden: false,
            brightness_vcp: None,
            min: 0,
            max: 100,
            last_brightness: None,
            show_contrast: false,
            last_contrast: None,
            show_inputs: false,
            input_names: BTreeMap::new(),
            hidden_inputs: Vec::new(),
        }
    }
}

impl MonitorSettings {
    /// Key of an input source in [`Self::input_names`].
    pub fn input_key(value: u8) -> String {
        format!("{value:02X}")
    }

    /// The user's name for an input source, or its standard name.
    pub fn input_name(&self, value: u8) -> String {
        self.input_names
            .get(&Self::input_key(value))
            .map(|name| name.trim())
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| crate::monitors::input_label(value))
    }

    /// Input sources the flyout shows, in the display's order.
    pub fn visible_inputs(&self, inputs: &[u8]) -> Vec<u8> {
        inputs
            .iter()
            .copied()
            .filter(|value| !self.hidden_inputs.contains(value))
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub version: u32,
    pub language: Language,
    pub theme: Theme,
    pub popup_material: PopupMaterial,
    pub backdrop_brightness: BackdropBrightness,
    pub tray_icon: TrayIcon,
    pub launch_at_startup: bool,
    /// Re-apply the last levels on startup, resume and display changes.
    pub restore_levels: bool,
    /// One "All displays" slider instead of one per monitor.
    pub link_levels: bool,
    pub turn_off: TurnOffSetting,
    /// Minimum interval between hardware writes while dragging.
    pub update_interval_ms: u32,
    /// Percent increment for level sliders and mouse-wheel adjustments.
    pub slider_step: u8,
    /// Monitor ids in display order; unknown monitors go last by `num`.
    pub order: Vec<MonitorId>,
    pub monitors: BTreeMap<MonitorId, MonitorSettings>,
    /// Set once the Twinkle Tray import prompt was answered either way.
    pub twinkle_import_done: bool,
    /// Level profiles in bottom-bar order.
    #[serde(alias = "templates")]
    pub profiles: Vec<Profile>,
    pub shortcuts: Shortcuts,
    /// Percent the brightness shortcuts move every display by.
    pub shortcut_step: u8,
    /// Scrolling over the tray icon changes every visible display.
    pub tray_scroll: bool,
    pub idle_dim: IdleDim,
    pub schedule: Schedule,
    /// Profiles applied by the foreground app or the power source.
    pub triggers: Vec<ProfileTrigger>,
    pub adaptive: AdaptiveBrightness,
    /// Look for a new release at startup and every hour.
    pub check_for_updates: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            language: Language::System,
            theme: Theme::Auto,
            popup_material: PopupMaterial::Acrylic,
            backdrop_brightness: BackdropBrightness::Normal,
            tray_icon: TrayIcon::Fire,
            launch_at_startup: true,
            restore_levels: true,
            link_levels: false,
            turn_off: TurnOffSetting::Windows,
            update_interval_ms: 100,
            slider_step: 1,
            order: Vec::new(),
            monitors: BTreeMap::new(),
            twinkle_import_done: false,
            profiles: Vec::new(),
            shortcuts: Shortcuts::default(),
            shortcut_step: 10,
            tray_scroll: true,
            idle_dim: IdleDim::default(),
            schedule: Schedule::default(),
            triggers: Vec::new(),
            adaptive: AdaptiveBrightness::default(),
            check_for_updates: true,
        }
    }
}

impl Settings {
    /// Ignore unsupported hand-edited values without losing other settings.
    pub fn slider_step(&self) -> u8 {
        if SLIDER_STEPS.contains(&self.slider_step) {
            self.slider_step
        } else {
            1
        }
    }

    /// `%APPDATA%\Candle Tray\settings.toml`.
    pub fn default_path() -> Result<PathBuf> {
        let dirs = directories::ProjectDirs::from("", "", "Candle Tray")
            .context("resolve Candle Tray config directory")?;
        // directories adds a Windows-only "config" suffix. The product path is
        // directly under APPDATA, so deliberately use its application parent.
        #[cfg(windows)]
        let directory = dirs
            .config_dir()
            .parent()
            .context("application config parent")?;
        #[cfg(not(windows))]
        let directory = dirs.config_dir();
        Ok(directory.join("settings.toml"))
    }

    pub fn load_or_create(path: &Path) -> Result<Self> {
        let mut settings = match fs::read(path) {
            Ok(raw) => match std::str::from_utf8(&raw)
                .context("settings UTF-8")
                .and_then(|raw| toml::from_str::<Self>(raw).context("decode settings"))
            {
                Ok(settings) => settings,
                Err(error) => {
                    let stamp = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
                    let backup = path.with_extension(format!("toml.bad-{stamp}"));
                    fs::rename(path, &backup).context("back up corrupt settings")?;
                    crate::logger::error(format!(
                        "Invalid settings ({error}); backed up to {}",
                        backup.display()
                    ));
                    let settings = Self::default();
                    settings.save(path)?;
                    return Ok(settings);
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let settings = Self::default();
                settings.save(path)?;
                return Ok(settings);
            }
            Err(error) => return Err(error).context("read settings"),
        };
        if settings.migrate()? {
            settings.save(path)?;
        }
        Ok(settings)
    }

    /// Parse a settings file exported by this or an older version.
    pub fn from_toml(raw: &str) -> Result<Self> {
        let mut settings: Self = toml::from_str(raw).context("decode settings")?;
        settings.migrate()?;
        Ok(settings)
    }

    /// Atomic write (temp file + rename).
    pub fn save(&self, path: &Path) -> Result<()> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent)?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(toml::to_string_pretty(self)?.as_bytes())?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(path)
            .with_context(|| format!("commit {}", path.display()))?;
        Ok(())
    }

    /// Migration entry point. Preserve future versions rather than silently downgrading.
    fn migrate(&mut self) -> Result<bool> {
        if self.version < CURRENT_VERSION {
            self.version = CURRENT_VERSION;
            return Ok(true);
        }
        Ok(false)
    }

    pub fn monitor(&self, id: &str) -> MonitorSettings {
        self.monitors.get(id).cloned().unwrap_or_default()
    }

    /// Every shortcut slot: one per profile, then the fixed actions.
    pub fn hotkeys(&self) -> Vec<(Action, Option<Hotkey>)> {
        let mut hotkeys: Vec<_> = self
            .profiles
            .iter()
            .enumerate()
            .map(|(index, profile)| (Action::Profile(index), profile.hotkey))
            .collect();
        hotkeys.extend([
            (Action::BrightnessUp, self.shortcuts.brightness_up),
            (Action::BrightnessDown, self.shortcuts.brightness_down),
            (Action::TurnOff, self.shortcuts.turn_off),
        ]);
        hotkeys
    }

    pub fn hotkeys_mut(&mut self) -> Vec<(Action, &mut Option<Hotkey>)> {
        let mut hotkeys: Vec<_> = self
            .profiles
            .iter_mut()
            .enumerate()
            .map(|(index, profile)| (Action::Profile(index), &mut profile.hotkey))
            .collect();
        hotkeys.extend([
            (Action::BrightnessUp, &mut self.shortcuts.brightness_up),
            (Action::BrightnessDown, &mut self.shortcuts.brightness_down),
            (Action::TurnOff, &mut self.shortcuts.turn_off),
        ]);
        hotkeys
    }

    /// Move a profile; schedule entries follow it to its new place.
    pub fn move_profile(&mut self, from: usize, to: usize) {
        let count = self.profiles.len();
        if from >= count || to >= count || from == to {
            return;
        }
        let moved = self.profiles.remove(from);
        self.profiles.insert(to, moved);
        // `order[new] = old`.
        let mut order: Vec<usize> = (0..count).collect();
        let old = order.remove(from);
        order.insert(to, old);
        for entry in &mut self.schedule.entries {
            if let ScheduleTarget::Profile { index } = &mut entry.target
                && let Some(new) = order.iter().position(|old| old == index)
            {
                *index = new;
            }
        }
        for trigger in &mut self.triggers {
            if let Some(new) = order.iter().position(|old| *old == trigger.profile) {
                trigger.profile = new;
            }
        }
    }

    /// Delete a profile and the schedule entries that apply it.
    pub fn delete_profile(&mut self, index: usize) {
        if index >= self.profiles.len() {
            return;
        }
        self.profiles.remove(index);
        self.schedule
            .entries
            .retain(|entry| entry.target != ScheduleTarget::Profile { index });
        for entry in &mut self.schedule.entries {
            if let ScheduleTarget::Profile { index: other } = &mut entry.target
                && *other > index
            {
                *other -= 1;
            }
        }
        self.triggers.retain(|trigger| trigger.profile != index);
        for trigger in &mut self.triggers {
            if trigger.profile > index {
                trigger.profile -= 1;
            }
        }
    }
}

type Subscriber = Arc<dyn Fn(&Settings) + Send + Sync>;
enum WriteRequest {
    Save(Settings),
    Flush(mpsc::Sender<std::result::Result<(), String>>),
}
struct StoreState {
    settings: Settings,
    subscribers: BTreeMap<u64, Subscriber>,
    next_id: u64,
}
struct StoreInner {
    state: Mutex<StoreState>,
    sender: Option<mpsc::Sender<WriteRequest>>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Drop for StoreInner {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Serial background persistence. Clones share one immediate in-memory snapshot.
/// Callbacks run on the editing thread, outside all locks; they must be cheap.
/// Concurrent edits may deliver callbacks out of order: read `snapshot()` when
/// scheduling UI work. Dropping the final store drains and joins the writer.
#[derive(Clone)]
pub struct SettingsStore(Arc<StoreInner>);
impl SettingsStore {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let settings = Settings::load_or_create(&path)?;
        let (sender, receiver) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("settings-writer".into())
            .spawn(move || {
                let mut pending = None;
                let mut last_error: Option<String> = None;
                let persist = |pending: &mut Option<Settings>, last_error: &mut Option<String>| {
                    if let Some(settings) = pending.take() {
                        *last_error = settings.save(&path).err().map(|error| format!("{error:#}"));
                        if let Some(error) = last_error {
                            crate::logger::error(error);
                        }
                    }
                };
                while let Ok(request) = receiver.recv() {
                    match request {
                        WriteRequest::Save(settings) => {
                            pending = Some(settings);
                            // A short quiet period coalesces slider bursts, with a hard
                            // deadline so continuous dragging still reaches disk.
                            let deadline =
                                std::time::Instant::now() + std::time::Duration::from_millis(100);
                            while std::time::Instant::now() < deadline {
                                match receiver.recv_timeout(std::time::Duration::from_millis(20)) {
                                    Ok(WriteRequest::Save(settings)) => pending = Some(settings),
                                    Ok(WriteRequest::Flush(reply)) => {
                                        persist(&mut pending, &mut last_error);
                                        let _ = reply.send(last_error.clone().map_or(Ok(()), Err));
                                        break;
                                    }
                                    Err(_) => break,
                                }
                            }
                            persist(&mut pending, &mut last_error);
                        }
                        WriteRequest::Flush(reply) => {
                            persist(&mut pending, &mut last_error);
                            let _ = reply.send(last_error.clone().map_or(Ok(()), Err));
                        }
                    }
                }
            })?;
        Ok(Self(Arc::new(StoreInner {
            state: Mutex::new(StoreState {
                settings,
                subscribers: BTreeMap::new(),
                next_id: 0,
            }),
            sender: Some(sender),
            worker: Some(worker),
        })))
    }
    pub fn snapshot(&self) -> Settings {
        self.0.state.lock().expect("settings lock").settings.clone()
    }
    /// Updates and queues atomically with respect to other edits. Do not call
    /// store methods from inside `edit`; subscriber callbacks may re-enter.
    pub fn edit(&self, edit: impl FnOnce(&mut Settings)) {
        let (snapshot, subscribers) = {
            let mut state = self.0.state.lock().expect("settings lock");
            edit(&mut state.settings);
            let snapshot = state.settings.clone();
            if self
                .0
                .sender
                .as_ref()
                .expect("writer")
                .send(WriteRequest::Save(snapshot.clone()))
                .is_err()
            {
                crate::logger::error("Settings writer disconnected");
            }
            (
                snapshot,
                state.subscribers.values().cloned().collect::<Vec<_>>(),
            )
        };
        for subscriber in subscribers {
            subscriber(&snapshot);
        }
    }
    pub fn subscribe(&self, callback: impl Fn(&Settings) + Send + Sync + 'static) -> u64 {
        let mut state = self.0.state.lock().expect("settings lock");
        let id = state.next_id;
        state.next_id += 1;
        state.subscribers.insert(id, Arc::new(callback));
        id
    }
    pub fn unsubscribe(&self, id: u64) {
        self.0
            .state
            .lock()
            .expect("settings lock")
            .subscribers
            .remove(&id);
    }
    /// Waits for all previously queued changes and reports persistence errors.
    pub fn flush(&self) -> Result<()> {
        let (sender, receiver) = mpsc::channel();
        self.0
            .sender
            .as_ref()
            .expect("writer")
            .send(WriteRequest::Flush(sender))?;
        receiver.recv()?.map_err(anyhow::Error::msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn round_trip_and_atomic_replace() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        let mut settings = Settings::load_or_create(&path).unwrap();
        settings.monitors.insert(
            "instance".into(),
            MonitorSettings {
                name: Some("Рабочий".into()),
                min: 20,
                brightness_vcp: Some(0x13),
                ..Default::default()
            },
        );
        settings.save(&path).unwrap();
        assert_eq!(Settings::load_or_create(&path).unwrap(), settings);
    }
    #[test]
    fn missing_fields_use_defaults() {
        let settings: Settings = toml::from_str("[monitors.a]\nname = 'Desk'").unwrap();
        assert_eq!(settings.update_interval_ms, 100);
        assert_eq!(settings.slider_step(), 1);
        assert_eq!(settings.monitors["a"].max, 100);
        assert_eq!(settings.monitors["a"].brightness_vcp, None);
        assert!(settings.restore_levels);
        assert!(settings.tray_scroll);
        assert_eq!(settings.shortcut_step, 10);
        assert!(!settings.idle_dim.enabled && !settings.schedule.enabled);
    }
    #[test]
    fn slider_steps_round_trip_and_invalid_values_fall_back() {
        for step in SLIDER_STEPS {
            let settings = Settings {
                slider_step: step,
                ..Default::default()
            };
            let raw = toml::to_string(&settings).unwrap();
            assert_eq!(
                toml::from_str::<Settings>(&raw).unwrap().slider_step(),
                step
            );
        }
        for step in [0, 3, 255] {
            let settings: Settings = toml::from_str(&format!("slider_step = {step}")).unwrap();
            assert_eq!(settings.slider_step(), 1);
        }
    }
    #[test]
    fn profiles_round_trip() {
        let mut settings = Settings::default();
        settings.profiles.push(Profile {
            name: "Night".into(),
            icon: ProfileIcon::Moon,
            hotkey: Some(crate::hotkey::Hotkey {
                ctrl: true,
                alt: true,
                shift: false,
                win: false,
                key: 0x31,
            }),
            levels: BTreeMap::from([(
                r"\?\DISPLAY#DEL#1".to_owned(),
                ProfileLevels {
                    brightness: Some(30),
                    sdr: None,
                    contrast: Some(55),
                },
            )]),
        });
        let raw = toml::to_string_pretty(&settings).unwrap();
        assert_eq!(toml::from_str::<Settings>(&raw).unwrap(), settings);
    }
    #[test]
    fn automation_settings_round_trip() {
        let mut settings = Settings::default();
        settings.schedule.enabled = true;
        settings.schedule.latitude = Some(55.75);
        settings.schedule.longitude = Some(37.62);
        settings.schedule.entries = vec![
            ScheduleEntry {
                time: ScheduleTime::Clock { minutes: 7 * 60 },
                target: ScheduleTarget::Level { level: 70 },
            },
            ScheduleEntry {
                time: ScheduleTime::Sunset { offset: -30 },
                target: ScheduleTarget::Profile { index: 1 },
            },
        ];
        settings.shortcuts.turn_off = Some(crate::hotkey::Hotkey {
            ctrl: true,
            alt: false,
            shift: true,
            win: false,
            key: 0x4F,
        });
        settings.idle_dim.enabled = true;
        settings
            .monitors
            .insert("a".into(), MonitorSettings::default());
        let monitor = settings.monitors.get_mut("a").unwrap();
        monitor.show_inputs = true;
        monitor
            .input_names
            .insert(MonitorSettings::input_key(0x0f), "Dock".into());
        monitor.hidden_inputs.push(0x12);
        assert_eq!(monitor.input_name(0x0f), "Dock");
        assert_eq!(monitor.input_name(0x11), "HDMI 1");
        assert_eq!(monitor.visible_inputs(&[0x0f, 0x11, 0x12]), [0x0f, 0x11]);
        let raw = toml::to_string_pretty(&settings).unwrap();
        assert_eq!(toml::from_str::<Settings>(&raw).unwrap(), settings);
        assert_eq!(settings.schedule.location(), Some((55.75, 37.62)));
    }
    #[test]
    fn schedule_entries_follow_moved_and_deleted_profiles() {
        let mut settings = Settings {
            profiles: vec![Profile::default(), Profile::default(), Profile::default()],
            ..Default::default()
        };
        let target = |index| ScheduleEntry {
            time: ScheduleTime::default(),
            target: ScheduleTarget::Profile { index },
        };
        settings.schedule.entries = vec![target(0), target(1), target(2)];
        settings.triggers = (0..3)
            .map(|profile| ProfileTrigger {
                condition: TriggerCondition::OnBattery,
                profile,
            })
            .collect();
        settings.move_profile(0, 2);
        let indices = |settings: &Settings| {
            settings
                .schedule
                .entries
                .iter()
                .map(|entry| match entry.target {
                    ScheduleTarget::Profile { index } => index,
                    ScheduleTarget::Level { .. } => usize::MAX,
                })
                .collect::<Vec<_>>()
        };
        let triggers = |settings: &Settings| {
            settings
                .triggers
                .iter()
                .map(|trigger| trigger.profile)
                .collect::<Vec<_>>()
        };
        assert_eq!(indices(&settings), [2, 0, 1]);
        assert_eq!(triggers(&settings), [2, 0, 1]);
        settings.delete_profile(0);
        assert_eq!(indices(&settings), [1, 0]);
        assert_eq!(triggers(&settings), [1, 0]);
        let app = ProfileTrigger {
            condition: TriggerCondition::App {
                exe: " Game.EXE ".into(),
            },
            profile: 0,
        };
        assert!(app.matches_app("game.exe"));
        assert!(!app.matches_app("other.exe"));
        assert!(!ProfileTrigger::default().matches_app(""));
    }
    #[test]
    fn templates_from_earlier_builds_load_as_profiles() {
        let settings: Settings = toml::from_str(
            "[[templates]]
icon = 'moon'
[templates.levels.a]
brightness = 40",
        )
        .unwrap();
        assert_eq!(settings.profiles[0].icon, ProfileIcon::Moon);
        assert_eq!(settings.profiles[0].levels["a"].brightness, Some(40));
        assert_eq!(settings.profiles[0].hotkey, None);
    }
    #[test]
    fn corrupt_settings_are_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        fs::write(&path, "[broken").unwrap();
        assert_eq!(
            Settings::load_or_create(&path).unwrap(),
            Settings::default()
        );
        let backup = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .find(|e| e.file_name().to_string_lossy().contains(".bad-"))
            .unwrap();
        assert_eq!(fs::read_to_string(backup.path()).unwrap(), "[broken");
    }
    #[test]
    fn invalid_utf8_is_recovered_and_old_versions_upgrade() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        fs::write(&path, [0xff, 0xfe]).unwrap();
        assert_eq!(
            Settings::load_or_create(&path).unwrap(),
            Settings::default()
        );
        fs::write(&path, "version = 0\nlink_levels = true").unwrap();
        let settings = Settings::load_or_create(&path).unwrap();
        assert_eq!(settings.version, CURRENT_VERSION);
        assert!(settings.link_levels);
    }
    #[cfg(windows)]
    #[test]
    fn default_path_is_directly_under_roaming_appdata() {
        assert_eq!(
            Settings::default_path().unwrap(),
            directories::BaseDirs::new()
                .unwrap()
                .config_dir()
                .join("Candle Tray")
                .join("settings.toml")
        );
    }
    #[test]
    fn writer_reports_failure_while_memory_stays_live() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        let store = SettingsStore::open(&path).unwrap();
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        store.edit(|s| s.link_levels = true);
        assert!(store.snapshot().link_levels);
        assert!(store.flush().is_err());
        fs::remove_dir(&path).unwrap();
        store.edit(|s| s.update_interval_ms = 120);
        store.flush().unwrap();
        assert!(Settings::load_or_create(&path).unwrap().link_levels);
    }
    #[test]
    fn store_notifies_immediately_and_flushes_latest_edit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        let store = SettingsStore::open(&path).unwrap();
        let (tx, rx) = mpsc::channel();
        let id = store.subscribe(move |settings| {
            tx.send(settings.update_interval_ms).unwrap();
        });
        for value in 1..=50 {
            store.edit(|s| s.update_interval_ms = value);
        }
        assert_eq!(store.snapshot().update_interval_ms, 50);
        assert_eq!(rx.try_iter().count(), 50);
        store.unsubscribe(id);
        store.flush().unwrap();
        assert_eq!(
            Settings::load_or_create(&path).unwrap().update_interval_ms,
            50
        );
        store.edit(|s| s.link_levels = true);
        drop(store);
        assert!(Settings::load_or_create(&path).unwrap().link_levels);
    }
}
