//! Tray icon thread: the icon, its menu, and popup dismissal.
//!
//! The tray lives on its own thread with a Win32 message pump (tray-icon's
//! hidden window must be serviced by the thread that created it). Clicks
//! toggle the popup; the same loop polls for outside clicks and Escape while
//! the popup is open, exactly like Codex Minibar's bridge.
//!
//! Windows sends no wheel input to notification icons. While the pointer
//! rests on ours, a low-level mouse hook (installed on this thread, so its
//! callback runs in this pump) catches the wheel; it is removed again when
//! the pointer leaves, so it costs nothing the rest of the time.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tray_icon::{
    Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
};

use crate::popup::{self, PopupCommand};
use crate::settings::TrayIcon;

static MENU_DIRTY: AtomicBool = AtomicBool::new(false);
static EXIT_REQUESTED: AtomicBool = AtomicBool::new(false);
/// Primary display brightness for the tooltip; 255 = unknown.
static TOOLTIP_LEVEL: AtomicU8 = AtomicU8::new(255);
/// Index into [`TrayIcon::ALL`] of the glyph to show.
static ICON: AtomicU8 = AtomicU8::new(0);
/// Win32 id of the thread running [`run`]; 0 before it starts.
static TRAY_THREAD: AtomicU32 = AtomicU32::new(0);
/// Scrolling over the icon changes the levels.
static SCROLL_ENABLED: AtomicBool = AtomicBool::new(true);
/// Wheel delta collected over the icon, in `WHEEL_DELTA` units (120 per
/// notch); partial notches from precise wheels carry over.
static WHEEL: AtomicI32 = AtomicI32::new(0);

/// Turn scrolling over the tray icon on or off.
pub fn set_scroll_enabled(enabled: bool) {
    SCROLL_ENABLED.store(enabled, Ordering::SeqCst);
}

/// Rebuild the menu labels (language changed).
pub fn refresh_menu() {
    MENU_DIRTY.store(true, Ordering::SeqCst);
    wake();
}

/// Publish the primary display brightness shown in the tooltip.
pub fn set_tooltip_level(level: Option<u8>) {
    TOOLTIP_LEVEL.store(level.unwrap_or(255), Ordering::SeqCst);
    wake();
}

/// Switch the tray glyph.
pub fn set_icon(icon: TrayIcon) {
    let index = TrayIcon::ALL.iter().position(|i| *i == icon).unwrap_or(0);
    ICON.store(index as u8, Ordering::SeqCst);
    wake();
}

fn current_icon() -> TrayIcon {
    TrayIcon::ALL
        .get(usize::from(ICON.load(Ordering::SeqCst)))
        .copied()
        .unwrap_or_default()
}

/// Ask the tray thread to remove its icon and end the process.
pub fn request_exit() {
    EXIT_REQUESTED.store(true, Ordering::SeqCst);
    wake();
}

/// Interrupt the tray thread's idle wait so it picks up new state now.
pub(crate) fn wake() {
    let thread = TRAY_THREAD.load(Ordering::SeqCst);
    if thread != 0 {
        post_wake(thread);
    }
}

#[cfg(windows)]
fn post_wake(thread: u32) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_NULL};
    unsafe {
        PostThreadMessageW(thread, WM_NULL, 0, 0);
    }
}

#[cfg(not(windows))]
fn post_wake(_thread: u32) {}

#[cfg(windows)]
fn current_thread_id() -> u32 {
    unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() }
}

#[cfg(not(windows))]
fn current_thread_id() -> u32 {
    0
}

const MENU_REFRESH: &str = "refresh";
const MENU_SETTINGS: &str = "settings";
const MENU_TURN_OFF: &str = "turn-off";
const MENU_EXIT: &str = "exit";

