//! Windows Night Light: on/off and strength, off the UI thread.
//!
//! Night Light has no public API. Its on/off state and its settings (color
//! temperature, schedule) are two CloudStore registry values that
//! `explorer.exe` watches. `win-nightlight-lib` decodes them and stamps every
//! write so Windows actually applies it. One worker thread owns the single
//! library handle the crate asks for, applies requests in order and reports
//! every change (ours, Windows Settings, Quick Settings, the Night Light
//! schedule) to the GPUI thread as [`crate::popup::PopupCommand::NightLight`].

/// Night Light as Windows Settings shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NightLight {
    pub on: bool,
    /// 0..=100, the Strength slider in Windows Settings.
    pub strength: u8,
}

/// Strength Windows uses before the slider was ever moved.
const DEFAULT_STRENGTH: u8 = 48;
/// Strength 0 is 6500 K (no tint), 100 is 1200 K (Windows' own range).
const NEUTRAL_KELVIN: u16 = 6500;
const WARMEST_KELVIN: u16 = 1200;
const KELVIN_PER_PERCENT: u16 = (NEUTRAL_KELVIN - WARMEST_KELVIN) / 100;

pub fn strength_to_kelvin(strength: u8) -> u16 {
    NEUTRAL_KELVIN - KELVIN_PER_PERCENT * u16::from(strength.min(100))
}

pub fn kelvin_to_strength(kelvin: i32) -> u8 {
    let kelvin = kelvin.clamp(i32::from(WARMEST_KELVIN), i32::from(NEUTRAL_KELVIN));
    let warmth = i32::from(NEUTRAL_KELVIN) - kelvin;
    let per = i32::from(KELVIN_PER_PERCENT);
    ((warmth + per / 2) / per) as u8
}

#[derive(Clone, Copy, Debug)]
enum Request {
    SetOn(bool),
    Toggle,
    SetStrength(u8),
}

/// Start the worker. It reports the current Night Light (or `None` where
/// Windows has none) right away and after every change.
pub fn start() {
    #[cfg(windows)]
    platform::start();
}

/// Turn Night Light on or off until its schedule (if any) changes it again.
pub fn set_on(on: bool) {
    send(Request::SetOn(on));
}

pub fn toggle() {
    send(Request::Toggle);
}

/// Change the strength. Calls may come at slider rate: the worker coalesces
/// them into one held preview, as dragging the Windows Settings slider does.
pub fn set_strength(strength: u8) {
    send(Request::SetStrength(strength.min(100)));
}

fn send(request: Request) {
    #[cfg(windows)]
    platform::send(request);
    #[cfg(not(windows))]
    let _ = request;
}

fn report(night_light: Option<NightLight>) {
    crate::popup::send_command(crate::popup::PopupCommand::NightLight(night_light));
}

#[cfg(windows)]
mod platform {
    use super::{NightLight, Request};
    use std::os::windows::io::AsRawHandle;
    use std::sync::OnceLock;
    use std::sync::mpsc::{Receiver, Sender};
    use std::time::{Duration, Instant};
    use win_nightlight_lib::{ColorTemperature, Nightlight};
    use win_nightlight_watch::Watcher;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::Threading::{
        CreateEventW, INFINITE, SetEvent, WaitForMultipleObjects,
    };

    /// Minimum spacing of preview writes while the strength slider moves.
    /// Every write moves the settings timestamp 2 s ahead, so this also caps
    /// how far a long drag pushes it past real time.
    const PREVIEW_INTERVAL: Duration = Duration::from_millis(50);
    /// Preview stays on this long after the last strength change; well above
    /// the ~9 ms Windows needs to apply it even after an Explorer restart.
    const PREVIEW_HOLD: Duration = Duration::from_millis(300);
    /// Retry interval while the Night Light registry keys do not exist yet
    /// (Windows creates them the first time Night Light settings open).
    const RETRY: Duration = Duration::from_secs(60);

    struct Worker {
        sender: Sender<Request>,
        /// Auto-reset event that wakes the worker for a new request.
        wake: usize,
    }

    static WORKER: OnceLock<Worker> = OnceLock::new();

