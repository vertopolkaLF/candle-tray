//! Thread-safe façade over the GPUI tray popup.
//!
//! The popup is rendered by GPUI on its own thread (see [`ui`]). The tray
//! thread only posts commands and reads the shared flags below; it never
//! touches GPUI state directly.
//!
//! Geometry model (from Codex Minibar): the native window is a fixed, tall
//! technical host anchored to the work area's edge by the taskbar. The
//! visible capsule is drawn inside it and the HWND region is clipped to that
//! capsule on every animation frame. Opening, closing and height motion
//! therefore never move or resize the HWND (and never reallocate the swap
//! chain); they only change what GPUI paints and what the region exposes.
//! After hiding, the host parks at 1x1 pixels to release its DirectX
//! surfaces.

pub mod ui;

use std::sync::{LazyLock, Mutex};

/// Commands posted to the GPUI thread. Every variant is cheap to send from
/// any thread; the GPUI side wakes through its foreground executor.
pub enum PopupCommand {
    Show {
        anchor: Option<(i32, i32)>,
    },
    Hide,
    AppearanceChanged,
    Reposition,
    /// Another window took the foreground; restore the popup to the top of
    /// the topmost band if it is visible.
    Raise,
    /// Hardware state from the monitor service.
    Monitors(crate::monitors::Event),
    /// Settings changed on another thread (or another window).
    SettingsChanged,
    OpenSettings,
    /// A profile shortcut was pressed.
    ApplyProfile(usize),
    /// Run on the GPUI thread with the app context (tray menu actions etc.).
    Run(Box<dyn FnOnce(&mut gpui::App) + Send>),
}

type CommandChannel = (
    futures::channel::mpsc::UnboundedSender<PopupCommand>,
    Mutex<Option<futures::channel::mpsc::UnboundedReceiver<PopupCommand>>>,
);

/// Commands sent before the GPUI thread starts simply queue up here.
static COMMANDS: LazyLock<CommandChannel> = LazyLock::new(|| {
    let (sender, receiver) = futures::channel::mpsc::unbounded();
    (sender, Mutex::new(Some(receiver)))
});

pub fn send_command(command: PopupCommand) {
    let _ = COMMANDS.0.unbounded_send(command);
}

pub(crate) fn take_command_receiver()
-> Option<futures::channel::mpsc::UnboundedReceiver<PopupCommand>> {
    COMMANDS.1.lock().ok().and_then(|mut slot| slot.take())
}

