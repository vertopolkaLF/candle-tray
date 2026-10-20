//! TEMPORARY stub; replaced by the Fluent implementation on merge.
use crate::settings::Language;

pub fn tr(key: &str) -> &'static str {
    match key {
        "app-name" => "Candle Tray",
        "tray-refresh" => "Refresh displays",
        "tray-settings" => "Settings",
        "tray-exit" => "Exit",
        "popup-all-displays" => "All displays",
        "popup-no-displays" => "No displays found",
        "popup-loading" => "Looking for displays…",
        "popup-unresponsive" => "Not responding",
        "popup-unsupported" => "Brightness can't be changed",
        "popup-sdr" => "SDR brightness",
        _ => "?",
    }
}

pub fn format(key: &str, args: &[(&str, String)]) -> String {
    let _ = args;
    tr(key).to_owned()
}

pub fn set_language(language: Language) {
    let _ = language;
}
