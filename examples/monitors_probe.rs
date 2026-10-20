//! Manual hardware probe. Do not run as part of automated verification.
use candle_tray::monitors::{Event, MonitorService, SnapshotReason};
fn main() {
    let (sender, receiver) = std::sync::mpsc::channel();
    let _service = MonitorService::start(move |event| {
        let _ = sender.send(event);
    });
    while let Ok(event) = receiver.recv_timeout(std::time::Duration::from_secs(30)) {
        if let Event::Snapshot {
            monitors,
            reason: SnapshotReason::Startup,
        } = event
        {
            println!("{monitors:#?}");
            return;
        }
    }
    eprintln!("Timed out waiting for startup snapshot");
    std::process::exit(1);
}
