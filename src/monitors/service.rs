//! The coordinator never waits for a worker or destroys its handles. Each worker
//! has at most one outstanding request; latest levels wait in the coordinator.
//! Windows provides no safe cancellation for a blocked DXVA2 call. A timed-out
//! worker remains parked with its handle, and a queued refresh reacquires it
//! after the driver returns. Repeated refreshes never spawn more stuck threads.
use super::{enumerate::Target, *};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Level {
    Brightness,
    Sdr,
    Contrast,
}
/// Clock values are supplied by the coordinator, making coalescing deterministic.
#[derive(Default)]
struct Throttle {
    pending: HashMap<Level, u8>,
    last: HashMap<Level, Duration>,
}
impl Throttle {
    fn set(&mut self, target: Level, percent: u8) {
        self.pending.insert(target, percent.min(100));
    }
    fn take(&mut self, now: Duration, interval: Duration) -> Option<(Level, u8)> {
        let target = [Level::Brightness, Level::Sdr, Level::Contrast]
            .into_iter()
            .filter(|t| {
                self.pending.contains_key(t)
                    && self
                        .last
                        .get(t)
                        .is_none_or(|last| now.saturating_sub(*last) >= interval)
            })
            .min_by_key(|t| self.last.get(t).copied())?;
        self.last.insert(target, now);
        self.pending.remove(&target).map(|value| (target, value))
    }
}
enum Work {
    Probe(Box<Target>, Arc<Vec<Target>>, Option<u8>),
    Capabilities,
    Features(Vec<u8>),
    /// Re-read levels through the open handles, keeping the last known value
    /// of any read that fails.
    Read,
    Write(Level, u8),
    /// VCP 0xD6: `true` = on (1), `false` = DPM off (4). DPM off keeps the
    /// monitor listening on DDC/CI, so it can be switched back on.
    Power(bool),
    /// Switch HDR on or off.
    Hdr(bool),
    /// VCP 0x60: switch to this input source.
    Input(u8),
    Action(crate::actions::Write, Instant),
}
struct Reply {
    id: String,
    operation: u64,
    monitor: Monitor,
    applied: Option<(Option<u8>, Option<u8>, Instant, crate::actions::Control)>,
}
struct Request {
    operation: u64,
    work: Work,
}
struct Slot {
    monitor: Monitor,
    sender: Sender<Request>,
    busy: Option<(u64, Instant, Duration)>,
    throttle: Throttle,
    ordered: std::collections::VecDeque<Work>,
    refresh: Option<(Target, Arc<Vec<Target>>)>,
    /// Last enumeration data, retained for a single-display re-probe.
    discovery: Option<(Target, Arc<Vec<Target>>)>,
    capabilities: bool,
    features: Option<Vec<u8>>,
    /// Requested DDC power state, not yet written.
    power: Option<bool>,
    /// Requested HDR state, not yet written.
    hdr: Option<bool>,
    /// Requested input source, not yet written.
    input: Option<u8>,
    /// Levels should be re-read once pending writes have landed.
    read: bool,
    awaiting_probe: bool,
    active: bool,
}
impl Slot {
    fn discard_pending(&mut self) {
        self.throttle.pending.clear();
        self.ordered.clear();
        self.power = None;
        self.hdr = None;
        self.input = None;
    }

    fn inventory_ready(&self) -> bool {
        self.monitor.unresponsive
            || (!self.awaiting_probe
                && self.refresh.is_none()
                && !self.capabilities
                && self.busy.is_none())
    }

    fn enqueue_action(&mut self, write: crate::actions::Write, issued: Instant) {
        // Drain earlier slider/control writes before the
        // sequence. Ordered actions themselves never coalesce.
        for level in [Level::Brightness, Level::Sdr, Level::Contrast] {
            if let Some(value) = self.throttle.pending.remove(&level) {
                self.ordered.push_back(Work::Write(level, value));
            }
        }
        if let Some(on) = self.power.take() {
            self.ordered.push_back(Work::Power(on));
        }
        if let Some(on) = self.hdr.take() {
            self.ordered.push_back(Work::Hdr(on));
        }
        if let Some(value) = self.input.take() {
            self.ordered.push_back(Work::Input(value));
        }
        let work = Work::Action(write, issued);
        self.ordered.push_back(work);
    }
    fn reprobe(&mut self) {
        self.refresh = self.discovery.clone();
        self.read = false;
        self.awaiting_probe = !self.monitor.unresponsive;
        self.capabilities = true;
    }
    /// Nothing in flight and nothing waiting to be dispatched.
    fn idle(&self) -> bool {
        self.busy.is_none()
            && self.throttle.pending.is_empty()
            && self.ordered.is_empty()
            && self.refresh.is_none()
            && self.features.is_none()
            && !self.capabilities
            && self.power.is_none()
            && self.hdr.is_none()
            && self.input.is_none()
            && !self.read
    }
    fn expire(&mut self, now: Instant) -> bool {
        if self
            .busy
            .is_some_and(|(_, started, timeout)| now.saturating_duration_since(started) >= timeout)
            && !self.monitor.unresponsive
        {
            self.monitor.unresponsive = true;
            self.awaiting_probe = false;
            log(format!("Monitor operation timed out: {}", self.monitor.id));
            return true;
        }
        false
    }
    fn dispatch(&mut self, operation: u64, work: Work, timeout: Duration) {
        if self.sender.send(Request { operation, work }).is_ok() {
            self.busy = Some((operation, Instant::now(), timeout));
        } else {
            self.monitor.unresponsive = true;
            self.awaiting_probe = false;
        }
    }
}

