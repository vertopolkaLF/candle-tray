//! Manual hardware diagnostic; run explicitly on a machine with a light sensor.

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use std::{sync::mpsc, time::Duration};
    use windows::{
        Devices::Sensors::LightSensor,
        Win32::{
            Foundation::S_OK,
            System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize},
        },
    };

    // The diagnostic owns this thread's apartment for the entire subscription.
    unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.ok()?;
    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe { RoUninitialize() };
        }
    }
    let _apartment = Apartment;
    let sensor = match LightSensor::GetDefault() {
        Ok(sensor) => sensor,
        Err(error) if error.code() == S_OK => {
            println!("No ambient light sensor found.");
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    let (sender, receiver) = mpsc::sync_channel(8);
    let _event = sensor.ReadingChanged(move |_, args| {
        if let Some(args) = args.as_ref()
            && let Ok(reading) = args.Reading()
            && let Ok(lux) = reading.IlluminanceInLux()
        {
            let _ = sender.try_send(lux);
        }
    })?;
    sensor.SetReportInterval(sensor.MinimumReportInterval()?.max(1000))?;
    let mut smoother = candle_tray::light_sensor::Smoother::new();
    if let Ok(reading) = sensor.GetCurrentReading()
        && let Ok(lux) = reading.IlluminanceInLux()
    {
        println!(
            "Current: {lux:.2} lux; smoothed: {:?}",
            smoother.update(lux)
        );
    }
    println!("Waiting for up to five readings (10 s timeout per reading)...");
    for _ in 0..5 {
        match receiver.recv_timeout(Duration::from_secs(10)) {
            Ok(lux) => println!("{lux:.2} lux; smoothed update: {:?}", smoother.update(lux)),
            Err(error) => {
                println!("No new reading: {error}");
                break;
            }
        }
    }
    sensor.SetReportInterval(0)?;
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("The light sensor probe requires Windows.");
}
