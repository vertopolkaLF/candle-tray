//! Slider (normalized) ↔ hardware percent mapping and monotone calibration.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalibrationPoint {
    pub slider: u8,
    pub hardware: u8,
}

/// Endpoints are supplied by min/max; interior points must be strictly monotone.
pub fn valid_curve(points: &[CalibrationPoint], min: u8, max: u8) -> bool {
    let mut previous = CalibrationPoint {
        slider: 0,
        hardware: min,
    };
    for point in points {
        if point.slider <= previous.slider
            || point.slider >= 100
            || point.hardware <= previous.hardware
            || point.hardware >= max
        {
            return false;
        }
        previous = *point;
    }
    min < max && max <= 100
}

fn map_curve(value: u8, points: &[CalibrationPoint], min: u8, max: u8, inverse: bool) -> u8 {
    if !valid_curve(points, min, max) {
        return if inverse {
            from_hardware(value, min, max)
        } else {
            to_hardware(value, min, max)
        };
    }
    let coordinate = |p: CalibrationPoint| {
        if inverse {
            (p.hardware, p.slider)
        } else {
            (p.slider, p.hardware)
        }
    };
    let mut left = coordinate(CalibrationPoint {
        slider: 0,
        hardware: min,
    });
    let value = if inverse {
        value.clamp(min, max)
    } else {
        value.min(100)
    };
    for point in points.iter().copied().chain([CalibrationPoint {
        slider: 100,
        hardware: max,
    }]) {
        let right = coordinate(point);
        if value <= right.0 {
            let offset = u32::from(value.saturating_sub(left.0));
            let width = u32::from(right.0 - left.0);
            return left.1 + ((offset * u32::from(right.1 - left.1) + width / 2) / width) as u8;
        }
        left = right;
    }
    left.1
}

pub fn calibrated_to_hardware(value: u8, min: u8, max: u8, points: &[CalibrationPoint]) -> u8 {
    map_curve(value, points, min, max, false)
}

pub fn calibrated_from_hardware(value: u8, min: u8, max: u8, points: &[CalibrationPoint]) -> u8 {
    map_curve(value, points, min, max, true)
}

/// Map a 0..=100 slider value into the monitor's `min..=max` hardware range.
pub fn to_hardware(slider: u8, min: u8, max: u8) -> u8 {
    let slider = slider.min(100) as u32;
    let (min, max) = (min.min(100) as u32, max.min(100) as u32);
    if max <= min {
        return min as u8;
    }
    (min + (slider * (max - min) + 50) / 100) as u8
}

/// Inverse of [`to_hardware`], clamped to 0..=100.
pub fn from_hardware(hardware: u8, min: u8, max: u8) -> u8 {
    let (min, max) = (min.min(100) as u32, max.min(100) as u32);
    if max <= min {
        return 0;
    }
    let hardware = (hardware as u32).clamp(min, max);
    (((hardware - min) * 100 + (max - min) / 2) / (max - min)) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calibrated_mapping_is_monotone_and_hits_every_control_point() {
        let points = [
            CalibrationPoint {
                slider: 25,
                hardware: 15,
            },
            CalibrationPoint {
                slider: 75,
                hardware: 70,
            },
        ];
        assert!(valid_curve(&points, 10, 90));
        assert_eq!(calibrated_to_hardware(0, 10, 90, &points), 10);
        assert_eq!(calibrated_to_hardware(100, 10, 90, &points), 90);
        for point in points {
            assert_eq!(
                calibrated_to_hardware(point.slider, 10, 90, &points),
                point.hardware
            );
            assert_eq!(
                calibrated_from_hardware(point.hardware, 10, 90, &points),
                point.slider
            );
        }
        for value in 0..100 {
            assert!(
                calibrated_to_hardware(value, 10, 90, &points)
                    <= calibrated_to_hardware(value + 1, 10, 90, &points)
            );
            assert!(
                calibrated_from_hardware(value, 10, 90, &points)
                    <= calibrated_from_hardware(value + 1, 10, 90, &points)
            );
        }
    }

    #[test]
    fn corrupt_or_obsolete_calibration_falls_back_to_linear_mapping() {
        for points in [
            vec![CalibrationPoint {
                slider: 0,
                hardware: 20,
            }],
            vec![CalibrationPoint {
                slider: 50,
                hardware: 90,
            }],
            vec![
                CalibrationPoint {
                    slider: 75,
                    hardware: 60,
                },
                CalibrationPoint {
                    slider: 25,
                    hardware: 30,
                },
            ],
        ] {
            assert!(!valid_curve(&points, 20, 80));
            for value in 0..=100 {
                assert_eq!(
                    calibrated_to_hardware(value, 20, 80, &points),
                    to_hardware(value, 20, 80)
                );
                assert_eq!(
                    calibrated_from_hardware(value, 20, 80, &points),
                    from_hardware(value, 20, 80)
                );
            }
        }
    }

    #[test]
    fn identity_range_round_trips() {
        for value in 0..=100 {
            assert_eq!(to_hardware(value, 0, 100), value);
            assert_eq!(from_hardware(value, 0, 100), value);
        }
    }

    #[test]
    fn narrowed_range_maps_endpoints() {
        assert_eq!(to_hardware(0, 20, 80), 20);
        assert_eq!(to_hardware(100, 20, 80), 80);
        assert_eq!(from_hardware(10, 20, 80), 0);
        assert_eq!(from_hardware(90, 20, 80), 100);
    }
}
