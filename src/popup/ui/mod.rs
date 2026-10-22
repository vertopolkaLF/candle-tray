//! GPUI implementation of the tray popup.
//!
//! GPUI runs on a dedicated thread with its own message loop. It hosts the
//! popup and the Settings window; other threads talk to it only through
//! [`super::PopupCommand`].

pub(crate) mod assets;
#[cfg(windows)]
mod backdrop;
#[cfg(windows)]
mod blur_effect;
#[allow(dead_code)]
pub(crate) mod components;
#[allow(dead_code)]
pub(crate) mod controls;
// Minibar's motion toolkit, kept whole so ported UI code keeps working.
#[allow(dead_code)]
pub(crate) mod fx;
#[cfg(windows)]
mod osd;
mod root;
pub(crate) mod slider;
pub(crate) mod theme;
#[cfg(windows)]
pub(crate) mod win32;

use futures::StreamExt;
use gpui::{
    AppContext, Application, Bounds, WindowBackgroundAppearance, WindowBounds, WindowKind,
    WindowOptions, point, px, size,
};

pub(crate) use root::PopupRoot;
#[cfg(windows)]
pub(crate) use win32::Monitor;

use super::PopupCommand;
use crate::app::AppModel;

/// Native work to do after an update that prepared a show.
#[cfg(windows)]
pub(crate) struct ShowPlan {
    pub(crate) hwnd: windows_sys::Win32::Foundation::HWND,
    pub(crate) rect: win32::Rect,
    pub(crate) first_show: bool,
}

/// Whether popup motion runs: Windows "Animation effects".
pub(crate) fn animations_enabled() -> bool {
    crate::popup::system_animations_enabled()
}

/// Which corner of the work area the popup stands in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Placement {
    pub(crate) left: bool,
    pub(crate) top: bool,
}

#[cfg(windows)]
impl Placement {
    /// The corner by the tray end of the taskbar.
    pub(crate) fn for_monitor(monitor: Monitor) -> Self {
        let edge = taskbar_edge(monitor);
        Self {
            left: edge == Edge::Left,
            top: edge == Edge::Top,
        }
    }
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Edge {
    Left,
    Top,
    Right,
    Bottom,
}

/// The monitor edge the taskbar is docked to. Its own frame decides, because
/// third-party appbars shrink the work area too; the work-area insets are only
/// a fallback when no taskbar lives on this monitor.
#[cfg(windows)]
fn taskbar_edge(monitor: Monitor) -> Edge {
    let (bounds, work) = (monitor.bounds, monitor.work);
    if let Some(bar) = monitor.taskbar {
        return if bar.height() > bar.width() {
            if bar.left + bar.right < bounds.left + bounds.right {
                Edge::Left
            } else {
                Edge::Right
            }
        } else if bar.top + bar.bottom < bounds.top + bounds.bottom {
            Edge::Top
        } else {
            Edge::Bottom
        };
    }
    if work.left > bounds.left {
        Edge::Left
    } else if work.right < bounds.right {
        Edge::Right
    } else if work.top > bounds.top {
        Edge::Top
    } else {
        Edge::Bottom
    }
}

/// The fixed technical host: anchored to the work area's edge on the
/// placement's side, standing on its bottom (or top) margin, tall enough for
/// every layout.
#[cfg(windows)]
pub(crate) fn host_rect(monitor: Monitor, placement: Placement) -> win32::Rect {
    let scale = monitor.scale();
    let margin = crate::popup::EDGE_MARGIN_PX;
    let width = (f64::from(crate::popup::POPUP_WIDTH) * scale).round() as i32 + margin;
    let height = (f64::from(monitor.bounds.height()) * crate::popup::POPUP_SCREEN_HEIGHT_FRACTION)
        .round() as i32;
    let work = monitor.work;
    let (left, right) = if placement.left {
        (work.left, work.left + width)
    } else {
        (work.right - width, work.right)
    };
    let (top, bottom) = if placement.top {
        let top = work.top + margin;
        (top, (top + height).min(work.bottom))
    } else {
        let bottom = work.bottom - margin;
        ((bottom - height).max(work.top), bottom)
    };
    win32::Rect {
        left,
        top,
        right,
        bottom,
    }
}

/// Start the GPUI thread. `init` runs once on it with the app context, before
/// any window opens, and returns the shared model.
pub fn start(init: impl FnOnce(&mut gpui::App) -> gpui::Entity<AppModel> + Send + 'static) {
    let Some(commands) = super::take_command_receiver() else {
        return;
    };
    let spawned = std::thread::Builder::new()
        .name("popup-gpui".into())
        .spawn(move || run(init, commands));
    if let Err(error) = spawned {
        crate::logger::error(format!("could not start the popup UI thread: {error}"));
    }
}

fn run(
    init: impl FnOnce(&mut gpui::App) -> gpui::Entity<AppModel> + 'static,
    mut commands: futures::channel::mpsc::UnboundedReceiver<PopupCommand>,
) {
    Application::new()
        .with_assets(assets::PopupAssets)
        .run(move |cx| {
            let model = init(cx);
            let default_font = theme::default_font_family(cx);
            // The tray starts hidden. Do not allocate large swap-chain/MSAA
            // surfaces before the user has even opened the popup.
            let initial = size(px(1.0), px(1.0));
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: point(px(0.0), px(0.0)),
                    size: initial,
                })),
                titlebar: None,
                focus: false,
                show: false,
                kind: WindowKind::PopUp,
                is_movable: false,
                is_resizable: false,
                is_minimizable: false,
                display_id: None,
                // Never enable HWND-wide Acrylic: it ignores the capsule's
                // region. A separate clipped composition visual supplies blur.
                window_background: WindowBackgroundAppearance::Transparent,
                app_id: None,
                window_min_size: None,
                window_max_size: None,
                window_decorations: None,
                tabbing_identifier: None,
            };
            let root_model = model.clone();
            let window = match cx.open_window(options, |window, cx| {
                cx.new(|cx| PopupRoot::new(root_model, default_font, window, cx))
            }) {
                Ok(window) => window,
                Err(error) => {
                    crate::logger::error(format!("could not create the popup window: {error:#}"));
                    return;
                }
            };

            cx.spawn(async move |cx| {
                #[cfg(windows)]
                {
                    let hwnd = window
                        .update(cx, |_, window, _| win32::hwnd_from_window(window))
                        .ok()
                        .flatten();
                    if let Some(hwnd) = hwnd {
                        win32::configure(hwnd);
                        let _ = window.update(cx, |root, _, _| {
                            root.attach_native_host(hwnd);
                        });
                        win32::park_hidden(hwnd);
                        cx.spawn(async move |cx| prewarm(window, cx).await).detach();
                    }
                }
                let mut osd_window = None;
                while let Some(command) = commands.next().await {
                    handle_command(command, &window, &model, &mut osd_window, cx);
                }
            })
            .detach();
        });
}

