//! Ambient light acquisition off the GPUI thread, with perceptual smoothing.

use std::sync::atomic::{AtomicBool, Ordering};

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Enable/disable the tray-owned watcher live. Disabled by default.
pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Release);
    crate::tray::wake();
}

pub(crate) fn is_enabled() -> bool {
    ENABLED.load(Ordering::Acquire)
}

/// Cached presence check. The first call waits for a short probe on a dedicated
/// MTA thread; subsequent calls do not perform any WinRT or hardware work.
/// A successful watcher start also refreshes this cache (e.g. after hotplug).
pub fn available() -> bool {
    #[cfg(windows)]
    {
        platform::available()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// What [`available`] found, without probing: `None` before the first probe.
/// Safe on the UI thread.
pub fn available_cached() -> Option<bool> {
    #[cfg(windows)]
    {
        platform::available_cached()
    }
    #[cfg(not(windows))]
    {
        Some(false)
    }
}

/// Probe again in the background (a sensor may have appeared or its driver
/// recovered) and repaint the windows when the answer changes. Never blocks.
pub fn refresh_availability() {
    #[cfg(windows)]
    platform::refresh_availability();
}

/// A light subscription. Create/drop on the same non-UI thread (normally tray).
/// Drop suppresses in-flight callbacks, revokes the event and releases WinRT.
pub struct Watcher {
    #[cfg(windows)]
    _inner: platform::Subscription,
}

impl Watcher {
    /// Subscribe at max(sensor minimum, 1000 ms), publishing smoothed lux via
    /// `PopupCommand::AmbientLight`. `None` means no default sensor exists.
    /// This performs WinRT I/O: call on the tray/worker thread, never GPUI.
    pub fn start() -> anyhow::Result<Option<Self>> {
        #[cfg(windows)]
        {
            platform::Subscription::start().map(|inner| inner.map(|_inner| Self { _inner }))
        }
        #[cfg(not(windows))]
        {
            Ok(None)
        }
    }
}

/// Log-space EMA (alpha=0.2), 15% hysteresis and a five-reading stable dwell.
/// Use once per sensor report (~1 s). Invalid samples are ignored. `update`
/// returns only values worth applying; construct a fresh smoother to reset it.
#[derive(Default, Debug)]
pub struct Smoother {
    log_ema: Option<f32>,
    reported: Option<f32>,
    stable_anchor: f32,
    stable_samples: u8,
}

impl Smoother {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(&mut self, lux: f32) -> Option<f32> {
        if !lux.is_finite() || lux < 0.0 {
            return None;
        }
        // ln(1 + lux) handles complete darkness without a singularity.
        let sample = lux.ln_1p();
        let Some(previous) = self.log_ema else {
            self.log_ema = Some(sample);
            self.reported = Some(sample);
            self.stable_anchor = sample;
            return Some(lux);
        };
        let filtered = previous + 0.2 * (sample - previous);
        self.log_ema = Some(filtered);
        // Compare the raw samples as well as the EMA: ongoing jitter must not
        // accidentally qualify as a stable small lighting change.
        if (sample - self.stable_anchor).abs() <= 1.02_f32.ln() {
            self.stable_samples = self.stable_samples.saturating_add(1);
        } else {
            self.stable_anchor = sample;
            self.stable_samples = 1;
        }
        let delta = (filtered - self.reported.unwrap_or(filtered)).abs();
        if delta > 1.15_f32.ln() || (self.stable_samples >= 5 && delta > 1.02_f32.ln()) {
            self.reported = Some(filtered);
            self.stable_samples = 0;
            Some(filtered.exp_m1().clamp(0.0, f32::MAX))
        } else {
            None
        }
    }
}

/// Logarithmic hardware-percent curve: <=5 lux is dark, >=1000 lux is bright.
/// Levels are clamped to 0..=100; reversed endpoints produce a decreasing curve.
/// NaN/negative lux uses the dark endpoint; positive infinity uses bright.
pub fn lux_to_level(lux: f32, dark_level: u8, bright_level: u8) -> u8 {
    let dark = f32::from(dark_level.min(100));
    let bright = f32::from(bright_level.min(100));
    let fraction = if lux.is_nan() || lux <= 5.0 {
        0.0
    } else {
        (lux.min(1000.0) / 5.0).ln() / 200.0_f32.ln()
    };
    (dark + (bright - dark) * fraction).round() as u8
}

#[cfg(windows)]
mod platform {
    use super::Smoother;
    use anyhow::{Context, Result};
    use std::sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU8, Ordering},
    };
    use windows::{
        Devices::Sensors::LightSensor,
        Win32::{
            Foundation::{RPC_E_CHANGED_MODE, S_OK},
            System::WinRT::{
                RO_INIT_MULTITHREADED, RO_INIT_SINGLETHREADED, RoInitialize, RoUninitialize,
            },
        },
    };

    // 0 = not probed, 1 = absent/unavailable, 2 = present.
    static AVAILABLE: AtomicU8 = AtomicU8::new(0);

    pub(super) fn available_cached() -> Option<bool> {
        match AVAILABLE.load(Ordering::Acquire) {
            0 => None,
            state => Some(state == 2),
        }
    }

    pub(super) fn available() -> bool {
        if AVAILABLE.load(Ordering::Acquire) == 0 {
            static PROBE: OnceLock<()> = OnceLock::new();
            PROBE.get_or_init(|| {
                let present = std::thread::Builder::new()
                    .name("light-probe".into())
                    .spawn(|| {
                        let _apartment = Apartment::new()?;
                        default_sensor().map(|sensor| sensor.is_some())
                    })
                    .ok()
                    .and_then(|thread| thread.join().ok())
                    .and_then(Result::ok)
                    .unwrap_or(false);
                // Do not overwrite a concurrent successful watcher start.
                let _ = AVAILABLE.compare_exchange(
                    0,
                    if present { 2 } else { 1 },
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
            });
        }
        AVAILABLE.load(Ordering::Acquire) == 2
    }

    pub(super) fn refresh_availability() {
        use std::sync::atomic::AtomicBool;
        static RUNNING: AtomicBool = AtomicBool::new(false);
        if RUNNING.swap(true, Ordering::AcqRel) {
            return;
        }
        let spawned = std::thread::Builder::new()
            .name("light-reprobe".into())
            .spawn(|| {
                let present = Apartment::new()
                    .and_then(|_apartment| default_sensor().map(|sensor| sensor.is_some()))
                    .unwrap_or(false);
                let state = if present { 2 } else { 1 };
                if AVAILABLE.swap(state, Ordering::AcqRel) != state {
                    crate::popup::send_command(crate::popup::PopupCommand::Run(Box::new(|cx| {
                        cx.refresh_windows()
                    })));
                }
                RUNNING.store(false, Ordering::Release);
            });
        if spawned.is_err() {
            RUNNING.store(false, Ordering::Release);
        }
    }

    fn default_sensor() -> Result<Option<LightSensor>> {
        match LightSensor::GetDefault() {
            Ok(sensor) => Ok(Some(sensor)),
            // This pinned windows-rs uses Error::empty() (S_OK) for a null
            // interface, rather than E_POINTER used by older releases.
            Err(error) if error.code() == S_OK => Ok(None),
            Err(error) => Err(error).context("get default ambient light sensor"),
        }
    }

    struct Apartment {
        // RoUninitialize must run on the initializing thread.
        _thread: std::marker::PhantomData<std::rc::Rc<()>>,
    }

    impl Apartment {
        fn new() -> Result<Self> {
            // SAFETY: balanced by Drop, including S_FALSE. If tray-icon has
            // initialized STA COM, explicitly initialize matching STA WinRT.
            let mut result = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
            if result == RPC_E_CHANGED_MODE {
                result = unsafe { RoInitialize(RO_INIT_SINGLETHREADED) };
            }
            result.ok().context("initialize sensor WinRT apartment")?;
            Ok(Self {
                _thread: std::marker::PhantomData,
            })
        }
    }

    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe { RoUninitialize() };
        }
    }

    pub(super) struct Subscription {
        state: Arc<Mutex<Option<Smoother>>>,
        // Fields drop in declaration order: revoke and release before WinRT.
        _event: windows::core::EventRevoker,
        sensor: LightSensor,
        _apartment: Apartment,
    }

    impl Subscription {
        pub(super) fn start() -> Result<Option<Self>> {
            let apartment = Apartment::new()?;
            let Some(sensor) = default_sensor()? else {
                AVAILABLE.store(1, Ordering::Release);
                return Ok(None);
            };
            AVAILABLE.store(2, Ordering::Release);
            let interval = sensor.MinimumReportInterval()?.max(1000);
            let state = Arc::new(Mutex::new(Some(Smoother::new())));
            let callback_state = state.clone();
            let event = sensor
                .ReadingChanged(move |_, args| {
                    let lux = args
                        .as_ref()
                        .and_then(|args| args.Reading().ok())
                        .and_then(|reading| reading.IlluminanceInLux().ok());
                    if let Some(lux) = lux {
                        publish(&callback_state, lux);
                    }
                })
                .context("subscribe to ambient light readings")?;
            // Assemble the guard first so a failed interval change also revokes.
            let subscription = Self {
                state,
                _event: event,
                sensor,
                _apartment: apartment,
            };
            subscription
                .sensor
                .SetReportInterval(interval)
                .context("set ambient light report interval")?;
            // Some sensors emit only changes: seed the model immediately.
            if let Ok(reading) = subscription.sensor.GetCurrentReading()
                && let Ok(lux) = reading.IlluminanceInLux()
            {
                publish(&subscription.state, lux);
            }
            Ok(Some(subscription))
        }
    }

    fn publish(state: &Mutex<Option<Smoother>>, lux: f32) {
        if let Ok(mut state) = state.lock()
            && let Some(smoother) = state.as_mut()
            && let Some(lux) = smoother.update(lux)
        {
            crate::popup::send_command(crate::popup::PopupCommand::AmbientLight { lux });
        }
    }

    impl Drop for Subscription {
        fn drop(&mut self) {
            // Serialize with publish so no callback sends after this guard drops.
            if let Ok(mut state) = self.state.lock() {
                *state = None;
            }
            let _ = self.sensor.SetReportInterval(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curve_has_clamped_endpoints_and_is_monotonic() {
        for lux in [f32::NAN, -10.0, 0.0, 5.0] {
            assert_eq!(lux_to_level(lux, 10, 90), 10);
        }
        for lux in [1000.0, 10000.0, f32::INFINITY] {
            assert_eq!(lux_to_level(lux, 10, 90), 90);
        }
        let values: Vec<_> = (0..=2000)
            .map(|lux| lux_to_level(lux as f32, 10, 90))
            .collect();
        assert!(values.windows(2).all(|pair| pair[0] <= pair[1]));
        assert_eq!(lux_to_level((5.0_f32 * 1000.0).sqrt(), 10, 90), 50);
        assert_eq!(lux_to_level(5.0, 255, 0), 100);
        assert_eq!(lux_to_level(1000.0, 90, 10), 10);
    }

    #[test]
    fn rejects_invalid_samples_without_changing_state() {
        let mut smoother = Smoother::new();
        for lux in [-1.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(smoother.update(lux).is_none());
        }
        assert_eq!(smoother.update(0.0), Some(0.0));
        assert!(smoother.update(0.0).is_none());
    }

    #[test]
    fn suppresses_jitter_and_isolated_spikes() {
        let mut smoother = Smoother::new();
        assert_eq!(smoother.update(100.0), Some(100.0));
        for lux in [105.0, 95.0].into_iter().cycle().take(60) {
            assert!(smoother.update(lux).is_none());
        }
        assert!(smoother.update(150.0).is_none());
        assert!(smoother.update(100.0).is_none());
    }

    #[test]
    fn large_changes_are_smoothed_and_small_stable_changes_eventually_report() {
        let mut smoother = Smoother::new();
        smoother.update(5.0);
        let first = smoother.update(1000.0).unwrap();
        assert!(first > 5.0 && first < 1000.0);
        let mut smoother = Smoother::new();
        smoother.update(100.0);
        for _ in 0..4 {
            assert!(smoother.update(110.0).is_none());
        }
        let stable = smoother.update(110.0).unwrap();
        assert!(stable > 100.0 && stable < 110.0);
        let mut latest = stable;
        for _ in 0..100 {
            if let Some(value) = smoother.update(110.0) {
                latest = value;
            }
        }
        assert!((latest - 110.0).abs() < 3.0);
    }

    #[test]
    fn ema_uses_log_space_and_can_return_to_darkness() {
        let mut smoother = Smoother::new();
        smoother.update(0.0);
        let first = smoother.update(1000.0).unwrap();
        let expected = (0.2 * 1001.0_f32.ln()).exp_m1();
        assert!((first - expected).abs() < 0.001);
        for _ in 0..100 {
            smoother.update(1000.0);
        }
        let mut latest = 1000.0;
        for _ in 0..100 {
            if let Some(value) = smoother.update(0.0) {
                assert!(value.is_finite() && value >= 0.0);
                latest = value;
            }
        }
        assert!(latest < 0.2);
    }
}