/// Run the tray loop on the calling thread until Exit. `on_exit` runs on
/// this thread right before it returns.
pub fn run(on_refresh: impl Fn(), on_exit: impl FnOnce()) -> Result<()> {
    let mut light = crate::theme::taskbar_uses_light_theme();
    let mut glyph = current_icon();
    let tray = TrayIconBuilder::new()
        .with_icon(tray_icon_image(glyph, light)?)
        .with_menu(Box::new(make_menu()?))
        .with_tooltip(crate::i18n::tr("app-name"))
        .with_menu_on_left_click(false)
        .build()
        .context("create tray icon")?;
    let mut tooltip_level = u8::MAX - 1;
    let mut next_theme_check = Instant::now();
    TRAY_THREAD.store(current_thread_id(), Ordering::SeqCst);
    // Registered from this thread so WM_HOTKEY reaches its message pump.
    let mut hotkeys = crate::hotkey::Registration::default();
    // Profile triggers only; the tray keeps working without them.
    let mut watchers = crate::foreground::Registration::new()
        .map_err(|error| {
            crate::logger::error(format!("could not watch the foreground app: {error:#}"))
        })
        .ok();
    let mut wheel = WheelHook::default();
    let mut light_sensor = None;
    let mut next_sensor_attempt = Instant::now();
    loop {
        // The theme and power-source checks share this timer; other inputs
        // arrive as window messages or a `wake`.
        let active = popup::is_visible();
        popup::wait_for_messages(Duration::from_millis(if active { 16 } else { 1000 }));
        popup::pump_messages();
        if !crate::light_sensor::is_enabled() {
            light_sensor = None;
            next_sensor_attempt = Instant::now();
        } else if light_sensor.is_none() && Instant::now() >= next_sensor_attempt {
            // Missing sensors and transient driver failures must not spin.
            next_sensor_attempt = Instant::now() + Duration::from_secs(30);
            match crate::light_sensor::Watcher::start() {
                Ok(watcher) => light_sensor = watcher,
                Err(error) => {
                    crate::logger::error(format!("could not watch ambient light: {error:#}"));
                }
            }
        }
        if let Some(watchers) = &mut watchers {
            watchers.poll();
        }

        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            match event {
                TrayIconEvent::Click {
                    position,
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } => {
                    if popup::is_visible() {
                        // A click while it is already closing (the press
                        // dismissed it) must not reopen it.
                        if !popup::is_closing() {
                            popup::hide();
                        }
                    } else {
                        popup::show_near(position.x as i32, position.y as i32);
                    }
                }
                TrayIconEvent::Enter { rect, .. } if SCROLL_ENABLED.load(Ordering::SeqCst) => {
                    wheel.watch(rect);
                }
                TrayIconEvent::Leave { .. } => wheel.stop(),
                _ => {}
            }
        }
        let notches = WHEEL.load(Ordering::SeqCst) / WHEEL_DELTA;
        if notches != 0 {
            WHEEL.fetch_sub(notches * WHEEL_DELTA, Ordering::SeqCst);
            popup::send_command(PopupCommand::TrayScroll(notches));
        }
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            match event.id.as_ref() {
                MENU_REFRESH => on_refresh(),
                MENU_SETTINGS => popup::send_command(PopupCommand::OpenSettings),
                MENU_TURN_OFF => {
                    popup::send_command(PopupCommand::Shortcut(crate::hotkey::Action::TurnOff))
                }
                MENU_EXIT => EXIT_REQUESTED.store(true, Ordering::SeqCst),
                _ => {}
            }
        }
        if hotkeys.sync() {
            // Settings shows whether each shortcut could be registered.
            popup::send_command(PopupCommand::Run(Box::new(|cx| cx.refresh_windows())));
        }
        for action in crate::hotkey::take_pressed() {
            popup::send_command(PopupCommand::Shortcut(action));
        }
        if EXIT_REQUESTED.load(Ordering::SeqCst) {
            drop(light_sensor);
            drop(wheel);
            drop(hotkeys);
            drop(watchers);
            drop(tray);
            on_exit();
            return Ok(());
        }
        if !popup::is_closing() && (popup::clicked_outside() || popup::escape_pressed()) {
            popup::hide();
        }
        if MENU_DIRTY.swap(false, Ordering::SeqCst) {
            match make_menu() {
                Ok(menu) => tray.set_menu(Some(Box::new(menu))),
                Err(error) => crate::logger::error(format!("{error:#}")),
            }
            tooltip_level = u8::MAX - 1;
        }
        let level = TOOLTIP_LEVEL.load(Ordering::SeqCst);
        if level != tooltip_level {
            tooltip_level = level;
            let text = if level <= 100 {
                crate::i18n::format("tray-tooltip", &[("percent", level.to_string())])
            } else {
                crate::i18n::tr("app-name").to_owned()
            };
            let _ = tray.set_tooltip(Some(text));
        }
        let now_glyph = current_icon();
        let mut restyle = now_glyph != glyph;
        if Instant::now() >= next_theme_check {
            next_theme_check = Instant::now() + Duration::from_secs(1);
            let now_light = crate::theme::taskbar_uses_light_theme();
            restyle |= now_light != light;
            light = now_light;
        }
        if restyle {
            glyph = now_glyph;
            if let Ok(icon) = tray_icon_image(glyph, light) {
                let _ = tray.set_icon(Some(icon));
            }
        }
    }
}

