#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use std::sync::Arc;

use anyhow::Result;
use candle_tray::{
    app::{AppModel, PersistFn},
    monitors::{Command, MonitorService},
    popup::{self, PopupCommand},
    settings::Settings,
};
use gpui::AppContext;

fn run() -> Result<()> {
    let path = Settings::default_path()?;
    let settings = Settings::load_or_create(&path)?;
    candle_tray::i18n::set_language(settings.language);
    popup::apply_popup_appearance(settings.popup_material);
    if let Err(error) = candle_tray::autostart::set_enabled(settings.launch_at_startup) {
        candle_tray::logger::error(format!("could not apply startup registration: {error:#}"));
    }

    // Hardware events hop onto the GPUI thread through the popup channel.
    let service = Arc::new(MonitorService::start(|event| {
        popup::send_command(PopupCommand::Monitors(event));
    }));

    let persist_path = path.clone();
    let persist: PersistFn = Arc::new(move |settings: &Settings| {
        if let Err(error) = settings.save(&persist_path) {
            candle_tray::logger::error(format!("could not save settings: {error:#}"));
        }
    });

    let model_service = Arc::clone(&service);
    popup::ui::start(move |cx| {
        let model = cx.new(|_| AppModel::new(settings, model_service, persist));
        // Keep the tray tooltip on the current average level.
        cx.observe(&model, |model, cx| {
            candle_tray::tray::set_tooltip_level(model.read(cx).average_level());
        })
        .detach();
        model
    });

    let refresh_service = Arc::clone(&service);
    candle_tray::tray::run(
        move || refresh_service.send(Command::Refresh),
        move || service.send(Command::Shutdown),
    )
}

fn main() {
    if let Err(error) = run() {
        candle_tray::logger::error(format!("Candle Tray failed to start: {error:#}"));
        std::process::exit(1);
    }
}
