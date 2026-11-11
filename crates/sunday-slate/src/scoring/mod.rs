pub mod service;

/// The score that wins a contest: the highest, and only when it beats zero.
/// A slate nobody has played yet scores every lineup at 0.0, and that has no
/// winner.
pub fn winning_score(points: impl IntoIterator<Item = f64>) -> Option<f64> {
    points.into_iter().reduce(f64::max).filter(|top| *top > 0.0)
}

/// Whether two scores count as the same score.
///
/// Rounds the way `format_points` does, so a co-winner is credited exactly
/// when the page shows equal figures. Exact `f64` equality would drop a
/// rightful co-winner whenever summation order left two identical stat lines
/// differing in the last bit.
pub fn ties(a: f64, b: f64) -> bool {
    hundredths(a) == hundredths(b)
}

/// A score in whole hundredths — the precision at which the league reads a
/// score, and so the precision at which two scores are the same score.
fn hundredths(points: f64) -> i64 {
    (points * 100.0).round() as i64
}

/// A score as the reader sees it: `1247.5` -> `"1,247.50"`.
///
/// Rounds to whole hundredths first, rather than `trunc`/`fract`, so `9.999`
/// carries into the whole part instead of giving `"9.100"`, and a negative
/// score keeps its sign.
pub fn format_points(points: f64) -> String {
    let hundredths = hundredths(points);
    let sign = if hundredths < 0 { "-" } else { "" };
    let hundredths = hundredths.unsigned_abs();
    let whole = group_digits(&(hundredths / 100).to_string());
    let frac = hundredths % 100;
    format!("{sign}{whole}.{frac:02}")
}

/// A whole-dollar figure as the reader sees it: `5700` -> `"5,700"`.
pub fn format_thousands(v: i64) -> String {
    let sign = if v < 0 { "-" } else { "" };
    format!("{sign}{}", group_digits(&v.unsigned_abs().to_string()))
}

/// `"1234567"` -> `"1,234,567"`.
fn group_digits(digits: &str) -> String {
    let mut out = String::new();
    for (i, ch) in digits.char_indices() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn winning_score_is_the_maximum_above_zero() {
        assert_eq!(winning_score(vec![3.0, 17.5, 9.0]), Some(17.5));
        assert_eq!(winning_score(vec![4.25]), Some(4.25));
        assert_eq!(winning_score(Vec::<f64>::new()), None);
        assert_eq!(winning_score(vec![0.0, 0.0]), None);
        assert_eq!(winning_score(vec![-8.0, -2.0]), None);
    }

    #[test]
    fn format_points_groups_signs_and_rounds() {
        assert_eq!(format_points(1247.5), "1,247.50");
        assert_eq!(format_points(-3.2), "-3.20");
        assert_eq!(format_points(9.999), "10.00");
        assert_eq!(format_points(1234567.0), "1,234,567.00");
    }

    #[test]
    fn format_thousands_groups_and_signs() {
        assert_eq!(format_thousands(700), "700");
        assert_eq!(format_thousands(5700), "5,700");
        assert_eq!(format_thousands(-1234567), "-1,234,567");
    }

    #[test]
    fn two_scores_tie_exactly_when_they_display_the_same() {
        // The tie rule and the display rule are the same rounding, so a
        // co-winner is credited if and only if the page shows equal figures.
        for (a, b) in [(122.26, 122.26 + 1e-12), (9.999, 10.0), (0.0, -0.001)] {
            assert_eq!(ties(a, b), format_points(a) == format_points(b), "{a} {b}");
        }
    }

    #[test]
    fn ties_ignores_differences_below_display_precision() {
        // The same stat line summed in a different order can land a bit apart.
        assert!(ties(122.26, 122.26 + 1e-12));
        assert!(ties(0.0, 0.0));
        assert!(!ties(122.26, 122.27));
        assert!(!ties(10.0, 90.0));
    }
}
