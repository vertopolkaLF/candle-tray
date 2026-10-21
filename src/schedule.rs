//! When schedule entries fire: clock times and sunrise/sunset.
//!
//! Pure time math, generic over the time zone so tests do not depend on the
//! machine's zone. The app passes `chrono::Local`.

use chrono::{DateTime, Days, NaiveDate, TimeZone, Utc};

use crate::settings::{ScheduleEntry, ScheduleTime};

/// Sunrise and sunset of one day, in UTC.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SunDay {
    Normal {
        sunrise: DateTime<Utc>,
        sunset: DateTime<Utc>,
    },
    /// The sun never sets or never rises (polar day or night).
    NoEvents,
}

const J2000: f64 = 2_451_545.0;

/// The sunrise equation (NOAA's simplified form), accurate to a minute or
/// two, which is plenty for dimming displays. Longitude is east-positive.
pub fn sun_day(date: NaiveDate, latitude: f64, longitude: f64) -> SunDay {
    let epoch = NaiveDate::from_ymd_opt(2000, 1, 1).expect("valid date");
    let days = (date - epoch).num_days() as f64;
    let mean_solar_noon = days - longitude / 360.0;
    let anomaly = (357.5291 + 0.985_600_28 * mean_solar_noon).rem_euclid(360.0);
    let m = anomaly.to_radians();
    let center = 1.9148 * m.sin() + 0.02 * (2.0 * m).sin() + 0.0003 * (3.0 * m).sin();
    let ecliptic = (anomaly + center + 180.0 + 102.9372).rem_euclid(360.0);
    let l = ecliptic.to_radians();
    let transit = J2000 + mean_solar_noon + 0.0053 * m.sin() - 0.0069 * (2.0 * l).sin();
    let declination = (l.sin() * 23.4397_f64.to_radians().sin()).asin();
    let phi = latitude.to_radians();
    let cos_hour = ((-0.833_f64).to_radians().sin() - phi.sin() * declination.sin())
        / (phi.cos() * declination.cos());
    if !(-1.0..=1.0).contains(&cos_hour) {
        return SunDay::NoEvents;
    }
    let hour = cos_hour.acos().to_degrees() / 360.0;
    let utc = |julian: f64| {
        let seconds = ((julian - 2_440_587.5) * 86_400.0).round() as i64;
        DateTime::from_timestamp(seconds, 0).unwrap_or_default()
    };
    SunDay::Normal {
        sunrise: utc(transit - hour),
        sunset: utc(transit + hour),
    }
}

/// When `entry` fires on the local `date`, if it does.
pub fn occurrence<Tz: TimeZone>(
    entry: &ScheduleEntry,
    date: NaiveDate,
    zone: &Tz,
    location: Option<(f64, f64)>,
) -> Option<DateTime<Tz>> {
    let sun = |offset: i16, pick: fn(DateTime<Utc>, DateTime<Utc>) -> DateTime<Utc>| {
        let (latitude, longitude) = location?;
        match sun_day(date, latitude, longitude) {
            SunDay::Normal { sunrise, sunset } => Some(
                (pick(sunrise, sunset) + chrono::Duration::minutes(i64::from(offset)))
                    .with_timezone(zone),
            ),
            SunDay::NoEvents => None,
        }
    };
    match entry.time {
        ScheduleTime::Clock { minutes } => {
            let minutes = u32::from(minutes.min(24 * 60 - 1));
            let naive = date.and_hms_opt(minutes / 60, minutes % 60, 0)?;
            // A time skipped by a DST jump fires at the first instant after it.
            zone.from_local_datetime(&naive).earliest().or_else(|| {
                zone.from_local_datetime(&(naive + chrono::Duration::hours(1)))
                    .earliest()
            })
        }
        ScheduleTime::Sunrise { offset } => sun(offset, |rise, _| rise),
        ScheduleTime::Sunset { offset } => sun(offset, |_, set| set),
    }
}

/// The entry in effect at `now`: the one that fired last, looking back over
/// the previous two days. Tomorrow's sun times count too, since a large
/// negative offset can move them before midnight. Returns its index and when
/// it fired.
pub fn active<Tz: TimeZone>(
    entries: &[ScheduleEntry],
    now: &DateTime<Tz>,
    location: Option<(f64, f64)>,
) -> Option<(usize, DateTime<Tz>)> {
    let zone = now.timezone();
    let today = now.date_naive();
    let mut best: Option<(usize, DateTime<Tz>)> = None;
    for shift in -1i64..=2 {
        let date = if shift < 0 {
            today.checked_add_days(Days::new(1))
        } else {
            today.checked_sub_days(Days::new(shift as u64))
        };
        let Some(date) = date else {
            continue;
        };
        for (index, entry) in entries.iter().enumerate() {
            let Some(at) = occurrence(entry, date, &zone, location) else {
                continue;
            };
            if at > *now {
                continue;
            }
            if best.as_ref().is_none_or(|(_, best_at)| at >= *best_at) {
                best = Some((index, at));
            }
        }
    }
    best
}

