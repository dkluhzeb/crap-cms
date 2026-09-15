//! Cron expression normalization.

/// Normalize a cron expression: the `cron` crate expects 6 or 7 fields (with a
/// leading seconds field), but users write standard 5-field cron (`0 3 * * *`).
/// If the expression has exactly 5 fields, prepend "0" for seconds.
pub(crate) fn normalize_cron(expr: &str) -> String {
    let fields: Vec<&str> = expr.split_whitespace().collect();

    if fields.len() == 5 {
        format!("0 {}", fields.join(" "))
    } else {
        fields.join(" ")
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use std::str::FromStr;

    use cron::Schedule;

    use super::*;

    #[test]
    fn normalize_cron_prepends_seconds_to_5_field_expr() {
        // A 5-field (minute-granularity) cron gets a leading "0 " seconds field.
        assert_eq!(normalize_cron("*/5 * * * *"), "0 */5 * * * *");
    }

    #[test]
    fn normalize_cron_passes_through_6_field_expr() {
        // Already 6 fields (seconds present) → unchanged (whitespace collapsed).
        assert_eq!(normalize_cron("30 */5 * * * *"), "30 */5 * * * *");
    }

    #[test]
    fn normalize_cron_collapses_whitespace() {
        assert_eq!(normalize_cron("  */5   *  * * *  "), "0 */5 * * * *");
    }

    #[test]
    fn normalize_cron_five_fields() {
        let result = normalize_cron("0 3 * * *");
        assert_eq!(result, "0 0 3 * * *");
    }

    #[test]
    fn normalize_cron_six_fields_unchanged() {
        let result = normalize_cron("0 0 3 * * *");
        assert_eq!(result, "0 0 3 * * *");
    }

    #[test]
    fn normalize_cron_seven_fields_unchanged() {
        let result = normalize_cron("0 0 3 * * * 2024");
        assert_eq!(result, "0 0 3 * * * 2024");
    }

    #[test]
    fn normalize_cron_every_minute() {
        let result = normalize_cron("* * * * *");
        assert_eq!(result, "0 * * * * *");
    }

    #[test]
    fn normalize_cron_complex_expression() {
        let result = normalize_cron("*/5 9-17 * * 1-5");
        assert_eq!(result, "0 */5 9-17 * * 1-5");
    }

    #[test]
    fn normalize_cron_empty_string() {
        let result = normalize_cron("");
        assert_eq!(result, "");
    }

    #[test]
    fn normalize_cron_single_field() {
        let result = normalize_cron("*");
        assert_eq!(result, "*");
    }

    #[test]
    fn normalize_cron_two_fields() {
        let result = normalize_cron("0 3");
        assert_eq!(result, "0 3");
    }

    #[test]
    fn normalize_cron_four_fields() {
        let result = normalize_cron("0 3 * *");
        assert_eq!(result, "0 3 * *");
    }

    #[test]
    fn normalize_cron_extra_whitespace() {
        // split_whitespace handles multiple spaces — normalizes to single spaces
        let result = normalize_cron("0  3  *  *  *");
        assert_eq!(result, "0 0 3 * * *");
    }

    #[test]
    fn normalize_cron_with_ranges_and_steps() {
        let result = normalize_cron("0-30/5 0-23 1-15 1-6 0-4");
        assert_eq!(result, "0 0-30/5 0-23 1-15 1-6 0-4");
    }

    #[test]
    fn normalize_cron_result_is_parseable() {
        // Verify that a normalized 5-field expression produces a valid cron schedule
        let normalized = normalize_cron("0 3 * * *");
        let schedule = Schedule::from_str(&normalized);
        assert!(
            schedule.is_ok(),
            "Normalized expression should be parseable"
        );
    }
}
