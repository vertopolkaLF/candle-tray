//! Persistent settings (`settings.toml` in the app's config directory).
//!
//! Every field applies live: changing a setting must update the running app
//! immediately, with no restart.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::monitors::{MonitorId, TurnOffMode};

pub const CURRENT_VERSION: u32 = 1;

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
    /// Hardware percent the slider's 0 maps to.
    pub min: u8,
    /// Hardware percent the slider's 100 maps to.
    pub max: u8,
    /// Last slider (normalized) brightness, restored on startup/resume.
    pub last_brightness: Option<u8>,
    /// Last SDR white level percent, restored the same way.
    pub last_sdr: Option<u8>,
}

impl Default for MonitorSettings {
    fn default() -> Self {
        Self {
            name: None,
            hidden: false,
            min: 0,
            max: 100,
            last_brightness: None,
            last_sdr: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub version: u32,
    pub language: Language,
    pub theme: Theme,
    pub popup_material: PopupMaterial,
    pub launch_at_startup: bool,
    /// Re-apply the last levels on startup, resume and display changes.
    pub restore_levels: bool,
    /// One "All displays" slider instead of one per monitor.
    pub link_levels: bool,
    pub turn_off: TurnOffSetting,
    /// Minimum interval between hardware writes while dragging.
    pub update_interval_ms: u32,
    /// Monitor ids in display order; unknown monitors go last by `num`.
    pub order: Vec<MonitorId>,
    pub monitors: BTreeMap<MonitorId, MonitorSettings>,
    /// Set once the Twinkle Tray import prompt was answered either way.
    pub twinkle_import_done: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            language: Language::System,
            theme: Theme::Auto,
            popup_material: PopupMaterial::Acrylic,
            launch_at_startup: true,
            restore_levels: true,
            link_levels: false,
            turn_off: TurnOffSetting::Windows,
            update_interval_ms: 100,
            order: Vec::new(),
            monitors: BTreeMap::new(),
            twinkle_import_done: false,
        }
    }
}

impl Settings {
    /// `%APPDATA%\Candle Tray\settings.toml`.
    pub fn default_path() -> Result<PathBuf> {
        todo!("settings path")
    }

    pub fn load_or_create(path: &Path) -> Result<Self> {
        let _ = path;
        todo!("settings load")
    }

    /// Atomic write (temp file + rename).
    pub fn save(&self, path: &Path) -> Result<()> {
        let _ = path;
        todo!("settings save")
    }

    pub fn monitor(&self, id: &str) -> MonitorSettings {
        self.monitors.get(id).cloned().unwrap_or_default()
    }
}
