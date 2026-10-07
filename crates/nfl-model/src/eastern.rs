use time::{Date, Month, OffsetDateTime, UtcOffset, Weekday};

/// US Eastern offset for a date under post-2007 DST rules (second Sunday of
/// March through first Sunday of November). Correct for every season this
/// crate is configured to fetch; no NFL game kicks off inside the 2 AM
/// transition window.
pub fn eastern_offset(date: Date) -> UtcOffset {
    let dst_start = nth_weekday(date.year(), Month::March, Weekday::Sunday, 2);
    let dst_end = nth_weekday(date.year(), Month::November, Weekday::Sunday, 1);
    if date >= dst_start && date < dst_end {
        UtcOffset::from_hms(-4, 0, 0).expect("valid constant offset")
    } else {
        UtcOffset::from_hms(-5, 0, 0).expect("valid constant offset")
    }
}

/// Convert an instant to US Eastern wall-clock time under post-2007 DST rules.
pub fn to_eastern(instant: OffsetDateTime) -> OffsetDateTime {
    let utc = instant.to_offset(UtcOffset::UTC);
    let dst_start = nth_weekday(utc.year(), Month::March, Weekday::Sunday, 2)
        .with_hms(7, 0, 0)
        .expect("valid DST start time")
        .assume_utc();
    let dst_end = nth_weekday(utc.year(), Month::November, Weekday::Sunday, 1)
        .with_hms(6, 0, 0)
        .expect("valid DST end time")
        .assume_utc();
    let offset = if dst_start <= utc && utc < dst_end {
        UtcOffset::from_hms(-4, 0, 0).expect("valid constant offset")
    } else {
        UtcOffset::from_hms(-5, 0, 0).expect("valid constant offset")
    };
    instant.to_offset(offset)
}

pub(crate) fn nth_weekday(year: i32, month: Month, weekday: Weekday, n: u8) -> Date {
    let mut date = Date::from_calendar_date(year, month, 1).expect("first of month is valid");
    let mut seen = 0;
    loop {
        if date.weekday() == weekday {
            seen += 1;
            if seen == n {
                return date;
            }
        }
        date = date.next_day().expect("date within month range");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::{date, datetime, time};

    fn assert_local(
        instant: OffsetDateTime,
        expected_date: Date,
        expected_time: time::Time,
        expected_offset: UtcOffset,
    ) {
        let local = to_eastern(instant);
        assert_eq!(local.date(), expected_date);
        assert_eq!(local.time(), expected_time);
        assert_eq!(local.offset(), expected_offset);
    }

    #[test]
    fn converts_spring_transition_before_and_at_boundary() {
        let est = UtcOffset::from_hms(-5, 0, 0).expect("valid test offset");
        let edt = UtcOffset::from_hms(-4, 0, 0).expect("valid test offset");
        assert_local(
            datetime!(2025 - 03 - 09 06:59:59 UTC),
            date!(2025 - 03 - 09),
            time!(01:59:59),
            est,
        );
        assert_local(
            datetime!(2025 - 03 - 09 07:00 UTC),
            date!(2025 - 03 - 09),
            time!(03:00),
            edt,
        );
    }

    #[test]
    fn converts_fall_transition_before_and_at_boundary() {
        let edt = UtcOffset::from_hms(-4, 0, 0).expect("valid test offset");
        let est = UtcOffset::from_hms(-5, 0, 0).expect("valid test offset");
        assert_local(
            datetime!(2025 - 11 - 02 05:59:59 UTC),
            date!(2025 - 11 - 02),
            time!(01:59:59),
            edt,
        );
        assert_local(
            datetime!(2025 - 11 - 02 06:00 UTC),
            date!(2025 - 11 - 02),
            time!(01:00),
            est,
        );
    }

    #[test]
    fn converts_fall_transition_before_utc_midnight_as_same_eastern_date() {
        let edt = UtcOffset::from_hms(-4, 0, 0).expect("valid test offset");
        assert_local(
            datetime!(2025 - 11 - 02 04:30 UTC),
            date!(2025 - 11 - 02),
            time!(00:30),
            edt,
        );
    }
}
