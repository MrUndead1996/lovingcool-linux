#[cfg(not(unix))]
use time::UtcOffset;
use time::{OffsetDateTime, Weekday};

pub fn fields(local: OffsetDateTime) -> [u8; 8] {
    let (year, month, day) = local.to_calendar_date();
    [
        year.div_euclid(100).rem_euclid(100) as u8,
        year.rem_euclid(100) as u8,
        month as u8,
        day,
        local.hour(),
        local.minute(),
        local.second(),
        match local.weekday() {
            Weekday::Sunday => 0,
            weekday => weekday.number_days_from_monday() + 1,
        },
    ]
}

pub fn now_local() -> [u8; 8] {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as libc::time_t;
    local_fields_at(now)
}

#[cfg(not(unix))]
fn local_fields_at(timestamp: i64) -> [u8; 8] {
    let utc = OffsetDateTime::from_unix_timestamp(timestamp).unwrap_or(OffsetDateTime::UNIX_EPOCH);
    let local = UtcOffset::current_local_offset()
        .map(|offset| utc.to_offset(offset))
        .unwrap_or(utc);
    fields(local)
}

/// Convert a Unix timestamp using the OS's reentrant local-time conversion.
/// Unlike `time::UtcOffset::current_local_offset`, this works after worker
/// threads have been spawned; this process does not mutate `TZ`.
#[cfg(unix)]
fn local_fields_at(timestamp: libc::time_t) -> [u8; 8] {
    // SAFETY: `localtime_r` writes to the provided `tm` and does not retain
    // either pointer. Its result is valid when non-null.
    unsafe {
        let mut tm = std::mem::zeroed::<libc::tm>();
        if libc::localtime_r(&timestamp, &mut tm).is_null() {
            return fields(OffsetDateTime::now_utc());
        }
        let year = tm.tm_year + 1900;
        let month = u8::try_from(tm.tm_mon + 1).unwrap_or(1);
        let day = u8::try_from(tm.tm_mday).unwrap_or(1);
        let weekday = match tm.tm_wday {
            0 => Weekday::Sunday,
            1 => Weekday::Monday,
            2 => Weekday::Tuesday,
            3 => Weekday::Wednesday,
            4 => Weekday::Thursday,
            5 => Weekday::Friday,
            _ => Weekday::Saturday,
        };
        [
            year.div_euclid(100).rem_euclid(100) as u8,
            year.rem_euclid(100) as u8,
            month,
            day,
            u8::try_from(tm.tm_hour).unwrap_or(0),
            u8::try_from(tm.tm_min).unwrap_or(0),
            u8::try_from(tm.tm_sec).unwrap_or(0),
            match weekday {
                Weekday::Sunday => 0,
                weekday => weekday.number_days_from_monday() + 1,
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::{Date, Month, PrimitiveDateTime, Time, UtcOffset};
    #[test]
    fn calendar_handles_leap_day_and_weekday() {
        let dt = PrimitiveDateTime::new(
            Date::from_calendar_date(2024, Month::February, 29).unwrap(),
            Time::from_hms(23, 59, 58).unwrap(),
        )
        .assume_offset(UtcOffset::UTC);
        assert_eq!(fields(dt), [20, 24, 2, 29, 23, 59, 58, 4]);
    }

    #[cfg(unix)]
    #[test]
    fn local_time_conversion_observes_tz_in_a_multithreaded_process() {
        const CHILD: &str = "COOLING_DISPLAY_TZ_CHILD";
        if std::env::var_os(CHILD).is_some() {
            // POSIX TZ=UTC+5 means local time is UTC-05:00.
            assert_eq!(
                local_fields_at(1_704_067_200),
                [20, 23, 12, 31, 19, 0, 0, 0]
            );
            return;
        }
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("calendar::tests::local_time_conversion_observes_tz_in_a_multithreaded_process")
            .arg("--nocapture")
            .env(CHILD, "1")
            .env("TZ", "UTC+5")
            .status()
            .unwrap();
        assert!(status.success());
    }
}