/// Mouse wheel units per notch.
const WHEEL_DELTA: i32 = 120;

/// Icon bounds in screen pixels while the wheel is watched.
static ICON_LEFT: AtomicI32 = AtomicI32::new(0);
static ICON_TOP: AtomicI32 = AtomicI32::new(0);
static ICON_RIGHT: AtomicI32 = AtomicI32::new(0);
static ICON_BOTTOM: AtomicI32 = AtomicI32::new(0);

/// The low-level mouse hook that is installed while the pointer is over
/// the icon. Lives on, and is dropped by, the tray thread.
#[derive(Default)]
struct WheelHook {
    #[cfg(windows)]
    hook: Option<windows_sys::Win32::UI::WindowsAndMessaging::HHOOK>,
}

impl WheelHook {
    fn watch(&mut self, rect: tray_icon::Rect) {
        let (x, y) = (rect.position.x as i32, rect.position.y as i32);
        ICON_LEFT.store(x, Ordering::SeqCst);
        ICON_TOP.store(y, Ordering::SeqCst);
        ICON_RIGHT.store(x + rect.size.width as i32, Ordering::SeqCst);
        ICON_BOTTOM.store(y + rect.size.height as i32, Ordering::SeqCst);
        #[cfg(windows)]
        if self.hook.is_none() {
            use windows_sys::Win32::{
                System::LibraryLoader::GetModuleHandleW,
                UI::WindowsAndMessaging::{SetWindowsHookExW, WH_MOUSE_LL},
            };
            // SAFETY: the callback is a plain function and the hook is
            // removed on this thread before it ends (Drop).
            let hook = unsafe {
                SetWindowsHookExW(
                    WH_MOUSE_LL,
                    Some(wheel_hook),
                    GetModuleHandleW(std::ptr::null()),
                    0,
                )
            };
            if !hook.is_null() {
                self.hook = Some(hook);
            }
        }
    }

    fn stop(&mut self) {
        #[cfg(windows)]
        if let Some(hook) = self.hook.take() {
            // SAFETY: the hook was installed by this thread.
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::UnhookWindowsHookEx(hook);
            }
        }
    }
}

impl Drop for WheelHook {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(windows)]
unsafe extern "system" fn wheel_hook(
    code: i32,
    wparam: windows_sys::Win32::Foundation::WPARAM,
    lparam: windows_sys::Win32::Foundation::LPARAM,
) -> windows_sys::Win32::Foundation::LRESULT {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, HC_ACTION, MSLLHOOKSTRUCT, WM_MOUSEWHEEL,
    };
    if code == HC_ACTION as i32 && wparam as u32 == WM_MOUSEWHEEL {
        // SAFETY: for WM_MOUSEWHEEL, lparam points at an MSLLHOOKSTRUCT.
        let info = unsafe { &*(lparam as *const MSLLHOOKSTRUCT) };
        let over_icon = (ICON_LEFT.load(Ordering::SeqCst)..ICON_RIGHT.load(Ordering::SeqCst))
            .contains(&info.pt.x)
            && (ICON_TOP.load(Ordering::SeqCst)..ICON_BOTTOM.load(Ordering::SeqCst))
                .contains(&info.pt.y);
        if over_icon && SCROLL_ENABLED.load(Ordering::SeqCst) {
            let delta = i32::from((info.mouseData >> 16) as u16 as i16);
            WHEEL.fetch_add(delta, Ordering::SeqCst);
        }
    }
    // SAFETY: passes the event on unchanged.
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

