#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use std::sync::Arc;

use anyhow::Result;
use candle_tray::{
    app::{AppModel, PersistFn},
    logger,
    monitors::{Command, MonitorService},
    popup::{self, PopupCommand},
    settings::{Settings, SettingsStore},
    single_instance, twinkle_import,
};
use gpui::AppContext;

fn run() -> Result<()> {
    let path = Settings::default_path()?;
    logger::attach(&path)?;
    let command = candle_tray::cli::parse(
        std::env::args_os()
            .skip(1)
            .map(|arg| {
                arg.into_string()
                    .map_err(|_| anyhow::anyhow!("CLI arguments must be valid Unicode"))
            })
            .collect::<Result<Vec<_>>>()?,
    )?;
    let Some(instance) = single_instance::acquire(command.as_ref())? else {
        return Ok(());
    };
    logger::initialize(&path)?;
    // Queue the initial command before installing the hook, preserving order
    // when second launches arrive during startup.
    if let Some(command) = command {
        popup::send_command(PopupCommand::Cli(command));
    }
    instance.set_activation_callback(|command| match command {
        Some(command) => popup::send_command(PopupCommand::Cli(command)),
        None => popup::show_near_cursor(),
    });

    // Probe for a light sensor off the UI thread, so Settings knows at once.
    std::thread::spawn(candle_tray::light_sensor::available);
    let store = SettingsStore::open(&path)?;
    store.edit(|settings| {
        if let Err(error) = candle_tray::autostart::reconcile(settings) {
            logger::error(format!(
                "could not reconcile startup registration: {error:#}"
            ));
        }
    });
    let settings = store.snapshot();
    candle_tray::i18n::set_language(settings.language);
    popup::apply_popup_appearance(settings.popup_material);
    candle_tray::tray::set_icon(settings.tray_icon);
    if let Err(error) = candle_tray::autostart::set_enabled(settings.launch_at_startup) {
        logger::error(format!("could not apply startup registration: {error:#}"));
    }
    let import_offer = (!settings.twinkle_import_done)
        .then(twinkle_import::find)
        .flatten()
        .and_then(|path| match twinkle_import::load(&path) {
            Ok(pending) => Some(pending),
            Err(error) => {
                logger::error(format!("could not read Twinkle Tray settings: {error:#}"));
                None
            }
        });

    // Hardware events hop onto the GPUI thread through the popup channel.
    let service = Arc::new(MonitorService::start(|event| {
        popup::send_command(PopupCommand::Monitors(event));
    }));

    let persist_store = store.clone();
    let persist: PersistFn = Arc::new(move |settings: &Settings| {
        let settings = settings.clone();
        persist_store.edit(move |stored| *stored = settings);
    });

    let model_service = Arc::clone(&service);
    popup::ui::start(move |cx| {
        let model = cx.new(|_| AppModel::new(settings, model_service, persist));
        // Keep the tray tooltip on the primary display's level.
        cx.observe(&model, |model, cx| {
            candle_tray::tray::set_tooltip_level(model.read(cx).primary_level());
        })
        .detach();
        if let Some(pending) = import_offer {
            model.update(cx, |model, cx| model.offer_import(pending, cx));
        }
        candle_tray::automation::start(model.clone(), cx);
        model
    });

    let refresh_service = Arc::clone(&service);
    let result = candle_tray::tray::run(
        move || refresh_service.send(Command::Refresh),
        move || service.send(Command::Shutdown),
    );
    if let Err(error) = store.flush() {
        logger::error(format!("could not save settings on exit: {error:#}"));
    }
    drop(instance);
    result
}

fn main() {
    if let Err(error) = run() {
        logger::error(format!("Candle Tray failed to start: {error:#}"));
        std::process::exit(1);
    }
    // The GPUI thread owns no state that needs an orderly shutdown.
    std::process::exit(0);
}