fn discard_pending_for(slots: &mut HashMap<MonitorId, Slot>, ids: &[MonitorId]) {
    for id in ids {
        if let Some(slot) = slots.get_mut(id) {
            slot.discard_pending();
        }
    }
}
/// VCP code of the contrast control.
const CONTRAST: u8 = 0x12;
/// VCP code of the input source select.
const INPUT: u8 = 0x60;
/// Hardware handles a worker keeps between requests.
struct Handles {
    target: Target,
    ddc: Option<ddc::Ddc>,
    wmi: Option<wmi::Wmi>,
    /// DDC maximum of the brightness code.
    max: u32,
    /// DDC maximum of the contrast code.
    contrast_max: u32,
    /// Exact normalized value retained between ordered actions; hardware
    /// quantization must not drift a cycle or repeated one-percent offsets.
    slider: Option<(u8, u8, u8)>,
    power_mode: u8,
    features: Vec<u8>,
    slider_curve: Vec<crate::levels::CalibrationPoint>,
}
/// Scale a DDC reading to percent.
fn percent(current: u32, max: u32) -> u8 {
    (u64::from(current) * 100 / u64::from(max.max(1))).min(100) as u8
}
/// Try the override once, then keep the normal fallback order.
fn probe_brightness(
    model: &str,
    vcp: Option<u8>,
    mut read: impl FnMut(u8) -> Option<(u32, u32)>,
) -> Option<(u8, u32, u32)> {
    vcp.into_iter()
        .chain(
            rules::brightness_codes(model)
                .into_iter()
                .filter(|code| Some(*code) != vcp),
        )
        .find_map(|code| read(code).map(|(current, maximum)| (code, current, maximum)))
}

fn set_brightness_vcp(
    overrides: &mut HashMap<MonitorId, u8>,
    slots: &mut HashMap<MonitorId, Slot>,
    id: MonitorId,
    vcp: Option<u8>,
) {
    if overrides.get(&id).copied() == vcp {
        return;
    }
    if let Some(code) = vcp {
        overrides.insert(id.clone(), code);
    } else {
        overrides.remove(&id);
    }
    if let Some(slot) = slots.get_mut(&id).filter(|slot| slot.active) {
        slot.reprobe();
    }
}

/// All callers pass hardware percent here. Normalization belongs to the
/// action resolver (or, for the popup/automation, their existing callers).
fn write_level(h: &mut Handles, level: Level, value: u8) -> bool {
    let scaled = |max: u32| (u64::from(value) * u64::from(max) / 100) as u32;
    let ok = match level {
        Level::Sdr => h.target.monitor.sdr.is_some() && hdr::set(&h.target, value),
        Level::Contrast => {
            h.target.monitor.contrast.is_some()
                && h.ddc
                    .as_ref()
                    .is_some_and(|d| d.set(CONTRAST, scaled(h.contrast_max)))
        }
        Level::Brightness => match h.target.monitor.backend {
            BrightnessBackend::Ddc { vcp } => {
                h.ddc.as_ref().is_some_and(|d| d.set(vcp, scaled(h.max)))
            }
            BrightnessBackend::Wmi => h
                .wmi
                .as_ref()
                .is_some_and(|w| w.set(&h.target.monitor.id, value).is_ok()),
            BrightnessBackend::Unsupported => false,
        },
    };
    if ok {
        match level {
            Level::Brightness => h.target.monitor.brightness = Some(value),
            Level::Sdr => h.target.monitor.sdr = Some(SdrLevel { percent: value }),
            Level::Contrast => h.target.monitor.contrast = Some(value),
        }
    }
    ok
}