    pub(super) fn start() {
        WORKER.get_or_init(|| {
            let (sender, receiver) = std::sync::mpsc::channel();
            // SAFETY: plain unnamed auto-reset event, kept for the process
            // lifetime.
            let wake = unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) } as usize;
            let spawned = std::thread::Builder::new()
                .name("night-light".into())
                .spawn(move || run(&receiver, wake as HANDLE));
            if let Err(error) = spawned {
                crate::logger::error(format!("could not start the Night Light worker: {error}"));
            }
            Worker { sender, wake }
        });
    }

    pub(super) fn send(request: Request) {
        if let Some(worker) = WORKER.get()
            && worker.sender.send(request).is_ok()
        {
            // SAFETY: the event handle lives for the process lifetime.
            unsafe { SetEvent(worker.wake as HANDLE) };
        }
    }

    fn log(message: impl std::fmt::Display) {
        crate::logger::info(format!("[night-light] {message}"));
    }

    struct Watchers {
        settings: Watcher,
        state: Watcher,
    }

    impl Watchers {
        fn open() -> win_nightlight_watch::Result<Self> {
            Ok(Self {
                settings: Watcher::settings()?,
                state: Watcher::state()?,
            })
        }

        /// Consume pending notifications. `Err` means the watchers must be
        /// recreated.
        fn drain(&self) -> Result<bool, ()> {
            let mut changed = false;
            for watcher in [&self.settings, &self.state] {
                match watcher.read_pending() {
                    Ok(snapshot) => changed |= snapshot.is_some(),
                    // Registration survives a failed read; the next read
                    // below picks up the value.
                    Err(error) if error.kind() == win_nightlight_watch::ErrorKind::Read => {
                        changed = true;
                    }
                    Err(error) => {
                        log(format!("Night Light watcher failed: {error}"));
                        return Err(());
                    }
                }
            }
            Ok(changed)
        }
    }

    fn read(handle: &Nightlight) -> Option<NightLight> {
        let state = handle.read_state().ok()?;
        if !state.is_usable() {
            return None;
        }
        let strength = handle
            .read_settings()
            .ok()?
            .color_temperature()
            .map_or(super::DEFAULT_STRENGTH, |kelvin| {
                super::kelvin_to_strength(i32::from(kelvin))
            });
        Some(NightLight {
            on: state.is_active(),
            strength,
        })
    }

    /// A strength change in flight.
    #[derive(Default)]
    struct Preview {
        /// The latest requested strength not written yet.
        pending: Option<u8>,
        /// When the last preview write landed, while preview is on.
        written: Option<Instant>,
    }

    impl Preview {
        /// Write what is due and return how long to wait for the next step.
        fn step(&mut self, handle: &Nightlight) -> Option<Duration> {
            let now = Instant::now();
            if let Some(strength) = self.pending {
                let due = self
                    .written
                    .map_or(now, |written| written + PREVIEW_INTERVAL);
                if now < due {
                    return Some(due - now);
                }
                self.pending = None;
                let kelvin = super::strength_to_kelvin(strength);
                match ColorTemperature::new(kelvin)
                    .and_then(|temperature| handle.preview_color_temperature(temperature))
                {
                    Ok(_) => self.written = Some(now),
                    Err(error) => log(format!("could not set strength {strength}: {error}")),
                }
            }
            let written = self.written?;
            let end = written + PREVIEW_HOLD;
            if now < end {
                return Some(end - now);
            }
            self.written = None;
            if let Err(error) = handle.end_preview() {
                log(format!("could not end the strength preview: {error}"));
            }
            None
        }
    }

    fn run(receiver: &Receiver<Request>, wake: HANDLE) {
        let handle = Nightlight::new().with_startup_margin();
        let mut watchers = Watchers::open()
            .inspect_err(|error| log(format!("not watching Night Light yet: {error}")))
            .ok();
        let mut reported = None;
        let mut preview = Preview::default();
        let mut refresh = true;
        loop {
            if refresh {
                refresh = false;
                let current = read(&handle);
                if reported != Some(current) {
                    reported = Some(current);
                    super::report(current);
                }
            }
            let mut timeout = preview.step(&handle);
            if watchers.is_none() {
                timeout = Some(timeout.map_or(RETRY, |timeout| timeout.min(RETRY)));
            }
            let mut handles = vec![wake];
            if let Some(watchers) = &watchers {
                handles.push(watchers.settings.as_raw_handle() as HANDLE);
                handles.push(watchers.state.as_raw_handle() as HANDLE);
            }
            let millis = timeout.map_or(INFINITE, |timeout| {
                u32::try_from(timeout.as_millis().max(1)).unwrap_or(INFINITE - 1)
            });
            // SAFETY: every handle stays open for the duration of the wait.
            unsafe { WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 0, millis) };

            match &watchers {
                Some(open) => match open.drain() {
                    Ok(changed) => refresh |= changed,
                    Err(()) => {
                        watchers = None;
                        refresh = true;
                    }
                },
                None => {
                    watchers = Watchers::open().ok();
                    refresh |= watchers.is_some();
                }
            }
            loop {
                match receiver.try_recv() {
                    Ok(request) => {
                        refresh |= apply(&handle, request, &mut preview);
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
                }
            }
        }
    }

    /// Returns whether the state should be read back now.
    fn apply(handle: &Nightlight, request: Request, preview: &mut Preview) -> bool {
        let result = match request {
            Request::SetOn(on) => handle.set_active(on).map(|_| ()),
            Request::Toggle => handle.toggle().map(|_| ()),
            Request::SetStrength(strength) => {
                preview.pending = Some(strength);
                return false;
            }
        };
        if let Err(error) = result {
            log(format!("could not switch Night Light: {error}"));
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strength_matches_windows_kelvin_range() {
        assert_eq!(strength_to_kelvin(0), 6500);
        assert_eq!(strength_to_kelvin(100), 1200);
        assert_eq!(strength_to_kelvin(48), 3956);
        for strength in 0..=100 {
            assert_eq!(
                kelvin_to_strength(i32::from(strength_to_kelvin(strength))),
                strength
            );
        }
        assert_eq!(kelvin_to_strength(9000), 0);
        assert_eq!(kelvin_to_strength(800), 100);
    }
}
