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

/// Detects `(latitude, longitude)`, rounded to two decimals for privacy.
///
/// Blocking: call on a background MTA thread, never the UI/tray thread. Requests
/// Windows permission first (which may prompt), then requests coarse accuracy
/// with a ten-second sensor timeout and permits cached fixes up to five minutes
/// old. Time spent waiting for user consent is not part of the sensor timeout.
/// Permission prompting is OS-dependent; Windows may require the app to be in
/// the foreground. Failures remain typed as [`LocationError`] inside anyhow.
#[cfg(windows)]
pub fn detect() -> anyhow::Result<(f64, f64)> {
    use windows::{
        Devices::Geolocation::{GeolocationAccessStatus, Geolocator, PositionAccuracy},
        Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize},
    };

    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            // Balanced only after a successful RoInitialize (including S_FALSE).
            unsafe { RoUninitialize() };
        }
    }

    fn lookup() -> Result<(f64, f64), LocationError> {
        let unavailable = |error: windows::core::Error| {
            if error.code() == windows::Win32::Foundation::E_ACCESSDENIED {
                LocationError::Denied
            } else {
                LocationError::Unavailable(error.to_string())
            }
        };
        unsafe { RoInitialize(RO_INIT_MULTITHREADED) }
            .ok()
            .map_err(unavailable)?;
        let _apartment = Apartment;
        let access = Geolocator::RequestAccessAsync()
            .and_then(|request| request.join())
            .map_err(unavailable)?;
        if access == GeolocationAccessStatus::Denied {
            return Err(LocationError::Denied);
        }
        if access != GeolocationAccessStatus::Allowed {
            return Err(LocationError::Unavailable(
                "Windows did not grant a location access status".into(),
            ));
        }
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

    lookup().map_err(Into::into)
}

/// Location detection is only supported on Windows.
#[cfg(not(windows))]
pub fn detect() -> anyhow::Result<(f64, f64)> {
    Err(LocationError::Unavailable("requires Windows".into()).into())
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