fn execute_action(
    h: &mut Handles,
    write: &crate::actions::Write,
) -> anyhow::Result<(Option<u8>, Option<u8>)> {
    use crate::actions::{Control, Operation};
    use crate::levels;
    crate::actions::Action {
        selector: crate::cli::Selector::All,
        control: write.control.clone(),
        operation: write.operation.clone(),
    }
    .validate()?;
    let monitor = &h.target.monitor;
    anyhow::ensure!(
        !monitor.powered_off || write.control == Control::Power,
        "display is powered off"
    );
    match write.control {
        Control::Brightness | Control::Sdr | Control::Contrast => {
            let level = match write.control {
                Control::Sdr => Level::Sdr,
                Control::Contrast => Level::Contrast,
                _ if monitor.sdr.is_some()
                    && (write.sdr_primary || monitor.brightness.is_none()) =>
                {
                    Level::Sdr
                }
                _ => Level::Brightness,
            };
            let (value, hardware) = if level == Level::Brightness {
                let previous = h.slider.filter(|_| h.slider_curve == write.calibration);
                let current = previous
                    .filter(|(min, max, _)| *min == write.min && *max == write.max)
                    .map(|(_, _, value)| value)
                    .or_else(|| {
                        monitor.brightness.map(|value| {
                            levels::calibrated_from_hardware(
                                value,
                                write.min,
                                write.max,
                                &write.calibration,
                            )
                        })
                    });
                let value = write
                    .operation
                    .evaluate(current.map(i32::from), 100, None)? as u8;
                (
                    value,
                    levels::calibrated_to_hardware(value, write.min, write.max, &write.calibration),
                )
            } else {
                let current = if level == Level::Sdr {
                    monitor.sdr.map(|v| v.percent)
                } else {
                    monitor.contrast
                };
                anyhow::ensure!(current.is_some(), "control unavailable");
                let value = write
                    .operation
                    .evaluate(current.map(i32::from), 100, None)? as u8;
                (value, value)
            };
            anyhow::ensure!(write_level(h, level, hardware), "level write failed");
            if level == Level::Brightness {
                h.slider = Some((write.min, write.max, value));
                h.slider_curve = write.calibration.clone();
            }
            Ok(if level == Level::Contrast {
                (None, Some(value))
            } else if level == Level::Sdr
                && !write.sdr_primary
                && h.target.monitor.brightness.is_some()
            {
                (None, None)
            } else {
                (Some(value), None)
            })
        }
        _ => {
            let code = match write.control {
                Control::Input => INPUT,
                Control::Power => 0xd6,
                Control::Vcp(code) => code,
                _ => unreachable!(),
            };
            // Re-probing may have changed support since the UI selected this
            // display. Never carry a stale capability assumption to a write.
            anyhow::ensure!(
                match write.control {
                    Control::Input => h.ddc.is_some(),
                    Control::Power => h.ddc.is_some(),
                    Control::Vcp(_) => h.ddc.is_some(),
                    _ => false,
                },
                "control no longer supported"
            );
            let ddc = h
                .ddc
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("DDC unavailable"))?;
            let max = if code == INPUT {
                255
            } else if code == 0xd6 {
                5
            } else {
                65535
            };
            write.operation.validate(max, write.allowed.as_deref())?;
            let current = match write.operation {
                Operation::Set(_) => None,
                _ if code == INPUT => monitor.input.map(i32::from),
                // Some monitors advertise power writes but do not answer reads.
                _ if code == 0xd6 => Some(i32::from(h.power_mode)),
                _ => {
                    let current = ddc
                        .read_raw(code)
                        .ok_or_else(|| anyhow::anyhow!("VCP read failed"))?
                        .0;
                    anyhow::ensure!(current <= 65535, "invalid VCP reading");
                    Some(current as i32)
                }
            };
            let value = write
                .operation
                .evaluate(current, max, write.allowed.as_deref())?;
            anyhow::ensure!(ddc.set(code, value as u32), "VCP write not acknowledged");
            if code == INPUT {
                h.target.monitor.input = Some(value as u8);
            }
            if code == 0xd6 {
                h.target.monitor.powered_off = value != 1;
                h.power_mode = value as u8;
            }
            if let Some((_, maximum)) = h.target.monitor.vcp_values.get(&code).copied() {
                h.target
                    .monitor
                    .vcp_values
                    .insert(code, (value as u32, maximum));
            }
            // Refresh the typed state when raw VCP touched the same hardware
            // control, so the next relative action observes this write.
            let mut remembered = (None, None);
            if matches!(h.target.monitor.backend, BrightnessBackend::Ddc { vcp } if vcp == code) {
                if let Some((current, maximum)) = ddc.read(code) {
                    h.max = maximum;
                    let hardware = percent(current, maximum);
                    h.target.monitor.brightness = Some(hardware);
                    h.slider = None;
                    remembered.0 =
                        (h.target.monitor.sdr.is_none() || !write.sdr_primary).then(|| {
                            levels::calibrated_from_hardware(
                                hardware,
                                write.min,
                                write.max,
                                &write.calibration,
                            )
                        });
                } else {
                    h.target.monitor.brightness = None;
                    h.slider = None;
                }
            }
            if code == CONTRAST {
                if let Some((current, maximum)) = ddc.read(code) {
                    h.contrast_max = maximum;
                    let value = percent(current, maximum);
                    h.target.monitor.contrast = Some(value);
                    remembered.1 = Some(value);
                } else {
                    h.target.monitor.contrast = None;
                }
            }
            Ok(remembered)
        }
    }
}