use std::{
    sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU8, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::settings::PopupMaterial;

/// Popup client width in DIP — height adapts to content.
pub const POPUP_WIDTH: i32 = 360;
/// Inner card radius.
pub const CARD_CORNER_RADIUS_DIP: i32 = 8;
/// Capsule corner radius (Windows 11 flyout radius).
pub const WINDOW_CORNER_RADIUS_DIP: i32 = 8;
/// Gap between the capsule and the monitor/taskbar edges, in physical pixels.
pub const EDGE_MARGIN_PX: i32 = 20;
/// Popup height as a share of the monitor it is opened on.
pub const POPUP_SCREEN_HEIGHT_FRACTION: f64 = 0.80;
/// Ignore outside presses briefly after open so the tray click that showed us
/// cannot immediately dismiss.
const SHOW_GRACE_MS: i64 = 200;

static POPUP_VISIBLE: AtomicBool = AtomicBool::new(false);
static POPUP_CLOSING: AtomicBool = AtomicBool::new(false);
static BUTTON_WAS_DOWN: AtomicBool = AtomicBool::new(false);
static ESCAPE_WAS_DOWN: AtomicBool = AtomicBool::new(false);
static IGNORE_OUTSIDE_UNTIL_MS: AtomicI64 = AtomicI64::new(0);
/// While set, outside clicks and Escape do not dismiss the popup.
static PINNED: AtomicBool = AtomicBool::new(false);
static POPUP_MATERIAL: AtomicU8 = AtomicU8::new(0);
/// Visible capsule bounds in screen pixels, published by the GPUI thread on
/// every geometry change. Outside-click detection must use the capsule and
/// not the transparent technical host around it.
static SURFACE_LEFT: AtomicI32 = AtomicI32::new(0);
static SURFACE_TOP: AtomicI32 = AtomicI32::new(0);
static SURFACE_RIGHT: AtomicI32 = AtomicI32::new(0);
static SURFACE_BOTTOM: AtomicI32 = AtomicI32::new(0);

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

pub fn is_visible() -> bool {
    POPUP_VISIBLE.load(Ordering::SeqCst)
}

pub fn is_closing() -> bool {
    POPUP_CLOSING.load(Ordering::SeqCst)
}

/// GPUI thread only: records the lifecycle reached by the native window.
pub(crate) fn set_lifecycle(visible: bool, closing: bool) {
    POPUP_VISIBLE.store(visible, Ordering::SeqCst);
    POPUP_CLOSING.store(closing, Ordering::SeqCst);
}

/// Keep the popup open while Settings uses it as a live preview.
pub fn set_pinned(pinned: bool) {
    PINNED.store(pinned, Ordering::SeqCst);
}

pub(crate) fn is_pinned() -> bool {
    PINNED.load(Ordering::SeqCst)
}

/// GPUI thread only: the capsule is now visible, so the press that opened it
/// must not count as an outside click.
pub(crate) fn arm_outside_click_grace() {
    IGNORE_OUTSIDE_UNTIL_MS.store(now_ms() + SHOW_GRACE_MS, Ordering::SeqCst);
    BUTTON_WAS_DOWN.store(true, Ordering::SeqCst);
}

/// GPUI thread only: publishes the capsule rectangle in screen pixels.
pub(crate) fn publish_surface_bounds(left: i32, top: i32, right: i32, bottom: i32) {
    SURFACE_LEFT.store(left, Ordering::SeqCst);
    SURFACE_TOP.store(top, Ordering::SeqCst);
    SURFACE_RIGHT.store(right, Ordering::SeqCst);
    SURFACE_BOTTOM.store(bottom, Ordering::SeqCst);
}

pub fn corner_radius_dip() -> i32 {
    WINDOW_CORNER_RADIUS_DIP
}

fn material_index(material: PopupMaterial) -> u8 {
    match material {
        PopupMaterial::Acrylic => 0,
        PopupMaterial::Mica => 1,
        PopupMaterial::Solid => 2,
    }
}

pub fn background_material() -> PopupMaterial {
    match POPUP_MATERIAL.load(Ordering::SeqCst) {
        1 => PopupMaterial::Mica,
        2 => PopupMaterial::Solid,
        _ => PopupMaterial::Acrylic,
    }
}

/// Apply the popup backdrop material. Startup calls this before the GPUI
/// thread exists; later calls repaint the live popup at once.
pub fn apply_popup_appearance(material: PopupMaterial) {
    let index = material_index(material);
    if POPUP_MATERIAL.swap(index, Ordering::SeqCst) != index {
        send_command(PopupCommand::AppearanceChanged);
    }
}

/// Show the popup near a tray click (physical screen pixels).
pub fn show_near(anchor_x: i32, anchor_y: i32) {
    show(anchor_x, anchor_y);
}

fn show(anchor_x: i32, anchor_y: i32) {
    // Optimistic: the tray bridge reads this flag before the GPUI thread has
    // processed the command, and a second tray click must toggle it closed.
    POPUP_VISIBLE.store(true, Ordering::SeqCst);
    POPUP_CLOSING.store(false, Ordering::SeqCst);
    arm_outside_click_grace();
    send_command(PopupCommand::Show {
        anchor: Some((anchor_x, anchor_y)),
    });
    // A show from another thread (second launch) must start the tray's
    // fast outside-click polling at once.
    crate::tray::wake();
}

/// Show the popup beside the current pointer location.
///
/// Settings opened from the tray menu do not carry a tray-click position, but
/// the pointer still gives the expected monitor and taskbar anchor.
pub fn show_near_cursor() {
    let (x, y) = cursor_position();
    show_near(x, y);
}

pub fn hide() {
    if !is_visible() || is_closing() {
        return;
    }
    POPUP_CLOSING.store(true, Ordering::SeqCst);
    send_command(PopupCommand::Hide);
}

pub fn toggle_near(anchor_x: i32, anchor_y: i32) {
    if is_visible() && !is_closing() {
        hide();
    } else {
        show_near(anchor_x, anchor_y);
    }
}

/// Respect the Windows "Animation effects" accessibility preference.
#[cfg(windows)]
pub fn system_animations_enabled() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SPI_GETCLIENTAREAANIMATION, SystemParametersInfoW,
    };
    let mut enabled = 1i32;
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            &mut enabled as *mut i32 as *mut _,
            0,
        )
    };
    ok == 0 || enabled != 0
}

#[cfg(not(windows))]
pub fn system_animations_enabled() -> bool {
    true
}

pub fn animations_enabled() -> bool {
    crate::theme::animations_enabled()
}

/// Dispatch messages for the thread that owns the tray icon.
#[cfg(windows)]
pub fn pump_messages() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
    };
    unsafe {
        let mut message = std::mem::zeroed::<MSG>();
        while PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
            crate::hotkey::observe_message(&message);
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

#[cfg(not(windows))]
pub fn pump_messages() {}

/// Sleeps until a window message is queued for this thread or `timeout`
/// elapses, whichever comes first. Tray clicks, menu input and popup window
/// messages therefore wake the bridge immediately instead of on a poll tick.
#[cfg(windows)]
pub fn wait_for_messages(timeout: std::time::Duration) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, QS_ALLINPUT,
    };
    let millis = timeout.as_millis().min(u128::from(u32::MAX - 1)) as u32;
    unsafe {
        MsgWaitForMultipleObjectsEx(
            0,
            std::ptr::null(),
            millis,
            QS_ALLINPUT,
            MWMO_INPUTAVAILABLE,
        );
    }
}

