//! Cron expression normalization and parsing.
//!
//! Schedules are written in standard crontab syntax; the `cron` crate parses
//! a near-but-not-quite variant of it. Everything that reconciles the two
//! lives here, behind the single [`parse_cron`] entry point.

use std::str::FromStr;

use cron::{Schedule, error::Error as CronError};

/// Highest day-of-week ordinal standard crontab accepts. Both `0` and `7`
/// mean Sunday there.
const CRONTAB_DOW_MAX: u32 = 7;

/// Zero-based index of the day-of-week field once a seconds field is present
/// (`sec min hour dom month dow [year]`).
const DOW_FIELD: usize = 5;

/// Translate one crontab day-of-week ordinal (`0`..=`7`, Sunday `0` or `7`)
/// into the `cron` crate's (`1`..=`7`, Sunday `1`).
fn remap_dow(value: u32) -> u32 {
    if value == 0 || value == CRONTAB_DOW_MAX {
        1
    } else {
        value + 1
    }
}

/// The crontab ordinals one day-of-week item covers, or `None` when the item
/// is not purely numeric. Named days (`MON`, `MON-FRI`) return `None` on
/// purpose: the `cron` crate resolves names to its own ordinals itself, so
/// they must pass through untouched.
fn crontab_dow_values(base: &str, step: u32) -> Option<Vec<u32>> {
    // A bare `n` covers only `n`; `n/step` counts from `n` up to the maximum,
    // the way crontab reads a step on a single value.
    let (lo, hi) = if let Some((lo, hi)) = base.split_once('-') {
        (lo.parse::<u32>().ok()?, hi.parse::<u32>().ok()?)
    } else {
        let n = base.parse::<u32>().ok()?;

        (n, if step > 1 { CRONTAB_DOW_MAX } else { n })
    };

    if lo > hi || hi > CRONTAB_DOW_MAX {
        return None;
    }

    let stride = usize::try_from(step).ok()?;

    Some((lo..=hi).step_by(stride).collect())
}

/// Render remapped ordinals, collapsing a contiguous run back into a range so
/// a normalized expression stays as readable as the operator's input.
fn render_dow_values(values: &[u32]) -> Option<String> {
    let (first, last) = (*values.first()?, *values.last()?);

    if values.len() > 1 && u32::try_from(values.len()).is_ok_and(|n| n == last - first + 1) {
        return Some(format!("{first}-{last}"));
    }

    Some(
        values
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(","),
    )
}

/// Remap one comma-separated day-of-week item. Anything that is not a plain
/// crontab ordinal — `*`, `?`, a name, a malformed step — is handed back
/// verbatim so the `cron` crate keeps ownership of both the wildcard
/// semantics and the parse error.
fn remap_dow_item(item: &str) -> String {
    let (base, step) = if let Some((base, step)) = item.split_once('/') {
        (base, step.parse::<u32>().unwrap_or(0))
    } else {
        (item, 1)
    };

    // `*` and `*/n` already line up: the crate's range starts at Sunday just
    // as crontab's does, so a step from the minimum selects the same days.
    if base == "*" || step == 0 {
        return item.to_string();
    }

    let Some(values) = crontab_dow_values(base, step) else {
        return item.to_string();
    };

    let mut mapped: Vec<u32> = values.into_iter().map(remap_dow).collect();
    mapped.sort_unstable();
    mapped.dedup();

    render_dow_values(&mapped).unwrap_or_else(|| item.to_string())
}

/// Remap a whole day-of-week field, item by item.
fn remap_dow_field(field: &str) -> String {
    field
        .split(',')
        .map(remap_dow_item)
        .collect::<Vec<_>>()
        .join(",")
}

/// Normalize a cron expression into what the `cron` crate parses.
///
/// Two translations, both of which make the crate speak standard crontab:
///
/// 1. **Seconds.** The crate expects 6 or 7 fields with a leading seconds
///    field; operators write the 5-field minute-granularity form
///    (`0 3 * * *`), so a `0` seconds field is prepended when there are
///    exactly 5.
/// 2. **Day of week.** The crate numbers days `1`..=`7` with **Sunday = 1**;
///    crontab numbers them `0`..=`6` with **Sunday = 0** (and accepts `7`
///    for Sunday as well). Every numeric ordinal in the day-of-week field is
///    remapped — single values, lists, ranges and steps alike. `*`, `?` and
///    named days (`MON`, `MON-FRI`) pass through untouched; the crate's own
///    name table already resolves to the right days.
///
/// Anything that is not a well-formed crontab ordinal is handed through
/// unchanged so the parse error comes from the `cron` crate, in its wording.
fn normalize_cron(expr: &str) -> String {
    let mut fields: Vec<String> = expr.split_whitespace().map(str::to_string).collect();

    if fields.len() == 5 {
        fields.insert(0, "0".to_string());
    }

    if let Some(dow) = fields.get(DOW_FIELD) {
        let remapped = remap_dow_field(dow);

        fields[DOW_FIELD] = remapped;
    }

    fields.join(" ")
}