fn worker(receiver: Receiver<Request>, sender: Sender<Reply>) {
    let mut state: Option<Handles> = None;
    while let Ok(request) = receiver.recv() {
        let mut applied = None;
        match request.work {
            Work::Probe(target, targets, brightness_vcp) => {
                let mut target = *target;
                // Releasing/reacquiring handles stays on this worker, behind any
                // previous call. Never close a handle still executing in a driver.
                // A re-probe cannot read back the power state; keep what the
                // last power write established.
                let was_off = state
                    .as_ref()
                    .is_some_and(|previous| previous.target.monitor.powered_off);
                target.monitor.powered_off = was_off;
                drop(state.take());
                let wmi = wmi::Wmi::connect()
                    .map_err(|e| log(format!("WMI: {e}")))
                    .ok();
                if let Some(name) = wmi.as_ref().and_then(|w| w.name(&target.monitor.id)) {
                    target.monitor.name = name;
                }
                let brightness = wmi.as_ref().and_then(|w| w.brightness(&target.monitor.id));
                let mut max = 100;
                let mut contrast_max = 100;
                let ddc = if let Some(brightness) = brightness {
                    target.monitor.kind = MonitorKind::Internal;
                    target.monitor.backend = BrightnessBackend::Wmi;
                    target.monitor.brightness = Some(brightness);
                    None
                } else {
                    let ddc = ddc::Ddc::acquire(&target, &targets);
                    if let Some(d) = &ddc {
                        if let Some((code, current, maximum)) =
                            probe_brightness(&target.monitor.model, brightness_vcp, |code| {
                                d.read(code)
                            })
                        {
                            max = maximum;
                            target.monitor.backend = BrightnessBackend::Ddc { vcp: code };
                            target.monitor.brightness = Some(percent(current, max));
                        }
                        target.monitor.contrast = d.read(CONTRAST).map(|(current, maximum)| {
                            contrast_max = maximum;
                            percent(current, maximum)
                        });
                    }
                    ddc
                };
                target.monitor.hdr_supported = hdr::supported(&target);
                target.monitor.sdr = hdr::read(&target);
                state = Some(Handles {
                    target,
                    ddc,
                    wmi,
                    max,
                    contrast_max,
                    slider: None,
                    power_mode: if was_off { 4 } else { 1 },
                    features: Vec::new(),
                    slider_curve: Vec::new(),
                });
            }
            Work::Features(codes) => {
                if let Some(h) = &mut state {
                    h.features = codes;
                    read_features(h);
                }
            }
            Work::Capabilities => {
                if let Some(h) = &mut state
                    && let Some(ddc) = &h.ddc
                {
                    let capabilities = ddc.capabilities().unwrap_or_default();
                    let monitor = &mut h.target.monitor;
                    monitor.supports_ddc_power = rules::has_power(&capabilities);
                    monitor.inputs = rules::input_sources(&capabilities);
                    monitor.vcp_codes = rules::vcp_features(&capabilities)
                        .into_iter()
                        .map(|(code, _)| code)
                        .collect();
                    // The low byte is the source; some displays set flags above it.
                    monitor.input = (!monitor.inputs.is_empty())
                        .then(|| ddc.read_raw(INPUT))
                        .flatten()
                        .map(|(current, _)| (current & 0xff) as u8)
                        .filter(|value| *value != 0);
                }
            }
            Work::Read => {
                if let Some(h) = &mut state {
                    read_features(h);
                    let brightness = match h.target.monitor.backend {
                        BrightnessBackend::Ddc { vcp } => h
                            .ddc
                            .as_ref()
                            .and_then(|d| d.read(vcp))
                            .map(|(current, maximum)| {
                                h.max = maximum;
                                percent(current, maximum)
                            }),
                        BrightnessBackend::Wmi => h
                            .wmi
                            .as_ref()
                            .and_then(|w| w.brightness(&h.target.monitor.id)),
                        BrightnessBackend::Unsupported => None,
                    };
                    if let Some(hardware) = brightness {
                        if h.slider.is_some_and(|(min, max, value)| {
                            crate::levels::calibrated_to_hardware(value, min, max, &h.slider_curve)
                                .abs_diff(hardware)
                                > 1
                        }) {
                            h.slider = None;
                        }
                        h.target.monitor.brightness = Some(hardware);
                    }
                    if h.target.monitor.contrast.is_some()
                        && let Some((current, maximum)) =
                            h.ddc.as_ref().and_then(|d| d.read(CONTRAST))
                    {
                        h.contrast_max = maximum;
                        h.target.monitor.contrast = Some(percent(current, maximum));
                    }
                    if h.target.monitor.sdr.is_some() {
                        h.target.monitor.sdr = hdr::read(&h.target).or(h.target.monitor.sdr);
                    }
                }
            }
            Work::Action(write, issued) => {
                if let Some(h) = &mut state {
                    match execute_action(h, &write) {
                        Ok((brightness, contrast)) => {
                            applied = Some((brightness, contrast, issued, write.control))
                        }
                        Err(error) => log(format!(
                            "Action on {} skipped: {error:#}",
                            h.target.monitor.id
                        )),
                    }
                }
            }
            Work::Power(on) => {
                if let Some(h) = &mut state {
                    match &h.ddc {
                        Some(ddc) if ddc.set(0xd6, if on { 1 } else { 4 }) => {
                            h.target.monitor.powered_off = !on;
                            h.power_mode = if on { 1 } else { 4 };
                        }
                        _ => log(format!("DDC power failed: {}", h.target.monitor.id)),
                    }
                }
            }
            Work::Input(value) => {
                if let Some(h) = &mut state {
                    // A display that switches away may stop answering, so a
                    // failed acknowledgement is only logged.
                    match &h.ddc {
                        Some(ddc) if ddc.set(INPUT, u32::from(value)) => {
                            h.target.monitor.input = Some(value);
                        }
                        _ => log(format!(
                            "Input switch to 0x{value:02X} not acknowledged: {}",
                            h.target.monitor.id
                        )),
                    }
                }
            }
            Work::Hdr(on) => {
                if let Some(h) = &mut state {
                    if hdr::enable(&h.target, on) {
                        h.target.monitor.sdr = hdr::read(&h.target);
                    } else {
                        log(format!("HDR switch failed: {}", h.target.monitor.id));
                    }
                }
            }
            Work::Write(level, percent) => {
                if let Some(h) = &mut state {
                    h.slider = None;
                    if !write_level(h, level, percent) {
                        if level == Level::Brightness {
                            h.target.monitor.brightness = None;
                        }
                        log(format!(
                            "Hardware write failed: {} {level:?}",
                            h.target.monitor.id
                        ));
                    }
                }
            }
        }
        if let Some(h) = &state
            && sender
                .send(Reply {
                    id: h.target.monitor.id.clone(),
                    operation: request.operation,
                    monitor: h.target.monitor.clone(),
                    applied,
                })
                .is_err()
        {
            break;
        }
    }
}

