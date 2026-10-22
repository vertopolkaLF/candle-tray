//! Clock and SunCalc-compatible solar schedule occurrences.
//!
//! Pure time math, generic over the time zone so tests do not depend on the
//! machine's zone. The app passes `chrono::Local`.

use chrono::{DateTime, Days, NaiveDate, TimeZone, Utc};

use crate::settings::{ScheduleEntry, ScheduleTime, SolarEvent};

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
    match (
        solar_utc(date, latitude, longitude, -0.833, false),
        solar_utc(date, latitude, longitude, -0.833, true),
    ) {
        (Some(sunrise), Some(sunset)) => SunDay::Normal { sunrise, sunset },
        _ => SunDay::NoEvents,
    }
}

/// Solar transit and elevation crossings for the solar day at this longitude.
/// A missing elevation crossing does not suppress other events (e.g. polar noon).
fn solar_utc(
    date: NaiveDate,
    latitude: f64,
    longitude: f64,
    elevation: f64,
    evening: bool,
) -> Option<DateTime<Utc>> {
    if !latitude.is_finite()
        || !longitude.is_finite()
        || latitude.abs() > 90.0
        || longitude.abs() > 180.0
    {
        return None;
    }
    let epoch = NaiveDate::from_ymd_opt(2000, 1, 1)?;
    let days = (date - epoch).num_days() as f64;
    let noon = days - longitude / 360.0 + 0.0009;
    let anomaly = (357.5291 + 0.985_600_28 * noon).to_radians();
    let center =
        (1.9148 * anomaly.sin() + 0.02 * (2.0 * anomaly).sin() + 0.0003 * (3.0 * anomaly).sin())
            .to_radians();
    let ecliptic = anomaly + center + (180.0 + 102.9372_f64).to_radians();
    let transit = J2000 + noon + 0.0053 * anomaly.sin() - 0.0069 * (2.0 * ecliptic).sin();
    let crossing = if elevation.is_nan() {
        0.0
    } else {
        let declination = (ecliptic.sin() * 23.4397_f64.to_radians().sin()).asin();
        let phi = latitude.to_radians();
        let cos_hour = (elevation.to_radians().sin() - phi.sin() * declination.sin())
            / (phi.cos() * declination.cos());
        if !(-1.0..=1.0).contains(&cos_hour) {
            return None;
        }
        cos_hour.acos().to_degrees() / 360.0 * if evening { 1.0 } else { -1.0 }
    };
    DateTime::from_timestamp(
        ((transit + crossing - 2_440_587.5) * 86_400.0).round() as i64,
        0,
    )
}

/// When `entry` fires on the local `date`, if it does.
pub fn occurrence<Tz: TimeZone>(
    entry: &ScheduleEntry,
    date: NaiveDate,
    zone: &Tz,
    location: Option<(f64, f64)>,
) -> Option<DateTime<Tz>> {
    let sun = |offset: i16, elevation: f64, evening: bool| {
        let (latitude, longitude) = location?;
        // A solar day is indexed at the longitude, not in the user's timezone.
        // Find the unshifted event on the requested local date first; only then
        // add the elapsed minute offset, which may cross DST or midnight.
        (-2..=2).find_map(|shift| {
            let candidate = date.checked_add_signed(chrono::Duration::days(shift))?;
            let at =
                solar_utc(candidate, latitude, longitude, elevation, evening)?.with_timezone(zone);
            (at.date_naive() == date).then(|| {
                (at.with_timezone(&Utc) + chrono::Duration::minutes(i64::from(offset)))
                    .with_timezone(zone)
            })
        })
    };
    match entry.time {
        ScheduleTime::Clock { minutes } => {
            let minutes = u32::from(minutes.min(24 * 60 - 1));
            let naive = date.and_hms_opt(minutes / 60, minutes % 60, 0)?;
            // Ambiguous clock times fire once, at the earlier instant. For
            // gaps (including non-hour DST jumps), choose the first valid minute.
            (0..=1440).find_map(|minute| {
                zone.from_local_datetime(&(naive + chrono::Duration::minutes(minute)))
                    .earliest()
            })
        }
        ScheduleTime::Sunrise { offset } => sun(offset, -0.833, false),
        ScheduleTime::Sunset { offset } => sun(offset, -0.833, true),
        ScheduleTime::Solar { event, offset } => match event {
            SolarEvent::Dawn => sun(offset, -6.0, false),
            SolarEvent::SolarNoon => sun(offset, f64::NAN, true),
            SolarEvent::GoldenHour => sun(offset, 6.0, true),
            SolarEvent::SunsetStart => sun(offset, -0.3, true),
            SolarEvent::Dusk => sun(offset, -6.0, true),
            SolarEvent::Night => sun(offset, -18.0, true),
        },
    }
}