#[cfg(not(windows))]
pub fn wait_for_messages(timeout: std::time::Duration) {
    std::thread::sleep(timeout);
}

#[cfg(windows)]
fn cursor_position() -> (i32, i32) {
    use windows_sys::Win32::{Foundation::POINT, UI::WindowsAndMessaging::GetCursorPos};
    let mut cursor = POINT { x: 0, y: 0 };
    unsafe {
        GetCursorPos(&mut cursor);
    }
    (cursor.x, cursor.y)
}

#[cfg(not(windows))]
fn cursor_position() -> (i32, i32) {
    (0, 0)
}

#[cfg(windows)]
fn any_mouse_button_down() -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON,
    };
    unsafe {
        [VK_LBUTTON, VK_MBUTTON, VK_RBUTTON]
            .into_iter()
            .any(|button| GetAsyncKeyState(button as i32) < 0)
    }
}

#[cfg(not(windows))]
fn any_mouse_button_down() -> bool {
    false
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SurfaceRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl SurfaceRect {
    pub(crate) fn contains(self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }
}

fn surface_rect() -> SurfaceRect {
    SurfaceRect {
        left: SURFACE_LEFT.load(Ordering::SeqCst),
        top: SURFACE_TOP.load(Ordering::SeqCst),
        right: SURFACE_RIGHT.load(Ordering::SeqCst),
        bottom: SURFACE_BOTTOM.load(Ordering::SeqCst),
    }
}

/// Detect a new mouse press that lands outside the visible capsule.
pub fn clicked_outside() -> bool {
    if !is_visible() || is_pinned() || now_ms() < IGNORE_OUTSIDE_UNTIL_MS.load(Ordering::SeqCst) {
        BUTTON_WAS_DOWN.store(any_mouse_button_down(), Ordering::SeqCst);
        return false;
    }
    let button_is_down = any_mouse_button_down();
    let was_down = BUTTON_WAS_DOWN.swap(button_is_down, Ordering::SeqCst);
    if !button_is_down || was_down {
        return false;
    }
    let surface = surface_rect();
    if surface.right <= surface.left || surface.bottom <= surface.top {
        return false;
    }
    let (x, y) = cursor_position();
    !surface.contains(x, y)
}

/// Rising edge of Escape while the transient popup is visible.
#[cfg(windows)]
pub fn escape_pressed() -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_ESCAPE};
    if !is_visible() {
        ESCAPE_WAS_DOWN.store(false, Ordering::SeqCst);
        return false;
    }
    let down = unsafe { GetAsyncKeyState(VK_ESCAPE as i32) < 0 };
    let was_down = ESCAPE_WAS_DOWN.swap(down, Ordering::SeqCst);
    down && !was_down && !is_pinned()
}

#[cfg(not(windows))]
pub fn escape_pressed() -> bool {
    false
}

/// Normalized critically-damped spring curve used for the short open/close
/// slide. It is monotonic, so the capsule never overshoots the monitor seam.
pub(crate) fn critical_spring_curve(progress: f64, omega: f64) -> f64 {
    let t = progress.clamp(0.0, 1.0);
    if t <= 0.0 {
        return 0.0;
    }
    if t >= 1.0 {
        return 1.0;
    }
    (1.0 - (1.0 + omega * t) * (-omega * t).exp()).clamp(0.0, 1.0)
}

/// Spring-based entrance: fast response with a soft landing.
pub(crate) fn ease_entrance(progress: f64) -> f64 {
    critical_spring_curve(progress, 8.0)
}

/// Spring-based gentle exit: the surface gains speed as it leaves.
pub(crate) fn ease_exit(progress: f64) -> f64 {
    critical_spring_curve(progress, 7.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spring_curves_keep_exact_endpoints() {
        for easing in [ease_entrance, ease_exit] {
            assert_eq!(easing(0.0), 0.0);
            assert_eq!(easing(1.0), 1.0);
        }
    }

    #[test]
    fn spring_curves_are_monotonic() {
        for easing in [ease_entrance, ease_exit] {
            let samples = (0..=100)
                .map(|step| easing(f64::from(step) / 100.0))
                .collect::<Vec<_>>();
            assert!(samples.windows(2).all(|pair| pair[0] <= pair[1]));
        }
    }

    #[test]
    fn surface_hit_test_excludes_the_right_and_bottom_edges() {
        let surface = SurfaceRect {
            left: 100,
            top: 200,
            right: 480,
            bottom: 700,
        };
        assert!(surface.contains(100, 200));
        assert!(surface.contains(479, 699));
        assert!(!surface.contains(480, 300));
        assert!(!surface.contains(300, 700));
        assert!(!surface.contains(99, 300));
    }
}
