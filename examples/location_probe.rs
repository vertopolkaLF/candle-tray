//! Manual, opt-in probe: may display a Windows location permission prompt.
fn main() -> anyhow::Result<()> {
    let (latitude, longitude) = std::thread::spawn(candle_tray::location::detect)
        .join()
        .map_err(|_| anyhow::anyhow!("location worker panicked"))??;
    println!("Latitude: {latitude:.2}, longitude: {longitude:.2}");
    Ok(())
}