/// Level `progress` (0..=1) of the way from `from` to `to`.
pub fn blend(from: u8, to: u8, progress: f32) -> u8 {
    let progress = progress.clamp(0.0, 1.0);
    (f32::from(from) + (f32::from(to) - f32::from(from)) * progress).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::ScheduleTarget;
    use chrono::{FixedOffset, Timelike};

    fn entry(time: ScheduleTime) -> ScheduleEntry {
        ScheduleEntry {
            time,
            target: ScheduleTarget::Level { level: 50 },
        }
    }

    #[test]
    fn moscow_equinox_sun_times() {
        // 2024-03-20 in Moscow (UTC+3): sunrise about 06:32, sunset about 18:43.
        let zone = FixedOffset::east_opt(3 * 3600).unwrap();
        let date = NaiveDate::from_ymd_opt(2024, 3, 20).unwrap();
        let SunDay::Normal { sunrise, sunset } = sun_day(date, 55.75, 37.62) else {
            panic!("Moscow has a sunrise in March");
        };
        let rise = sunrise.with_timezone(&zone);
        let set = sunset.with_timezone(&zone);
        let minutes = |t: DateTime<FixedOffset>| t.hour() * 60 + t.minute();
        assert!((minutes(rise) as i32 - (6 * 60 + 32)).abs() <= 5, "{rise}");
        assert!((minutes(set) as i32 - (18 * 60 + 43)).abs() <= 5, "{set}");
    }

    #[test]
    fn polar_night_has_no_events() {
        let date = NaiveDate::from_ymd_opt(2024, 12, 21).unwrap();
        assert_eq!(sun_day(date, 78.2, 15.6), SunDay::NoEvents);
    }

    #[test]
    fn latest_past_entry_wins_and_wraps_to_yesterday() {
        let zone = FixedOffset::east_opt(0).unwrap();
        let entries = [
            entry(ScheduleTime::Clock { minutes: 7 * 60 }),
            entry(ScheduleTime::Clock { minutes: 21 * 60 }),
        ];
        let at = |h, m| zone.with_ymd_and_hms(2024, 5, 10, h, m, 0).unwrap();
        assert_eq!(active(&entries, &at(12, 0), None).unwrap().0, 0);
        assert_eq!(active(&entries, &at(22, 0), None).unwrap().0, 1);
        // Before 07:00 yesterday's 21:00 entry is still in effect.
        let (index, fired) = active(&entries, &at(6, 0), None).unwrap();
        assert_eq!(index, 1);
        assert_eq!(
            fired.date_naive(),
            NaiveDate::from_ymd_opt(2024, 5, 9).unwrap()
        );
        assert!(active(&[], &at(6, 0), None).is_none());
    }

    #[test]
    fn sun_entries_need_a_location() {
        let zone = FixedOffset::east_opt(3 * 3600).unwrap();
        let entries = [entry(ScheduleTime::Sunset { offset: 0 })];
        let now = zone.with_ymd_and_hms(2024, 3, 20, 20, 0, 0).unwrap();
        assert!(active(&entries, &now, None).is_none());
        let (_, fired) = active(&entries, &now, Some((55.75, 37.62))).unwrap();
        assert_eq!(fired.date_naive(), now.date_naive());
        assert_eq!(fired.hour(), 18);
    }

    #[test]
    fn early_sun_offsets_fire_the_evening_before() {
        // Sunrise on 2024-03-21 in Moscow is about 06:30; 12 hours earlier
        // is the evening of the 20th.
        let zone = FixedOffset::east_opt(3 * 3600).unwrap();
        let entries = [entry(ScheduleTime::Sunrise { offset: -720 })];
        let now = zone.with_ymd_and_hms(2024, 3, 20, 20, 0, 0).unwrap();
        let (_, fired) = active(&entries, &now, Some((55.75, 37.62))).unwrap();
        assert_eq!(fired.date_naive(), now.date_naive());
        assert_eq!(fired.hour(), 18);
    }

    #[test]
    fn blend_moves_linearly() {
        assert_eq!(blend(80, 20, 0.0), 80);
        assert_eq!(blend(80, 20, 0.5), 50);
        assert_eq!(blend(80, 20, 2.0), 20);
    }
}