fn handle_command(
    command: PopupCommand,
    window: &gpui::WindowHandle<PopupRoot>,
    model: &gpui::Entity<AppModel>,
    #[cfg(windows)] osd_window: &mut Option<gpui::WindowHandle<osd::Osd>>,
    #[cfg(not(windows))] osd_window: &mut Option<()>,
    cx: &mut gpui::AsyncApp,
) {
    match command {
        PopupCommand::Osd(name, value) => {
            #[cfg(windows)]
            {
                if osd_window.is_none() {
                    *osd_window = osd::create(model.clone(), cx);
                }
                if let Some(handle) = *osd_window {
                    osd::show(handle, name, value, cx);
                }
            }
            #[cfg(not(windows))]
            let _ = (name, value);
        }
        PopupCommand::Show { anchor } => {
            crate::idle::leave();
            let _ = model.update(cx, |model, _| model.read_levels());
            #[cfg(windows)]
            {
                let plan = window
                    .update(cx, |root, window, cx| root.begin_show(anchor, window, cx))
                    .ok()
                    .flatten();
                let Some(plan) = plan else {
                    crate::popup::set_lifecycle(false, false);
                    return;
                };
                if plan.first_show {
                    // Hide every pixel before the first on-screen frame; the
                    // capsule then slides in from beyond the monitor edge.
                    win32::set_region(plan.hwnd, None, 0);
                }
                win32::place(plan.hwnd, plan.rect);
                // A DPI change during placement makes GPUI apply Windows'
                // suggested rect; place again with the final geometry.
                if win32::window_rect(plan.hwnd) != plan.rect {
                    win32::place(plan.hwnd, plan.rect);
                }
                let _ = window.update(cx, |root, window, cx| {
                    root.host.last_region = None;
                    window.refresh();
                    cx.notify();
                });
                if plan.first_show {
                    win32::show(plan.hwnd);
                    if animations_enabled() {
                        // Hold the UI thread until the first frame is really
                        // on screen. The slide counts this wait as a hitch,
                        // so the entrance plays instead of finishing unseen.
                        win32::wait_for_composition();
                    }
                }
                if !animations_enabled() {
                    win32::activate(plan.hwnd);
                }
            }
            #[cfg(not(windows))]
            {
                let _ = anchor;
            }
        }
        PopupCommand::Hide => {
            let _ = window.update(cx, |root, _, cx| root.begin_hide(cx));
        }
        PopupCommand::AppearanceChanged => {
            let _ = window.update(cx, |root, window, cx| root.appearance_changed(window, cx));
            let _ = cx.update(|cx| cx.refresh_windows());
        }
        PopupCommand::Cli(command) => {
            let _ = model.update(cx, |model, cx| model.run_cli(command, cx));
        }
        PopupCommand::Monitors(event) => {
            let _ = model.update(cx, |model, cx| model.handle_monitor_event(event, cx));
        }
        PopupCommand::SettingsChanged => {
            let _ = model.update(cx, |model, cx| model.reload_settings(cx));
        }
        PopupCommand::OpenSettings => {
            crate::idle::leave();
            let model = model.clone();
            let _ = cx.update(|cx| crate::settings_window::open(model, cx));
        }
        PopupCommand::Shortcut(action) => {
            let _ = model.update(cx, |model, cx| model.run_shortcut(action, cx));
        }
        PopupCommand::TrayScroll(notches) => {
            let _ = model.update(cx, |model, cx| model.scroll_all(notches, cx));
        }
        PopupCommand::Foreground { exe, .. } => {
            let _ = model.update(cx, |model, cx| model.on_foreground(&exe, cx));
        }
        PopupCommand::PowerSource { on_battery } => {
            let _ = model.update(cx, |model, cx| model.on_power_source(on_battery, cx));
        }
        PopupCommand::SessionLocked(locked) => {
            let _ = model.update(cx, |model, cx| model.on_session_locked(locked, cx));
        }
        PopupCommand::Suspended(suspended) => {
            let _ = model.update(cx, |model, cx| model.on_suspend(suspended, cx));
        }
        PopupCommand::LidClosed(closed) => {
            let _ = model.update(cx, |model, cx| model.on_lid_closed(closed, cx));
        }
        PopupCommand::AmbientLight { lux } => {
            let _ = model.update(cx, |model, cx| model.on_ambient_light(lux, cx));
        }
        PopupCommand::Run(job) => {
            let _ = cx.update(|cx| job(cx));
        }
        PopupCommand::Raise => {
            #[cfg(windows)]
            {
                let hwnd = window
                    .update(cx, |root, _, _| {
                        root.host.hwnd.filter(|_| root.host.visible())
                    })
                    .ok()
                    .flatten();
                if let Some(hwnd) = hwnd {
                    win32::raise(hwnd);
                }
            }
        }
        PopupCommand::Reposition => {
            #[cfg(windows)]
            {
                let plan = window
                    .update(cx, |root, _, _| {
                        let hwnd = root.host.hwnd?;
                        if !root.host.visible() {
                            return None;
                        }
                        let rect = win32::window_rect(hwnd);
                        let monitor = win32::monitor_for_point(rect.right - 1, rect.bottom - 1);
                        root.host.monitor = Some(monitor);
                        root.host.scale = monitor.scale() as f32;
                        Some((hwnd, host_rect(monitor, root.host.placement)))
                    })
                    .ok()
                    .flatten();
                if let Some((hwnd, rect)) = plan
                    && win32::window_rect(hwnd) != rect
                {
                    win32::place_keep_order(hwnd, rect);
                    let _ = window.update(cx, |root, window, cx| {
                        root.host.last_region = None;
                        window.refresh();
                        cx.notify();
                    });
                }
            }
        }
    }
}

