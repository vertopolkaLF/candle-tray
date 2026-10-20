//! Slider (normalized) ↔ hardware percent mapping using per-monitor min/max.

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
