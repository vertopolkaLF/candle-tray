//! Manual, opt-in probe: may display a Windows location permission prompt.
fn main() -> anyhow::Result<()> {
    // The main thread acts as the UI thread and pumps consent messages.
    let mut detection = candle_tray::location::start();
    let (latitude, longitude) = loop {
        if let Some(result) = detection.poll() {
            break result?;
        }
        #[cfg(windows)]
        unsafe {
            use windows_sys::Win32::UI::WindowsAndMessaging::{
                DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
            };
            let mut message: MSG = std::mem::zeroed();
            while PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    };
    println!("Latitude: {latitude:.2}, longitude: {longitude:.2}");
    Ok(())
}