pub(super) fn start(on_event: impl Fn(Event) + Send + Sync + 'static) -> MonitorService {
    let (sender, receiver) = mpsc::channel();
    thread::Builder::new()
        .name("monitor-coordinator".into())
        .spawn(move || coordinator(receiver, Arc::new(on_event)))
        .expect("start monitor coordinator");
    MonitorService { sender }
}
fn coordinator(commands: Receiver<Command>, callback: Arc<dyn Fn(Event) + Send + Sync>) {
    let _events = events::Events::new()
        .map_err(|e| log(format!("Monitor event window: {e}")))
        .ok();
    let (replies, completed) = mpsc::channel::<Reply>();
    let mut slots = HashMap::<String, Slot>::new();
    let mut feature_codes: HashMap<MonitorId, Vec<u8>> = HashMap::new();
    let mut brightness_vcps = HashMap::<MonitorId, u8>::new();
    let mut sequence = 0u64;
    let mut throttle = Duration::from_millis(100);
    let epoch = Instant::now();
    let mut refresh = Some(SnapshotReason::Startup);
    let mut snapshot = None;
    let mut inventory_requested = false;
    let mut reasons = vec![SnapshotReason::Startup];
    let mut debounce: Option<(Instant, SnapshotReason)> = None;
    let mut windows_off_at: Option<Instant> = None;
    let mut queued = std::collections::VecDeque::new();
    // Input tick seen when every display went dark, and since when.
    let mut all_off_since: Option<(u32, Instant)> = None;
    loop {
        if windows_off_at.is_some_and(|at| Instant::now() >= at) {
            windows_off_at = None;
            events::turn_off();
        }
        for reason in events::Events::pump() {
            log(format!("Hardware event: {reason:?}"));
            let reason = if debounce
                .as_ref()
                .is_some_and(|(_, r)| *r == SnapshotReason::Resumed)
            {
                SnapshotReason::Resumed
            } else {
                reason
            };
            debounce = Some((Instant::now() + Duration::from_millis(1500), reason));
        }
        if debounce
            .as_ref()
            .is_some_and(|(at, _)| Instant::now() >= *at)
        {
            refresh = debounce.take().map(|(_, r)| r);
        }
        // Bound each drain so continuous slider input cannot starve deadlines.
        for _ in 0..256 {
            let next = match queued.pop_front() {
                Some(command) => Ok(command),
                None => commands.try_recv(),
            };
            match next {
                Ok(Command::Shutdown) | Err(mpsc::TryRecvError::Disconnected) => return,
                Err(mpsc::TryRecvError::Empty) => break,
                Ok(Command::Refresh) => refresh = Some(SnapshotReason::Requested),
                Ok(Command::ConfigureFeatures { id, codes }) => {
                    feature_codes.insert(id.clone(), codes.clone());
                    if let Some(slot) = slots.get_mut(&id) {
                        slot.features = Some(codes);
                    }
                }
                Ok(Command::Execute(writes, issued)) => {
                    for (id, write) in writes {
                        if let Some(slot) = slots
                            .get_mut(&id)
                            .filter(|s| s.active && !s.monitor.unresponsive)
                        {
                            slot.enqueue_action(write, issued);
                        }
                    }
                }
                Ok(Command::ReadLevels) => {
                    // A display in DPM off may not answer; a pending probe
                    // reads levels anyway.
                    for s in slots
                        .values_mut()
                        .filter(|s| s.active && s.refresh.is_none() && !s.monitor.powered_off)
                    {
                        s.read = true;
                    }
                }
                Ok(Command::ReadInventory) => inventory_requested = true,
                Ok(Command::SetThrottle(value)) => throttle = value,
                Ok(Command::DiscardPendingLevels) => {
                    for slot in slots.values_mut() {
                        slot.discard_pending();
                    }
                }
                Ok(Command::DiscardPendingFor(ids)) => discard_pending_for(&mut slots, &ids),
                Ok(Command::SetBrightnessVcp { id, vcp }) => {
                    set_brightness_vcp(&mut brightness_vcps, &mut slots, id, vcp);
                }
                Ok(Command::SetBrightness { id, percent }) => {
                    if let Some(s) = slots.get_mut(&id).filter(|s| s.active) {
                        s.throttle.set(Level::Brightness, percent);
                    }
                }
                Ok(Command::SetSdr { id, percent }) => {
                    if let Some(s) = slots.get_mut(&id).filter(|s| s.active) {
                        s.throttle.set(Level::Sdr, percent);
                    }
                }
                Ok(Command::SetContrast { id, percent }) => {
                    if let Some(s) = slots.get_mut(&id).filter(|s| s.active) {
                        s.throttle.set(Level::Contrast, percent);
                    }
                }
                Ok(Command::SetHdr { id, on }) => {
                    log(format!("Setting HDR of {id} to {on}"));
                    if let Some(s) = slots.get_mut(&id).filter(|s| s.active) {
                        s.hdr = Some(on);
                    }
                }
                Ok(Command::SetInput { id, value }) => {
                    log(format!("Switching input of {id} to 0x{value:02X}"));
                    if let Some(s) = slots.get_mut(&id).filter(|s| s.active) {
                        s.input = Some(value);
                    }
                }
                Ok(Command::Apply(targets)) => {
                    for t in targets {
                        if let Some(s) = slots.get_mut(&t.id).filter(|s| s.active) {
                            if let Some(p) = t.brightness {
                                s.throttle.set(Level::Brightness, p);
                            }
                            if let Some(p) = t.sdr {
                                s.throttle.set(Level::Sdr, p);
                            }
                            if let Some(p) = t.contrast {
                                s.throttle.set(Level::Contrast, p);
                            }
                        }
                    }
                }
                Ok(Command::SetPower { id, on }) => {
                    log(format!("Setting DDC power of {id} to {on}"));
                    if let Some(s) = slots.get_mut(&id).filter(|s| s.active) {
                        s.power = Some(on);
                    }
                }
                Ok(Command::TurnOff(mode)) => {
                    log(format!("Turning displays off ({mode:?})"));
                    if mode == TurnOffMode::Windows {
                        events::turn_off();
                    }
                    // Give DDC workers a head start before Windows suspends
                    // the display links, as in legacy's combined sleep path.
                    if mode == TurnOffMode::Both {
                        windows_off_at = Some(Instant::now() + Duration::from_millis(333));
                    }
                    if matches!(mode, TurnOffMode::Ddc | TurnOffMode::Both) {
                        for s in slots.values_mut().filter(|s| s.active) {
                            s.power = Some(false);
                        }
                    }
                }
            }
        }
        for reply in completed.try_iter() {
            let Some(slot) = slots.get_mut(&reply.id) else {
                continue;
            };
            if slot
                .busy
                .is_none_or(|(operation, _, _)| operation != reply.operation)
            {
                continue;
            }
            slot.busy = None;
            slot.monitor = reply.monitor;
            slot.monitor.unresponsive = false;
            if slot.refresh.is_none() {
                slot.awaiting_probe = false;
            }
            if slot.active {
                if let Some((brightness, contrast, issued, control)) = reply.applied {
                    callback(Event::ActionApplied {
                        control,
                        issued,
                        monitor: slot.monitor.clone(),
                        brightness,
                        contrast,
                    });
                } else {
                    callback(Event::Updated(slot.monitor.clone()));
                }
            }
        }
        if let Some(reason) = refresh.take() {
            if !reasons.contains(&reason) {
                reasons.push(reason);
            }
            match enumerate::enumerate() {
                Ok(targets) => {
                    let targets = Arc::new(targets);
                    // Keep disconnected busy workers parked until they finish, so
                    // reconnect/refresh cannot accumulate uncancellable driver calls.
                    slots.retain(|id, s| {
                        targets.iter().any(|t| t.monitor.id == *id) || s.busy.is_some()
                    });
                    for (id, slot) in &mut slots {
                        slot.active = targets.iter().any(|t| t.monitor.id == *id);
                        if !slot.active {
                            slot.refresh = None;
                            slot.throttle.pending.clear();
                            slot.capabilities = false;
                            slot.ordered.clear();
                            slot.power = None;
                            slot.hdr = None;
                            slot.input = None;
                            slot.read = false;
                        }
                    }
                    for target in targets.iter() {
                        let slot = slots.entry(target.monitor.id.clone()).or_insert_with(|| {
                            let (sender, receiver) = mpsc::channel();
                            let replies = replies.clone();
                            thread::Builder::new()
                                .name(format!("monitor-{}", target.monitor.id))
                                .spawn(move || worker(receiver, replies))
                                .expect("start monitor worker");
                            Slot {
                                monitor: target.monitor.clone(),
                                sender,
                                busy: None,
                                throttle: Throttle::default(),
                                ordered: Default::default(),
                                refresh: None,
                                discovery: None,
                                capabilities: false,
                                features: feature_codes.get(&target.monitor.id).cloned(),
                                power: None,
                                hdr: None,
                                input: None,
                                read: false,
                                awaiting_probe: false,
                                active: true,
                            }
                        });
                        slot.discovery = Some((target.clone(), targets.clone()));
                        slot.reprobe();
                        slot.features = feature_codes.get(&target.monitor.id).cloned();
                    }
                    snapshot = Some(
                        targets
                            .iter()
                            .map(|t| t.monitor.id.clone())
                            .collect::<Vec<_>>(),
                    );
                }
                Err(e) => {
                    log(format!("Display enumeration failed: {e}"));
                    snapshot = None;
                    for reason in reasons.drain(..) {
                        callback(Event::Snapshot {
                            monitors: sorted(&slots, None),
                            reason,
                        });
                    }
                }
            }
        }
        // Safety net: with every display switched off there is nothing left
        // to click, so the next input switches them all back on.
        let all_off = slots.values().any(|s| s.active)
            && slots
                .values()
                .filter(|s| s.active)
                .all(|s| s.monitor.powered_off);
        if all_off {
            let tick = events::last_input_tick();
            match all_off_since {
                None => all_off_since = Some((tick, Instant::now())),
                Some((seen, since))
                    if tick != seen && since.elapsed() >= Duration::from_millis(1000) =>
                {
                    log("Input while every display is off; switching them back on");
                    for s in slots.values_mut().filter(|s| s.active) {
                        s.power = Some(true);
                    }
                    all_off_since = None;
                }
                Some((_, since)) if since.elapsed() < Duration::from_millis(1000) => {
                    // Input during the grace period (releasing the click
                    // that switched them off) does not count.
                    all_off_since = Some((tick, since));
                }
                Some(_) => {}
            }
        } else {
            all_off_since = None;
        }
        for slot in slots.values_mut() {
            if slot.busy.is_some() {
                if slot.expire(Instant::now()) && slot.active {
                    callback(Event::Updated(slot.monitor.clone()));
                }
                continue;
            }
            if !slot.active {
                continue;
            }
            let (work, timeout) = if let Some((target, targets)) = slot.refresh.take() {
                slot.features = feature_codes.get(&slot.monitor.id).cloned();
                (
                    Work::Probe(
                        Box::new(target),
                        targets,
                        brightness_vcps.get(&slot.monitor.id).copied(),
                    ),
                    Duration::from_secs(15),
                )
            } else if let Some(work) = slot.ordered.pop_front() {
                (work, Duration::from_secs(5))
            } else if let Some((level, p)) = slot.throttle.take(epoch.elapsed(), throttle) {
                (Work::Write(level, p), Duration::from_secs(3))
            } else if let Some(on) = slot.power.take() {
                (Work::Power(on), Duration::from_secs(3))
            } else if let Some(on) = slot.hdr.take() {
                (Work::Hdr(on), Duration::from_secs(5))
            } else if let Some(value) = slot.input.take() {
                (Work::Input(value), Duration::from_secs(5))
            } else if slot.read && slot.throttle.pending.is_empty() {
                // After pending writes, so the read reports what they set.
                slot.read = false;
                (Work::Read, Duration::from_secs(3))
            } else if slot.capabilities {
                slot.capabilities = false;
                (Work::Capabilities, Duration::from_secs(15))
            } else if let Some(codes) = slot.features.take() {
                (Work::Features(codes), Duration::from_secs(10))
            } else {
                continue;
            };
            sequence = sequence.wrapping_add(1);
            slot.dispatch(sequence, work, timeout);
        }
        if snapshot.as_ref().is_some_and(|ids| {
            ids.iter()
                .all(|id| slots.get(id).is_none_or(|s| !s.awaiting_probe))
        }) {
            let ids = snapshot.take().expect("pending snapshot");
            for reason in reasons.drain(..) {
                callback(Event::Snapshot {
                    monitors: sorted(&slots, Some(&ids)),
                    reason,
                });
            }
        }
        if inventory_requested
            && snapshot.is_none()
            && slots
                .values()
                .filter(|slot| slot.active)
                .all(Slot::inventory_ready)
        {
            inventory_requested = false;
            callback(Event::Inventory(sorted(&slots, None)));
        }
        slots.retain(|_, slot| slot.active || slot.busy.is_some());
        // Poll quickly only while work is in flight; an idle tray app must not
        // wake the CPU 100 times a second. Commands wake the idle wait at once.
        let idle = refresh.is_none()
            && debounce.is_none()
            && windows_off_at.is_none()
            && snapshot.is_none()
            && slots.values().all(Slot::idle);
        if idle {
            match commands.recv_timeout(Duration::from_millis(250)) {
                Ok(command) => queued.push_back(command),
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        } else {
            thread::sleep(Duration::from_millis(10));
        }
    }
}

fn read_features(h: &mut Handles) {
    h.target
        .monitor
        .vcp_values
        .retain(|code, _| h.features.contains(code));
    if let Some(ddc) = &h.ddc {
        for code in &h.features {
            if let Some(value) = ddc.read_raw(*code) {
                h.target.monitor.vcp_values.insert(*code, value);
            }
        }
    }
}
fn sorted(slots: &HashMap<String, Slot>, ids: Option<&Vec<String>>) -> Vec<Monitor> {
    let mut monitors: Vec<_> = slots
        .iter()
        .filter(|(id, s)| s.active && ids.is_none_or(|ids| ids.contains(id)))
        .map(|(_, s)| s.monitor.clone())
        .collect();
    monitors.sort_by_key(|m| m.num);
    monitors
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inventory_waits_for_capability_reply_but_not_for_a_timed_out_worker() {
        let mut display = slot();
        assert!(!display.inventory_ready());
        display.awaiting_probe = false;
        display.capabilities = true;
        assert!(!display.inventory_ready());
        display.capabilities = false;
        let now = Instant::now();
        display.busy = Some((1, now, Duration::from_secs(15)));
        assert!(!display.inventory_ready());
        // A successful capability reply populates the inventory before it is ready.
        display.monitor.inputs = vec![0x11, 0x12];
        display.monitor.vcp_codes = vec![0x10, 0x60];
        display.busy = None;
        assert!(display.inventory_ready());
        assert_eq!(display.monitor.inputs, [0x11, 0x12]);
        display.busy = Some((2, now, Duration::from_secs(15)));
        assert!(display.expire(now + Duration::from_secs(15)));
        assert!(display.inventory_ready());
        assert!(display.busy.is_some());
    }

    #[test]
    fn targeted_cancellation_preserves_external_actions_and_in_flight_calls() {
        let pending = || {
            let mut display = slot();
            display.throttle.set(Level::Brightness, 80);
            display.ordered.push_back(Work::Input(0x12));
            display.power = Some(false);
            display.hdr = Some(true);
            display.input = Some(0x11);
            display.busy = Some((1, Instant::now(), Duration::from_secs(3)));
            display
        };
        let mut slots = HashMap::from([
            ("internal".into(), pending()),
            ("external".into(), pending()),
        ]);
        discard_pending_for(&mut slots, &["internal".into(), "disconnected".into()]);
        let internal = &slots["internal"];
        assert!(internal.throttle.pending.is_empty());
        assert!(internal.ordered.is_empty());
        assert!(internal.power.is_none() && internal.hdr.is_none() && internal.input.is_none());
        assert!(internal.busy.is_some());
        let external = &slots["external"];
        assert_eq!(external.throttle.pending.get(&Level::Brightness), Some(&80));
        assert!(matches!(external.ordered.front(), Some(Work::Input(0x12))));
        assert_eq!(external.power, Some(false));
        assert_eq!(external.hdr, Some(true));
        assert_eq!(external.input, Some(0x11));
    }
    #[test]
    fn action_dispatch_preserves_prior_slider_write_and_sequence_order() {
        use crate::actions::{Control, Operation, Write};
        let mut slot = slot();
        slot.throttle.set(Level::Brightness, 30);
        slot.throttle.set(Level::Brightness, 40);
        let write = |operation| Write {
            calibration: Vec::new(),
            sdr_primary: true,
            control: Control::Brightness,
            operation,
            min: 20,
            max: 80,
            allowed: None,
        };
        slot.enqueue_action(write(Operation::Set(20)), Instant::now());
        slot.enqueue_action(write(Operation::Offset(5)), Instant::now());
        slot.enqueue_action(write(Operation::Cycle(vec![25, 50])), Instant::now());
        assert!(slot.throttle.pending.is_empty());
        assert!(matches!(
            slot.ordered.pop_front(),
            Some(Work::Write(Level::Brightness, 40))
        ));
        assert!(matches!(
            slot.ordered.pop_front(),
            Some(Work::Action(
                Write {
                    operation: Operation::Set(20),
                    ..
                },
                _
            ))
        ));
        assert!(matches!(
            slot.ordered.pop_front(),
            Some(Work::Action(
                Write {
                    operation: Operation::Offset(5),
                    ..
                },
                _
            ))
        ));
        assert!(
            matches!(slot.ordered.pop_front(), Some(Work::Action(Write { operation: Operation::Cycle(v), .. }, _)) if v == vec![25, 50])
        );
        assert!(slot.ordered.is_empty());
    }

    #[test]
    fn rejected_or_failed_action_does_not_change_worker_state() {
        use crate::actions::{Control, Operation, Write};
        let mut monitor = slot().monitor;
        monitor.brightness = Some(50);
        let (target, _) = discovery(monitor.clone());
        let mut h = Handles {
            target,
            ddc: None,
            wmi: None,
            max: 100,
            contrast_max: 100,
            slider: Some((20, 80, 50)),
            power_mode: 1,
            features: Vec::new(),
            slider_curve: Vec::new(),
        };
        for operation in [Operation::Cycle(vec![50, 101]), Operation::Set(30)] {
            let result = execute_action(
                &mut h,
                &Write {
                    calibration: Vec::new(),
                    sdr_primary: true,
                    control: Control::Brightness,
                    operation,
                    min: 20,
                    max: 80,
                    allowed: None,
                },
            );
            assert!(result.is_err());
            assert_eq!(h.target.monitor, monitor);
            assert_eq!(h.slider, Some((20, 80, 50)));
        }
        h.target.monitor.powered_off = true;
        assert!(
            execute_action(
                &mut h,
                &Write {
                    calibration: Vec::new(),
                    sdr_primary: true,
                    control: Control::Brightness,
                    operation: Operation::Set(10),
                    min: 0,
                    max: 100,
                    allowed: None
                }
            )
            .is_err()
        );
        assert_eq!(h.target.monitor.brightness, Some(50));
    }

    fn slot() -> Slot {
        let (sender, _) = mpsc::channel();
        Slot {
            monitor: Monitor {
                vcp_values: Default::default(),
                id: "TEST".into(),
                model: "TEST".into(),
                device_path: String::new(),
                name: "Test".into(),
                kind: MonitorKind::External,
                num: 1,
                bounds: None,
                backend: BrightnessBackend::Unsupported,
                brightness: None,
                sdr: None,
                hdr_supported: false,
                contrast: None,
                supports_ddc_power: false,
                inputs: Vec::new(),
                vcp_codes: Vec::new(),
                input: None,
                powered_off: false,
                unresponsive: false,
            },
            sender,
            busy: None,
            throttle: Throttle::default(),
            ordered: Default::default(),
            refresh: None,
            discovery: None,
            capabilities: false,
            features: None,
            power: None,
            hdr: None,
            input: None,
            read: false,
            awaiting_probe: true,
            active: true,
        }
    }
    fn discovery(monitor: Monitor) -> (Target, Arc<Vec<Target>>) {
        let target = Target {
            monitor,
            adapter: Default::default(),
            target: 0,
            source: String::new(),
        };
        (target.clone(), Arc::new(vec![target]))
    }
    #[test]
    fn brightness_override_precedes_rules_and_falls_back_without_duplicates() {
        let mut tried = Vec::new();
        let result = probe_brightness("FUS087C", Some(0x13), |code| {
            tried.push(code);
            (code == 0x10).then_some((30, 100))
        });
        assert_eq!(tried, [0x13, 0x6b, 0x10]);
        assert_eq!(result, Some((0x10, 30, 100)));
        tried.clear();
        assert_eq!(
            probe_brightness("unknown", Some(0x10), |code| {
                tried.push(code);
                None
            }),
            None
        );
        assert_eq!(tried, [0x10, 0x13, 0x6b, 0x12]);
        assert_eq!(
            probe_brightness("unknown", Some(0xe1), |_| Some((5, 10))),
            Some((0xe1, 5, 10))
        );
    }
    #[test]
    fn override_changes_reprobe_only_the_slot_and_survive_disconnect() {
        let mut a = slot();
        a.discovery = Some(discovery(a.monitor.clone()));
        a.busy = Some((7, Instant::now(), Duration::from_secs(15)));
        let mut slots = HashMap::from([("a".into(), a), ("b".into(), slot())]);
        let mut overrides = HashMap::new();
        set_brightness_vcp(&mut overrides, &mut slots, "a".into(), Some(0x13));
        assert!(slots["a"].refresh.is_some());
        assert!(slots["a"].busy.is_some());
        assert!(slots["b"].refresh.is_none());
        slots.get_mut("a").unwrap().refresh = None;
        set_brightness_vcp(&mut overrides, &mut slots, "a".into(), Some(0x13));
        assert!(slots["a"].refresh.is_none());
        slots.remove("a");
        assert_eq!(overrides.get("a"), Some(&0x13));
        let mut reconnected = slot();
        reconnected.discovery = Some(discovery(reconnected.monitor.clone()));
        reconnected.reprobe();
        slots.insert("a".into(), reconnected);
        set_brightness_vcp(&mut overrides, &mut slots, "a".into(), None);
        assert!(!overrides.contains_key("a"));
        assert!(slots["a"].refresh.is_some());
    }
    #[test]
    fn stuck_worker_expires_once_without_affecting_another_monitor() {
        let start = Instant::now();
        let mut hung = slot();
        let mut healthy = slot();
        hung.busy = Some((1, start, Duration::from_secs(3)));
        healthy.busy = Some((2, start, Duration::from_secs(15)));
        assert!(!hung.expire(start + Duration::from_millis(2999)));
        assert!(hung.expire(start + Duration::from_secs(3)));
        assert!(hung.monitor.unresponsive);
        assert!(!hung.awaiting_probe);
        assert!(!hung.expire(start + Duration::from_secs(4)));
        assert!(!healthy.expire(start + Duration::from_secs(4)));
        assert!(!healthy.monitor.unresponsive);
        // The driver still owns an in-flight operation; timeout must not
        // pretend it completed or allow concurrent handle destruction.
        assert!(hung.busy.is_some());
    }
    #[test]
    fn changing_throttle_preserves_pending_targets_and_does_not_starve_sdr() {
        let mut t = Throttle::default();
        t.set(Level::Brightness, 1);
        assert_eq!(
            t.take(Duration::ZERO, Duration::ZERO),
            Some((Level::Brightness, 1))
        );
        t.set(Level::Brightness, 2);
        t.set(Level::Sdr, 55);
        assert_eq!(
            t.take(Duration::from_millis(1), Duration::ZERO),
            Some((Level::Sdr, 55))
        );
        assert_eq!(
            t.take(Duration::from_millis(1), Duration::from_secs(1)),
            None
        );
        assert_eq!(
            t.take(Duration::from_millis(2), Duration::ZERO),
            Some((Level::Brightness, 2))
        );
    }
    #[test]
    fn latest_wins_and_final_is_not_lost() {
        let mut t = Throttle::default();
        let interval = Duration::from_millis(100);
        t.set(Level::Brightness, 10);
        assert_eq!(
            t.take(Duration::ZERO, interval),
            Some((Level::Brightness, 10))
        );
        t.set(Level::Brightness, 20);
        t.set(Level::Brightness, 80);
        t.set(Level::Sdr, 35);
        assert_eq!(
            t.take(Duration::from_millis(50), interval),
            Some((Level::Sdr, 35))
        );
        assert_eq!(t.take(Duration::from_millis(99), interval), None);
        assert_eq!(
            t.take(Duration::from_millis(100), interval),
            Some((Level::Brightness, 80))
        );
        assert_eq!(t.take(Duration::from_secs(1), interval), None);
    }
}
