/// Human cadence for the sync interval, e.g. `every hour`.
pub fn interval_label(secs: u64) -> String {
    let (n, unit) = if secs.is_multiple_of(3600) {
        (secs / 3600, "hour")
    } else if secs.is_multiple_of(60) {
        (secs / 60, "minute")
    } else {
        return format!("every {secs} seconds");
    };
    if n == 1 {
        format!("every {unit}")
    } else {
        format!("every {n} {unit}s")
    }
}

#[cfg(test)]
mod tests {
    use super::interval_label;

    #[test]
    fn interval_label_formats_hours_minutes_and_seconds() {
        assert_eq!(interval_label(3600), "every hour");
        assert_eq!(interval_label(7200), "every 2 hours");
        assert_eq!(interval_label(5400), "every 90 minutes");
        assert_eq!(interval_label(60), "every minute");
        assert_eq!(interval_label(900), "every 15 minutes");
        assert_eq!(interval_label(45), "every 45 seconds");
    }
}
