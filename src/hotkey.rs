//! System-wide shortcuts: profiles, brightness steps and turning displays off.
//!
//! Shortcuts are registered with `RegisterHotKey` on the tray thread, which
//! already pumps the tray's messages. `WM_HOTKEY` arrives as a thread
//! message (no window), so [`observe_message`] picks it out of the pump and
//! the tray loop consumes it with [`take_pressed`]. The hotkey and recorder
//! logic is Codex Minibar's, extended from one shortcut to one per action.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

/// A key plus modifiers. `key` is a Windows virtual-key code, so the
/// shortcut follows the physical key regardless of the keyboard layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Hotkey {
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub alt: bool,
    #[serde(default)]
    pub shift: bool,
    #[serde(default)]
    pub win: bool,
    pub key: u16,
}

const VK_F1: u16 = 0x70;
const VK_F13: u16 = 0x7C;
const VK_F24: u16 = 0x87;

impl Hotkey {
    /// Ctrl, Alt or Win is required so the shortcut cannot swallow ordinary
    /// typing (Shift+A) or a key that other apps rely on (F5). F13–F24 are
    /// free on standard keyboards and macro pads, so they may stand alone.
    pub fn is_valid(&self) -> bool {
        !is_modifier_key(self.key)
            && self.key != 0
            && (self.ctrl || self.alt || self.win || (VK_F13..=VK_F24).contains(&self.key))
    }

    /// Display text such as `Ctrl+Alt+M`.
    pub fn label(&self) -> String {
        let mut parts = Vec::with_capacity(5);
        if self.ctrl {
            parts.push("Ctrl".to_string());
        }
        if self.alt {
            parts.push("Alt".to_string());
        }
        if self.shift {
            parts.push("Shift".to_string());
        }
        if self.win {
            parts.push("Win".to_string());
        }
        parts.push(key_name(self.key));
        parts.join("+")
    }
}

fn is_modifier_key(key: u16) -> bool {
    matches!(
        key,
        // Shift, Ctrl, Alt, both Win keys and their left/right variants.
        0x10 | 0x11 | 0x12 | 0x5B | 0x5C | 0xA0..=0xA5
    )
}

fn key_name(key: u16) -> String {
    match key {
        0x30..=0x39 | 0x41..=0x5A => char::from(key as u8).to_string(),
        VK_F1..=VK_F24 => format!("F{}", key - VK_F1 + 1),
        _ => system_key_name(key).unwrap_or_else(|| format!("Key {key:#04X}")),
    }
}

#[cfg(windows)]
fn system_key_name(key: u16) -> Option<String> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetKeyNameTextW, MAPVK_VK_TO_VSC, MapVirtualKeyW,
    };
    let scan = unsafe { MapVirtualKeyW(u32::from(key), MAPVK_VK_TO_VSC) };
    if scan == 0 {
        return None;
    }
    // Navigation keys share scan codes with the numeric keypad; the extended
    // bit selects "Delete" instead of "Num Del".
    let extended = matches!(key, 0x21..=0x28 | 0x2D | 0x2E | 0x6F | 0x90 | 0x5D);
    let lparam = ((scan as i32) << 16) | if extended { 1 << 24 } else { 0 };
    let mut buffer = [0u16; 64];
    let len = unsafe { GetKeyNameTextW(lparam, buffer.as_mut_ptr(), buffer.len() as i32) };
    (len > 0).then(|| String::from_utf16_lossy(&buffer[..len as usize]))
}

#[cfg(not(windows))]
fn system_key_name(_key: u16) -> Option<String> {
    None
}

/// Reads the shortcut being pressed while a key-down message is handled.
///
/// GPUI reports layout-dependent key text (and folds Shift into symbols), so
/// the recorder reads the virtual-key state Windows keeps for the message
/// being processed instead.
#[cfg(windows)]
pub fn pressed_now() -> Option<Hotkey> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
    };
    let down = |key: u16| unsafe { GetKeyState(i32::from(key)) } < 0;
    let key = (0x08..=0xFE_u16).find(|&key| {
        !is_modifier_key(key)
            // Mouse buttons, lock keys, IME processing and injected Unicode.
            && !matches!(key, 0x14 | 0x90 | 0x91 | 0xE5 | 0xE7)
            && down(key)
    })?;
    Some(Hotkey {
        ctrl: down(VK_CONTROL),
        alt: down(VK_MENU),
        shift: down(VK_SHIFT),
        win: down(VK_LWIN) || down(VK_RWIN),
        key,
    })
}

#[cfg(not(windows))]
pub fn pressed_now() -> Option<Hotkey> {
    None
}

