//! Hands memory and CPU back to Windows while no window is on screen.
//!
//! A tray app is idle almost all the time, but GPUI, Direct3D and
//! DirectWrite keep everything they touched resident. A short while after
//! the popup hides and Settings is closed, the process compacts its heap,
//! empties its working set, lowers its memory priority and opts into
//! EcoQoS. Showing any surface restores normal priorities; the trimmed pages
//! fault back in from the standby list as they are touched.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

/// Grace period after the last surface hides, so a quick reopen stays warm.
const IDLE_DELAY: Duration = Duration::from_secs(10);

/// Bumped on every show and every scheduled idle entry; a pending entry only
/// runs if nothing happened since it was scheduled.
static GENERATION: AtomicU64 = AtomicU64::new(0);
static IDLE: AtomicBool = AtomicBool::new(false);

/// A surface is about to show: cancel a pending idle entry and restore
/// normal memory priority and scheduling. GPUI thread only.
pub fn leave() {
    GENERATION.fetch_add(1, Ordering::SeqCst);
    if IDLE.swap(false, Ordering::SeqCst) {
        set_idle_priorities(false);
    }
}

/// Enter the idle state after [`IDLE_DELAY`] if no surface shows meanwhile.
/// GPUI thread only.
pub fn enter_soon(cx: &mut gpui::App) {
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    cx.spawn(async move |cx| {
        cx.background_executor().timer(IDLE_DELAY).await;
        let current = GENERATION.load(Ordering::SeqCst) == generation;
        if !current || crate::popup::is_visible() || crate::settings_window::is_open() {
            return;
        }
        if !IDLE.swap(true, Ordering::SeqCst) {
            set_idle_priorities(true);
        }
        // Emptying the working set walks every resident page; keep that
        // off the UI thread.
        cx.background_executor().spawn(async { trim() }).detach();
    })
    .detach();
}

#[cfg(windows)]
fn trim() {
    use windows_sys::Win32::System::{
        Memory::{GetProcessHeap, HeapCompact},
        Threading::{GetCurrentProcess, SetProcessWorkingSetSize},
    };
    unsafe {
        // Decommit free heap blocks first so they do not fault back in.
        HeapCompact(GetProcessHeap(), 0);
        SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
    }
}

#[cfg(not(windows))]
fn trim() {}

/// Low memory priority makes our standby pages the first to be reused;
/// EcoQoS runs the occasional background wake-up on efficient cores.
#[cfg(windows)]
fn set_idle_priorities(idle: bool) {
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, MEMORY_PRIORITY_INFORMATION, MEMORY_PRIORITY_LOW,
        MEMORY_PRIORITY_NORMAL, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_STATE,
        ProcessMemoryPriority, ProcessPowerThrottling, SetProcessInformation,
    };
    let memory = MEMORY_PRIORITY_INFORMATION {
        MemoryPriority: if idle {
            MEMORY_PRIORITY_LOW
        } else {
            MEMORY_PRIORITY_NORMAL
        },
    };
    // A zero control mask hands execution speed back to the system's own
    // heuristics instead of pinning it on.
    let throttling = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: if idle {
            PROCESS_POWER_THROTTLING_EXECUTION_SPEED
        } else {
            0
        },
        StateMask: if idle {
            PROCESS_POWER_THROTTLING_EXECUTION_SPEED
        } else {
            0
        },
    };
    unsafe {
        let process = GetCurrentProcess();
        SetProcessInformation(
            process,
            ProcessMemoryPriority,
            &memory as *const _ as *const _,
            size_of::<MEMORY_PRIORITY_INFORMATION>() as u32,
        );
        SetProcessInformation(
            process,
            ProcessPowerThrottling,
            &throttling as *const _ as *const _,
            size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        );
    }
}

#[cfg(not(windows))]
fn set_idle_priorities(_idle: bool) {}
