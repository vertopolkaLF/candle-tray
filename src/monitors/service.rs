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
        let target = [Level::Brightness, Level::Sdr]
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
    Probe(Target, Arc<Vec<Target>>),
    Capabilities,
    Write(Level, u8),
    Power,
}
struct Reply {
    id: String,
    operation: u64,
    monitor: Monitor,
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
    refresh: Option<(Target, Arc<Vec<Target>>)>,
    capabilities: bool,
    power: bool,
    awaiting_probe: bool,
    active: bool,
}
impl Slot {
    /// Nothing in flight and nothing waiting to be dispatched.
    fn idle(&self) -> bool {
        self.busy.is_none()
            && self.throttle.pending.is_empty()
            && self.refresh.is_none()
            && !self.capabilities
            && !(self.power && self.monitor.supports_ddc_power)
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
fn worker(receiver: Receiver<Request>, sender: Sender<Reply>) {
    let mut state: Option<(Target, Option<ddc::Ddc>, Option<wmi::Wmi>, u32)> = None;
    while let Ok(request) = receiver.recv() {
        match request.work {
            Work::Probe(mut target, targets) => {
                // Releasing/reacquiring handles stays on this worker, behind any
                // previous call. Never close a handle still executing in a driver.
                drop(state.take());
                let wmi = wmi::Wmi::connect()
                    .map_err(|e| log(format!("WMI: {e}")))
                    .ok();
                if let Some(name) = wmi.as_ref().and_then(|w| w.name(&target.monitor.id)) {
                    target.monitor.name = name;
                }
                let brightness = wmi.as_ref().and_then(|w| w.brightness(&target.monitor.id));
                let mut max = 100;
                let ddc = if let Some(brightness) = brightness {
                    target.monitor.kind = MonitorKind::Internal;
                    target.monitor.backend = BrightnessBackend::Wmi;
                    target.monitor.brightness = Some(brightness);
                    None
                } else {
                    let ddc = ddc::Ddc::acquire(&target, &targets);
                    if let Some(d) = &ddc {
                        for code in rules::brightness_codes(&target.monitor.model) {
                            if let Some((current, maximum)) = d.read(code) {
                                max = maximum;
                                target.monitor.backend = BrightnessBackend::Ddc { vcp: code };
                                target.monitor.brightness = Some(
                                    ((u64::from(current) * 100 / u64::from(max)).min(100)) as u8,
                                );
                                break;
                            }
                        }
                    }
                    ddc
                };
                target.monitor.sdr = hdr::read(&target);
                state = Some((target, ddc, wmi, max));
            }
            Work::Capabilities => {
                if let Some((target, ddc, _, _)) = &mut state {
                    target.monitor.supports_ddc_power =
                        ddc.as_ref().is_some_and(|d| d.power_supported());
                }
            }
            Work::Power => {
                if let Some((target, Some(ddc), _, _)) = &state
                    && target.monitor.supports_ddc_power
                    && !ddc.set(0xd6, 5)
                {
                    log(format!("DDC power failed: {}", target.monitor.id));
                }
            }
            Work::Write(level, percent) => {
                if let Some((target, ddc, wmi, max)) = &mut state {
                    let ok = match level {
                        Level::Sdr => target.monitor.sdr.is_some() && hdr::set(target, percent),
                        Level::Brightness => match target.monitor.backend {
                            BrightnessBackend::Ddc { vcp } => ddc.as_ref().is_some_and(|d| {
                                d.set(vcp, (u64::from(percent) * u64::from(*max) / 100) as u32)
                            }),
                            BrightnessBackend::Wmi => wmi
                                .as_ref()
                                .is_some_and(|w| w.set(&target.monitor.id, percent).is_ok()),
                            BrightnessBackend::Unsupported => false,
                        },
                    };
                    if ok {
                        match level {
                            Level::Brightness => target.monitor.brightness = Some(percent),
                            Level::Sdr => target.monitor.sdr = Some(SdrLevel { percent }),
                        }
                    } else {
                        if level == Level::Brightness {
                            target.monitor.brightness = None;
                        }
                        log(format!(
                            "Hardware write failed: {} {level:?}",
                            target.monitor.id
                        ));
                    }
                }
            }
        }
        if let Some((target, _, _, _)) = &state
            && sender
                .send(Reply {
                    id: target.monitor.id.clone(),
                    operation: request.operation,
                    monitor: target.monitor.clone(),
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
    let mut sequence = 0u64;
    let mut throttle = Duration::from_millis(100);
    let epoch = Instant::now();
    let mut refresh = Some(SnapshotReason::Startup);
    let mut snapshot = None;
    let mut reasons = vec![SnapshotReason::Startup];
    let mut debounce: Option<(Instant, SnapshotReason)> = None;
    let mut windows_off_at: Option<Instant> = None;
    let mut queued = std::collections::VecDeque::new();
    loop {
        if windows_off_at.is_some_and(|at| Instant::now() >= at) {
            windows_off_at = None;
            events::turn_off();
        }
        for reason in events::Events::pump() {
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
                Ok(Command::SetThrottle(value)) => throttle = value,
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
                Ok(Command::Apply(targets)) => {
                    for t in targets {
                        if let Some(s) = slots.get_mut(&t.id).filter(|s| s.active) {
                            if let Some(p) = t.brightness {
                                s.throttle.set(Level::Brightness, p);
                            }
                            if let Some(p) = t.sdr {
                                s.throttle.set(Level::Sdr, p);
                            }
                        }
                    }
                }
                Ok(Command::TurnOff(mode)) => {
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
                            s.power = true;
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
                callback(Event::Updated(slot.monitor.clone()));
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
                            slot.power = false;
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
                                refresh: None,
                                capabilities: false,
                                power: false,
                                awaiting_probe: false,
                                active: true,
                            }
                        });
                        slot.refresh = Some((target.clone(), targets.clone()));
                        slot.awaiting_probe = true;
                        slot.capabilities = true;
                        if slot.monitor.unresponsive {
                            slot.awaiting_probe = false;
                        }
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
                (Work::Probe(target, targets), Duration::from_secs(15))
            } else if let Some((level, p)) = slot.throttle.take(epoch.elapsed(), throttle) {
                (Work::Write(level, p), Duration::from_secs(3))
            } else if slot.power && slot.monitor.supports_ddc_power {
                slot.power = false;
                (Work::Power, Duration::from_secs(3))
            } else if slot.capabilities {
                slot.capabilities = false;
                (Work::Capabilities, Duration::from_secs(15))
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
    fn slot() -> Slot {
        let (sender, _) = mpsc::channel();
        Slot {
            monitor: Monitor {
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
                supports_ddc_power: false,
                unresponsive: false,
            },
            sender,
            busy: None,
            throttle: Throttle::default(),
            refresh: None,
            capabilities: false,
            power: false,
            awaiting_probe: true,
            active: true,
        }
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