/// Parse a cron expression written in crontab syntax.
///
/// The single parse entry point: the boot-time schedule validator and the
/// per-tick scheduler must agree on exactly which expressions are usable, so
/// neither calls `Schedule::from_str` directly.
///
/// # Errors
///
/// Returns the `cron` crate's parse error when the normalized expression is
/// not a valid schedule.
pub(crate) fn parse_cron(expr: &str) -> Result<Schedule, CronError> {
    Schedule::from_str(&normalize_cron(expr))
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use chrono::{DateTime, Datelike, Utc, Weekday};

    use super::*;

    /// The weekday the schedule next fires on, starting from a fixed instant.
    fn next_weekday(expr: &str) -> Weekday {
        let schedule = parse_cron(expr).expect("schedule should parse");
        let from: DateTime<Utc> = "2026-01-01T00:00:00Z".parse().expect("fixed instant");

        schedule
            .after(&from)
            .next()
            .expect("schedule should have a next fire time")
            .weekday()
    }

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
        // The weekday range shifts by one: crontab Mon-Fri (1-5) is the
        // crate's 2-6.
        let result = normalize_cron("*/5 9-17 * * 1-5");
        assert_eq!(result, "0 */5 9-17 * * 2-6");
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
        // Only the weekday range is renumbered; the other fields are copied.
        let result = normalize_cron("0-30/5 0-23 1-15 1-6 0-4");
        assert_eq!(result, "0 0-30/5 0-23 1-15 1-6 1-5");
    }

    #[test]
    fn normalize_cron_result_is_parseable() {
        // Verify that a normalized 5-field expression produces a valid cron schedule
        assert!(parse_cron("0 3 * * *").is_ok(), "should be parseable");
    }

    /// Regression: the `cron` crate numbers Sunday as 1, so an unmapped
    /// numeric weekday fired a day early — the documented "Mondays at 8am"
    /// example ran on Sunday.
    #[test]
    fn numeric_weekday_fires_on_the_crontab_day() {
        assert_eq!(next_weekday("0 8 * * 1"), Weekday::Mon);
        assert_eq!(next_weekday("0 8 * * 2"), Weekday::Tue);
        assert_eq!(next_weekday("0 8 * * 6"), Weekday::Sat);
    }

    /// Regression: `0` is the standard crontab spelling of Sunday and used to
    /// fail to parse outright, so the job warned every tick and never ran.
    /// `7` is the accepted alias for the same day.
    #[test]
    fn both_crontab_spellings_of_sunday_parse_and_mean_sunday() {
        assert_eq!(next_weekday("0 3 * * 0"), Weekday::Sun);
        assert_eq!(next_weekday("0 3 * * 7"), Weekday::Sun);
        assert_eq!(normalize_cron("0 3 * * 0"), normalize_cron("0 3 * * 7"));
    }

    #[test]
    fn weekday_lists_ranges_and_steps_are_remapped() {
        // List: Mon, Wed, Fri.
        assert_eq!(normalize_cron("0 8 * * 1,3,5"), "0 0 8 * * 2,4,6");
        // Range: Mon-Fri.
        assert_eq!(normalize_cron("0 8 * * 1-5"), "0 0 8 * * 2-6");
        // Full week, either spelling.
        assert_eq!(normalize_cron("0 8 * * 0-6"), "0 0 8 * * 1-7");
        assert_eq!(normalize_cron("0 8 * * 1-7"), "0 0 8 * * 1-7");
        // Range with a step: every other day starting Sunday.
        assert_eq!(normalize_cron("0 8 * * 0-6/2"), "0 0 8 * * 1,3,5,7");
    }

    /// A range that wraps across Sunday cannot stay a range once renumbered —
    /// it becomes the explicit list of the same days.
    #[test]
    fn weekday_range_wrapping_sunday_becomes_a_list() {
        // Fri, Sat, Sun.
        assert_eq!(normalize_cron("0 8 * * 5-7"), "0 0 8 * * 1,6,7");
    }

    /// `*` and `*/n` already select the same days under both numberings —
    /// each range starts at Sunday — so they are left exactly as written.
    /// `*/2` is Sun/Tue/Thu/Sat; from the fixed Thursday instant the next
    /// fire is that same Thursday at 08:00.
    #[test]
    fn wildcard_weekday_is_untouched() {
        assert_eq!(normalize_cron("0 8 * * *"), "0 0 8 * * *");
        assert_eq!(normalize_cron("0 8 * * */2"), "0 0 8 * * */2");
        assert_eq!(next_weekday("0 8 * * */2"), Weekday::Thu);
        assert_eq!(next_weekday("0 8 * * 5"), Weekday::Fri);
    }

    /// Named days are the `cron` crate's own vocabulary and must not be
    /// shifted a second time.
    #[test]
    fn named_weekdays_are_untouched_and_mean_what_they_say() {
        assert_eq!(normalize_cron("0 8 * * MON-FRI"), "0 0 8 * * MON-FRI");
        assert_eq!(next_weekday("0 8 * * MON"), Weekday::Mon);
        assert_eq!(next_weekday("0 8 * * SUN"), Weekday::Sun);
    }

    #[test]
    fn out_of_range_and_malformed_weekdays_reach_the_parser_unchanged() {
        // 9 is not a crontab weekday; the `cron` crate reports it.
        assert_eq!(normalize_cron("0 8 * * 9"), "0 0 8 * * 9");
        assert!(parse_cron("0 8 * * 9").is_err());
        // A zero step would be a division by nothing; leave it to the parser.
        assert_eq!(normalize_cron("0 8 * * 1/0"), "0 0 8 * * 1/0");
        assert!(parse_cron("not a valid cron").is_err());
    }
}