fn make_menu() -> Result<Menu> {
    let refresh = MenuItem::with_id(MENU_REFRESH, crate::i18n::tr("tray-refresh"), true, None);
    let settings = MenuItem::with_id(MENU_SETTINGS, crate::i18n::tr("tray-settings"), true, None);
    let turn_off = MenuItem::with_id(MENU_TURN_OFF, crate::i18n::tr("tray-turn-off"), true, None);
    let exit = MenuItem::with_id(MENU_EXIT, crate::i18n::tr("tray-exit"), true, None);
    Menu::with_items(&[
        &turn_off,
        &refresh,
        &settings,
        &PredefinedMenuItem::separator(),
        &exit,
    ])
    .context("create tray menu")
}

const ICON_SIZE: u32 = 32;

/// Asset name of a tray glyph, so Settings can preview it.
pub(crate) fn glyph_asset(glyph: TrayIcon) -> &'static str {
    match glyph {
        TrayIcon::Fire => "tray-fire",
        TrayIcon::Sun => "tray-sun",
        TrayIcon::BrightnessHigh => "tray-brightness-high",
        TrayIcon::BrightnessLow => "tray-brightness-low",
        TrayIcon::Lightbulb => "tray-lightbulb",
    }
}

/// The 16 px Fluent source; it stays pixel-aligned at 100% and 200% scale.
pub(crate) fn glyph_svg(glyph: TrayIcon) -> &'static str {
    match glyph {
        TrayIcon::Fire => include_str!("../assets/tray/fluent-fire-16-regular.svg"),
        TrayIcon::Sun => include_str!("../assets/tray/fluent-weather-sunny-16-regular.svg"),
        TrayIcon::BrightnessHigh => {
            include_str!("../assets/tray/fluent-brightness-high-16-regular.svg")
        }
        TrayIcon::BrightnessLow => {
            include_str!("../assets/tray/fluent-brightness-low-16-regular.svg")
        }
        TrayIcon::Lightbulb => include_str!("../assets/tray/fluent-lightbulb-16-regular.svg"),
    }
}

/// A Fluent glyph, black on a light taskbar and white on a dark one like the
/// system icons next to it.
fn tray_icon_image(glyph: TrayIcon, light_taskbar: bool) -> Result<Icon> {
    let color = if light_taskbar { "#000000" } else { "#FFFFFF" };
    let svg = glyph_svg(glyph).replace("currentColor", color);
    let pixels = rasterize(&svg, ICON_SIZE)?;
    Icon::from_rgba(pixels, ICON_SIZE, ICON_SIZE).context("create RGBA tray icon")
}

fn rasterize(svg: &str, size: u32) -> Result<Vec<u8>> {
    use resvg::{tiny_skia, usvg};
    let tree = usvg::Tree::from_str(svg, &usvg::Options::default()).context("parse tray svg")?;
    let mut pixmap = tiny_skia::Pixmap::new(size, size).context("allocate tray pixmap")?;
    let scale = size as f32 / tree.size().width().max(tree.size().height());
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    // tiny-skia stores premultiplied RGBA; Windows icons want straight alpha.
    let mut data = pixmap.take();
    for pixel in data.chunks_exact_mut(4) {
        let alpha = u32::from(pixel[3]);
        if alpha > 0 && alpha < 255 {
            for channel in &mut pixel[..3] {
                *channel = ((u32::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    #[test]
    fn tray_icons_rasterize() {
        for glyph in super::TrayIcon::ALL {
            for light in [true, false] {
                assert!(super::tray_icon_image(glyph, light).is_ok());
            }
        }
    }
}