/// The entry in effect at `now`: the one that fired last, looking back over
/// recent local dates, extending the window for the largest offset. Future
/// solar dates count too, since a large
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
    let offset_days = entries
        .iter()
        .map(|entry| match entry.time {
            ScheduleTime::Clock { .. } => 0,
            ScheduleTime::Sunrise { offset }
            | ScheduleTime::Sunset { offset }
            | ScheduleTime::Solar { offset, .. } => i64::from(offset).abs() / 1440,
        })
        .max()
        .unwrap_or(0);
    for shift in -(offset_days + 2)..=(offset_days + 3) {
        let date = if shift < 0 {
            today.checked_add_days(Days::new(shift.unsigned_abs()))
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

    // A deterministic European 2024 DST zone, without host timezone dependence.
    #[derive(Clone, Copy, Debug)]
    struct Dst;
    impl TimeZone for Dst {
        type Offset = FixedOffset;
        fn from_offset(_: &Self::Offset) -> Self {
            Self
        }
        fn offset_from_local_date(&self, date: &NaiveDate) -> chrono::LocalResult<Self::Offset> {
            self.offset_from_local_datetime(&date.and_hms_opt(12, 0, 0).unwrap())
        }
        fn offset_from_local_datetime(
            &self,
            local: &chrono::NaiveDateTime,
        ) -> chrono::LocalResult<Self::Offset> {
            let one = FixedOffset::east_opt(3600).unwrap();
            let two = FixedOffset::east_opt(7200).unwrap();
            let spring = NaiveDate::from_ymd_opt(2024, 3, 31)
                .unwrap()
                .and_hms_opt(2, 0, 0)
                .unwrap();
            let autumn = NaiveDate::from_ymd_opt(2024, 10, 27)
                .unwrap()
                .and_hms_opt(2, 0, 0)
                .unwrap();
            if *local >= spring && *local < spring + chrono::Duration::hours(1) {
                chrono::LocalResult::None
            } else if *local >= autumn && *local < autumn + chrono::Duration::hours(1) {
                chrono::LocalResult::Ambiguous(two, one)
            } else {
                chrono::LocalResult::Single(if *local >= spring && *local < autumn {
                    two
                } else {
                    one
                })
            }
        }
        fn offset_from_utc_date(&self, date: &NaiveDate) -> Self::Offset {
            self.offset_from_utc_datetime(&date.and_hms_opt(12, 0, 0).unwrap())
        }
        fn offset_from_utc_datetime(&self, utc: &chrono::NaiveDateTime) -> Self::Offset {
            let spring = NaiveDate::from_ymd_opt(2024, 3, 31)
                .unwrap()
                .and_hms_opt(1, 0, 0)
                .unwrap();
            let autumn = NaiveDate::from_ymd_opt(2024, 10, 27)
                .unwrap()
                .and_hms_opt(1, 0, 0)
                .unwrap();
            FixedOffset::east_opt(if *utc >= spring && *utc < autumn {
                7200
            } else {
                3600
            })
            .unwrap()
        }
    }

    #[test]
    fn dst_gaps_use_first_valid_minute_and_folds_fire_once() {
        let clock = entry(ScheduleTime::Clock { minutes: 150 });
        let spring = NaiveDate::from_ymd_opt(2024, 3, 31).unwrap();
        let at = occurrence(&clock, spring, &Dst, None).unwrap();
        assert_eq!((at.hour(), at.minute()), (3, 0));
        let autumn = NaiveDate::from_ymd_opt(2024, 10, 27).unwrap();
        let at = occurrence(&clock, autumn, &Dst, None).unwrap();
        assert_eq!(at.offset().local_minus_utc(), 7200);
        let now = Dst.with_ymd_and_hms(2024, 10, 27, 3, 0, 0).unwrap();
        assert_eq!(active(&[clock], &now, None).unwrap().1, at);
    }

    #[test]
    fn solar_offsets_recalculate_dst_offset() {
        let date = NaiveDate::from_ymd_opt(2024, 3, 31).unwrap();
        let base = entry(ScheduleTime::Sunrise { offset: 0 });
        let shifted = entry(ScheduleTime::Sunrise { offset: -300 });
        let rise = occurrence(&base, date, &Dst, Some((52.5, 13.4))).unwrap();
        let before = occurrence(&shifted, date, &Dst, Some((52.5, 13.4))).unwrap();
        assert_eq!(rise.offset().local_minus_utc(), 7200);
        assert_eq!(before.offset().local_minus_utc(), 3600);
        assert_eq!(
            (rise.with_timezone(&Utc) - before.with_timezone(&Utc)).num_minutes(),
            300
        );
    }

    #[test]
    fn all_solar_events_keep_order_and_requested_local_date() {
        let date = NaiveDate::from_ymd_opt(2024, 3, 20).unwrap();
        for (offset, lat, lon) in [
            (14 * 3600, 1.8, -157.4),
            (-10 * 3600, -17.5, -149.6),
            (-8 * 3600, 37.8, -122.4),
            (12 * 3600, -36.8, 174.8),
        ] {
            let zone = FixedOffset::east_opt(offset).unwrap();
            let times = [
                ScheduleTime::Solar {
                    event: SolarEvent::Dawn,
                    offset: 0,
                },
                ScheduleTime::Sunrise { offset: 0 },
                ScheduleTime::Solar {
                    event: SolarEvent::SolarNoon,
                    offset: 0,
                },
                ScheduleTime::Solar {
                    event: SolarEvent::GoldenHour,
                    offset: 0,
                },
                ScheduleTime::Solar {
                    event: SolarEvent::SunsetStart,
                    offset: 0,
                },
                ScheduleTime::Sunset { offset: 0 },
                ScheduleTime::Solar {
                    event: SolarEvent::Dusk,
                    offset: 0,
                },
                ScheduleTime::Solar {
                    event: SolarEvent::Night,
                    offset: 0,
                },
            ]
            .map(|time| occurrence(&entry(time), date, &zone, Some((lat, lon))).unwrap());
            assert!(times.iter().all(|at| at.date_naive() == date));
            assert!(times.windows(2).all(|pair| pair[0] < pair[1]), "{times:?}");
        }
    }

    #[test]
    fn polar_events_are_independent_and_invalid_location_is_rejected() {
        let date = NaiveDate::from_ymd_opt(2024, 12, 21).unwrap();
        let zone = FixedOffset::east_opt(3600).unwrap();
        assert!(
            occurrence(
                &entry(ScheduleTime::Sunrise { offset: 0 }),
                date,
                &zone,
                Some((78.2, 15.6))
            )
            .is_none()
        );
        assert!(
            occurrence(
                &entry(ScheduleTime::Solar {
                    event: SolarEvent::SolarNoon,
                    offset: 0
                }),
                date,
                &zone,
                Some((78.2, 15.6))
            )
            .is_some()
        );
        assert!(
            occurrence(
                &entry(ScheduleTime::Sunset { offset: 0 }),
                date,
                &zone,
                Some((f64::NAN, 0.0))
            )
            .is_none()
        );
    }

    #[test]
    fn solar_offsets_cross_both_midnights_and_large_offsets_remain_active() {
        let zone = FixedOffset::east_opt(3 * 3600).unwrap();
        let date = NaiveDate::from_ymd_opt(2024, 3, 20).unwrap();
        for time in [
            ScheduleTime::Sunrise { offset: -1440 },
            ScheduleTime::Sunset { offset: 1440 },
            ScheduleTime::Sunset { offset: 5000 },
        ] {
            let item = entry(time);
            let at = occurrence(&item, date, &zone, Some((55.75, 37.62))).unwrap();
            let now = at + chrono::Duration::minutes(1);
            assert_eq!(active(&[item], &now, Some((55.75, 37.62))).unwrap().1, at);
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
