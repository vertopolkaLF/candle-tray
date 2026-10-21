//! One-shot approximate location lookup. No coordinates are persisted or logged here.

use std::fmt;

/// A permission refusal is distinguishable through `anyhow::Error::downcast_ref`.
#[derive(Debug, PartialEq, Eq)]
pub enum LocationError {
    /// Location access is disabled or refused by Windows privacy settings/policy.
    Denied,
    /// Initialization, sensor, timeout, or invalid-coordinate failure.
    Unavailable(String),
}

impl fmt::Display for LocationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Denied => f.write_str(
                "Location access is off in Windows Settings or denied by policy. Enable location services and location access for desktop apps.",
            ),
            Self::Unavailable(reason) => write!(f, "Location is unavailable: {reason}"),
        }
    }
}

impl std::error::Error for LocationError {}

/// An in-flight lookup. Poll from the UI thread without waiting for the worker.
pub struct Detection {
    receiver: std::sync::mpsc::Receiver<anyhow::Result<(f64, f64)>>,
    finished: bool,
    // Keep WinRT initialized until the UI has consumed or dropped the lookup.
    #[cfg(windows)]
    _apartment: Option<Apartment>,
}

impl Detection {
    /// Returns the result once; `None` means pending or already consumed.
    pub fn poll(&mut self) -> Option<anyhow::Result<(f64, f64)>> {
        if self.finished {
            return None;
        }
        let result = match self.receiver.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => return None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err(LocationError::Unavailable("location worker disconnected".into()).into())
            }
        };
        self.finished = true;
        Some(result)
    }
}

/// Starts Windows consent on the calling UI thread, without waiting for it.
///
/// The caller must be the app's UI thread and keep pumping messages. A worker waits for consent, then requests coarse coordinates with
/// a ten-second sensor timeout and a maximum cached age of five minutes.
/// Consent time is not part of that timeout. Coordinates are rounded to two
/// decimals, and failures remain typed as [`LocationError`] inside anyhow.
#[cfg(windows)]
pub fn start() -> Detection {
    use windows::{
        Devices::Geolocation::{GeolocationAccessStatus, Geolocator},
        Win32::System::WinRT::{RO_INIT_MULTITHREADED, RO_INIT_SINGLETHREADED, RoInitialize},
    };

    let (sender, receiver) = std::sync::mpsc::channel();
    let mut detection = Detection {
        receiver,
        finished: false,
        _apartment: None,
    };
    // GPUI initializes STA COM through OLE; also initialize WinRT explicitly.
    if let Err(error) = unsafe { RoInitialize(RO_INIT_SINGLETHREADED) }.ok() {
        let _ = sender.send(Err(unavailable(error).into()));
        return detection;
    }
    detection._apartment = Some(Apartment(std::marker::PhantomData));
    // This call must happen before moving any work off the UI thread.
    let access = match Geolocator::RequestAccessAsync() {
        Ok(access) => access,
        Err(error) => {
            let _ = sender.send(Err(unavailable(error).into()));
            return detection;
        }
    };
    let worker_sender = sender.clone();
    let worker = std::thread::Builder::new()
        .name("location".into())
        .spawn(move || {
            let lookup = || -> Result<(f64, f64), LocationError> {
                unsafe { RoInitialize(RO_INIT_MULTITHREADED) }
                    .ok()
                    .map_err(unavailable)?;
                let _apartment = Apartment(std::marker::PhantomData);
                let access = access.join().map_err(unavailable)?;
                if access == GeolocationAccessStatus::Denied {
                    return Err(LocationError::Denied);
                }
                if access != GeolocationAccessStatus::Allowed {
                    return Err(LocationError::Unavailable(
                        "Windows did not grant a location access status".into(),
                    ));
                }
                lookup_coordinates()
            };
            let _ = worker_sender.send(lookup().map_err(Into::into));
        });
    if let Err(error) = worker {
        let _ = sender.send(Err(LocationError::Unavailable(error.to_string()).into()));
    }
    detection
}

// Apartment teardown must stay on the thread that initialized it.
#[cfg(windows)]
struct Apartment(std::marker::PhantomData<std::rc::Rc<()>>);