/// What a shortcut does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    /// Apply the profile at this index.
    Profile(usize),
    BrightnessUp,
    BrightnessDown,
    TurnOff,
}

impl Action {
    /// Brightness steps repeat while the keys are held; everything else
    /// fires once per press.
    fn repeats(self) -> bool {
        matches!(self, Self::BrightnessUp | Self::BrightnessDown)
    }
}

/// Registration outcome of one shortcut, shown in Settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Status {
    #[default]
    Off,
    Active,
    /// Another application (or Windows) already owns the combination.
    Unavailable,
}

/// Shortcuts as the settings last asked for them.
static DESIRED: Mutex<Vec<(Action, Option<Hotkey>)>> = Mutex::new(Vec::new());
static DIRTY: AtomicBool = AtomicBool::new(false);
static STATUS: Mutex<Vec<(Action, Status)>> = Mutex::new(Vec::new());
/// Current id-to-action mapping and the cursor retained across registrations.
static REGISTERED: Mutex<HotkeyIds> = Mutex::new(HotkeyIds::new());
/// Actions whose shortcut was pressed, oldest first.
static PRESSED: Mutex<Vec<Action>> = Mutex::new(Vec::new());
/// Set while Settings records a shortcut, so pressing an existing one
/// records it instead of running its action.
static SUSPENDED: AtomicBool = AtomicBool::new(false);

/// Application-owned ids, excluding the shared-DLL range (0xC000..=0xFFFF).
const HOTKEY_ID_MAX: i32 = 0xBFFF;

struct HotkeyIds {
    next: i32,
    actions: BTreeMap<i32, Action>,
}

impl HotkeyIds {
    const fn new() -> Self {
        Self {
            next: 0x4354,
            actions: BTreeMap::new(),
        }
    }

    /// Keep advancing across generations so queued messages for retired ids
    /// cannot select a newly registered action. Wrap only after the app range.
    fn allocate(&mut self) -> Option<i32> {
        for _ in 0..=HOTKEY_ID_MAX {
            let id = self.next;
            self.next = if id == HOTKEY_ID_MAX { 0 } else { id + 1 };
            if !self.actions.contains_key(&id) {
                return Some(id);
            }
        }
        None
    }

    #[cfg(any(windows, test))]
    fn action(&self, id: i32) -> Option<Action> {
        self.actions.get(&id).copied()
    }
}

/// Publish every shortcut the settings define.
pub fn set_desired(hotkeys: Vec<(Action, Option<Hotkey>)>) {
    let mut desired = DESIRED.lock().unwrap_or_else(|e| e.into_inner());
    if *desired != hotkeys {
        *desired = hotkeys;
        DIRTY.store(true, Ordering::SeqCst);
        crate::tray::wake();
    }
}

pub fn status(action: Action) -> Status {
    STATUS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(known, _)| *known == action)
        .map(|(_, status)| *status)
        .unwrap_or_default()
}

pub fn set_suspended(suspended: bool) {
    if SUSPENDED.swap(suspended, Ordering::SeqCst) != suspended {
        DIRTY.store(true, Ordering::SeqCst);
        crate::tray::wake();
    }
}

/// Notes a `WM_HOTKEY` for one of our shortcuts. Called by the tray's
/// message pump for every message it removes from the queue.
#[cfg(windows)]
pub fn observe_message(message: &windows_sys::Win32::UI::WindowsAndMessaging::MSG) {
    use windows_sys::Win32::UI::WindowsAndMessaging::WM_HOTKEY;
    if message.message != WM_HOTKEY {
        return;
    }
    let action = i32::try_from(message.wParam).ok().and_then(|id| {
        REGISTERED
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .action(id)
    });
    if let Some(action) = action {
        PRESSED
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(action);
    }
}