/// The popup is the app's main surface: render it once invisibly at launch so
/// the first tray click gets a warm renderer and a fully played open slide.
/// A real Show during the prewarm simply takes over the already shown host.
#[cfg(windows)]
async fn prewarm(window: gpui::WindowHandle<PopupRoot>, cx: &mut gpui::AsyncApp) {
    use std::time::{Duration, Instant};
    const LIMIT: Duration = Duration::from_secs(3);
    let Some(plan) = window
        .update(cx, |root, window, cx| root.begin_prewarm(window, cx))
        .ok()
        .flatten()
    else {
        return;
    };
    win32::set_region(plan.hwnd, None, 0);
    win32::place(plan.hwnd, plan.rect);
    if win32::window_rect(plan.hwnd) != plan.rect {
        win32::place(plan.hwnd, plan.rect);
    }
    let _ = window.update(cx, |root, window, cx| {
        root.host.last_region = None;
        window.refresh();
        cx.notify();
    });
    win32::show(plan.hwnd);
    let started = Instant::now();
    loop {
        cx.background_executor()
            .timer(Duration::from_millis(50))
            .await;
        let settled = window
            .update(cx, |root, _, _| root.prewarm_settled())
            .unwrap_or(true);
        if settled || started.elapsed() >= LIMIT {
            break;
        }
    }
    let _ = window.update(cx, |root, _, cx| root.end_prewarm(cx));
}