#[cfg(windows)]
impl Drop for Apartment {
    fn drop(&mut self) {
        // Balanced only after successful initialization (including S_FALSE).
        unsafe { windows::Win32::System::WinRT::RoUninitialize() };
    }
}

#[cfg(windows)]
fn unavailable(error: windows::core::Error) -> LocationError {
    if error.code() == windows::Win32::Foundation::E_ACCESSDENIED {
        LocationError::Denied
    } else {
        LocationError::Unavailable(error.to_string())
    }
}

#[cfg(windows)]
fn lookup_coordinates() -> Result<(f64, f64), LocationError> {
    use windows::Devices::Geolocation::{Geolocator, PositionAccuracy};
    let locator = Geolocator::new().map_err(unavailable)?;
    locator
        .SetDesiredAccuracy(PositionAccuracy::Default)
        .map_err(unavailable)?;
    let position = locator
        .GetGeopositionAsyncWithAgeAndTimeout(
            std::time::Duration::from_secs(300)
                .try_into()
                .expect("five minutes fits in a WinRT TimeSpan"),
            std::time::Duration::from_secs(10)
                .try_into()
                .expect("ten seconds fits in a WinRT TimeSpan"),
        )
        .and_then(|request| request.join())
        .map_err(unavailable)?;
    let coordinate = position.Coordinate().map_err(unavailable)?;
    // Latitude/Longitude avoid requiring the optional geopoint interface.
    rounded_coordinates(
        coordinate.Latitude().map_err(unavailable)?,
        coordinate.Longitude().map_err(unavailable)?,
    )
}

/// Location detection is only supported on Windows.
#[cfg(not(windows))]
pub fn start() -> Detection {
    let (sender, receiver) = std::sync::mpsc::channel();
    let _ = sender.send(Err(
        LocationError::Unavailable("requires Windows".into()).into()
    ));
    Detection {
        receiver,
        finished: false,
    }
}

#[cfg(any(windows, test))]
fn rounded_coordinates(latitude: f64, longitude: f64) -> Result<(f64, f64), LocationError> {
    if !latitude.is_finite()
        || !longitude.is_finite()
        || !(-90.0..=90.0).contains(&latitude)
        || !(-180.0..=180.0).contains(&longitude)
    {
        return Err(LocationError::Unavailable(
            "Windows returned invalid coordinates".into(),
        ));
    }
    Ok((
        (latitude * 100.0).round() / 100.0,
        (longitude * 100.0).round() / 100.0,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polling_is_nonblocking_and_delivers_typed_results_once() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut detection = Detection {
            receiver,
            finished: false,
            #[cfg(windows)]
            _apartment: None,
        };
        assert!(detection.poll().is_none());
        sender.send(Err(LocationError::Denied.into())).unwrap();
        let error = detection.poll().unwrap().unwrap_err();
        assert_eq!(
            error.downcast_ref::<LocationError>(),
            Some(&LocationError::Denied)
        );
        assert!(detection.poll().is_none());
    }

    #[test]
    fn a_disconnected_worker_is_a_typed_failure() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut detection = Detection {
            receiver,
            finished: false,
            #[cfg(windows)]
            _apartment: None,
        };
        drop(sender);
        let error = detection.poll().unwrap().unwrap_err();
        assert!(matches!(
            error.downcast_ref::<LocationError>(),
            Some(LocationError::Unavailable(_))
        ));
        assert!(detection.poll().is_none());
    }

    #[test]
    fn rounds_both_hemispheres_to_two_decimals() {
        assert_eq!(
            rounded_coordinates(55.7558, -37.6173).unwrap(),
            (55.76, -37.62)
        );
        assert_eq!(
            rounded_coordinates(-33.8688, 151.2093).unwrap(),
            (-33.87, 151.21)
        );
        assert_eq!(rounded_coordinates(90.0, -180.0).unwrap(), (90.0, -180.0));
    }

    #[test]
    fn validates_before_rounding() {
        for (lat, lon) in [
            (90.001, 0.0),
            (-90.001, 0.0),
            (0.0, 180.001),
            (0.0, -180.001),
            (f64::NAN, 0.0),
            (0.0, f64::INFINITY),
        ] {
            assert!(matches!(
                rounded_coordinates(lat, lon),
                Err(LocationError::Unavailable(_))
            ));
        }
    }
}