/// Actions whose shortcut was pressed since the last call.
pub fn take_pressed() -> Vec<Action> {
    std::mem::take(&mut *PRESSED.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Owns the registrations. Must live on, and be dropped by, the thread whose
/// message queue receives `WM_HOTKEY` (the tray thread).
#[derive(Default)]
pub struct Registration {
    /// Ids currently registered.
    registered: Vec<i32>,
}

impl Registration {
    /// Re-register after the shortcuts or the recording state changed.
    /// Returns whether any visible status changed.
    pub fn sync(&mut self) -> bool {
        if !DIRTY.swap(false, Ordering::SeqCst) {
            return false;
        }
        self.unregister();
        let desired = DESIRED.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let suspended = SUSPENDED.load(Ordering::SeqCst);
        let mut statuses = Vec::with_capacity(desired.len());
        let mut ids = REGISTERED.lock().unwrap_or_else(|e| e.into_inner());
        for (action, hotkey) in &desired {
            let status = match hotkey.filter(Hotkey::is_valid) {
                None => Status::Off,
                // Recording keeps the last known status on display.
                Some(_) if suspended => status(*action),
                Some(hotkey) => {
                    if let Some(id) = ids
                        .allocate()
                        .filter(|&id| register(id, hotkey, action.repeats()))
                    {
                        self.registered.push(id);
                        ids.actions.insert(id, *action);
                        Status::Active
                    } else {
                        crate::logger::info(format!(
                            "Shortcut {} is already in use by another application",
                            hotkey.label()
                        ));
                        Status::Unavailable
                    }
                }
            };
            statuses.push((*action, status));
        }
        let mut current = STATUS.lock().unwrap_or_else(|e| e.into_inner());
        let changed = *current != statuses;
        *current = statuses;
        changed
    }

    fn unregister(&mut self) {
        let mut ids = REGISTERED.lock().unwrap_or_else(|e| e.into_inner());
        for id in self.registered.drain(..) {
            ids.actions.remove(&id);
            unregister(id);
        }
        PRESSED.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.unregister();
    }
}

#[cfg(windows)]
fn register(id: i32, hotkey: Hotkey, repeats: bool) -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, RegisterHotKey,
    };
    let mut modifiers = if repeats { 0 } else { MOD_NOREPEAT };
    if hotkey.ctrl {
        modifiers |= MOD_CONTROL;
    }
    if hotkey.alt {
        modifiers |= MOD_ALT;
    }
    if hotkey.shift {
        modifiers |= MOD_SHIFT;
    }
    if hotkey.win {
        modifiers |= MOD_WIN;
    }
    unsafe { RegisterHotKey(std::ptr::null_mut(), id, modifiers, u32::from(hotkey.key)) != 0 }
}

#[cfg(windows)]
fn unregister(id: i32) {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::UnregisterHotKey;
    unsafe {
        UnregisterHotKey(std::ptr::null_mut(), id);
    }
}

#[cfg(not(windows))]
fn register(_id: i32, _hotkey: Hotkey, _repeats: bool) -> bool {
    false
}

#[cfg(not(windows))]
fn unregister(_id: i32) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(ctrl: bool, alt: bool, shift: bool, win: bool, key: u16) -> Hotkey {
        Hotkey {
            ctrl,
            alt,
            shift,
            win,
            key,
        }
    }

    #[test]
    fn retired_ids_do_not_select_actions_in_a_new_generation() {
        let mut ids = HotkeyIds::new();
        let old = ids.allocate().unwrap();
        ids.actions.insert(old, Action::Profile(0));
        assert_eq!(ids.action(old), Some(Action::Profile(0)));
        ids.actions.remove(&old);
        let new = ids.allocate().unwrap();
        ids.actions.insert(new, Action::TurnOff);
        assert_ne!(old, new);
        assert_eq!(ids.action(old), None);
        assert_eq!(ids.action(new), Some(Action::TurnOff));
        assert_eq!(ids.action(-1), None);
        assert_eq!(ids.action(HOTKEY_ID_MAX + 1), None);
    }

    #[test]
    fn id_allocation_wraps_and_skips_active_registrations() {
        let mut ids = HotkeyIds::new();
        ids.next = HOTKEY_ID_MAX;
        ids.actions.insert(HOTKEY_ID_MAX, Action::BrightnessUp);
        ids.actions.insert(0, Action::BrightnessDown);
        assert_eq!(ids.allocate(), Some(1));
        assert_eq!(ids.action(HOTKEY_ID_MAX), Some(Action::BrightnessUp));
        ids.actions = (0..=HOTKEY_ID_MAX)
            .map(|id| (id, Action::TurnOff))
            .collect();
        assert_eq!(ids.allocate(), None);
    }

    #[test]
    fn requires_a_modifier_except_for_extended_function_keys() {
        assert!(key(true, true, false, false, 0x4D).is_valid());
        assert!(key(false, false, false, true, 0x4D).is_valid());
        assert!(!key(false, false, true, false, 0x41).is_valid());
        assert!(!key(false, false, false, false, 0x74).is_valid());
        assert!(key(false, false, false, false, 0x7C).is_valid());
        assert!(!key(true, false, false, false, 0x11).is_valid());
    }

    #[test]
    fn labels_letters_digits_and_function_keys() {
        assert_eq!(key(true, true, false, false, 0x4D).label(), "Ctrl+Alt+M");
        assert_eq!(key(false, false, true, true, 0x31).label(), "Shift+Win+1");
        assert_eq!(key(false, false, false, false, 0x87).label(), "F24");
    }
}
