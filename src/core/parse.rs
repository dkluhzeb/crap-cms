//! Shared scalar-value parsers. One token set for boolean spellings so a value
//! can never mean different things across the write-coerce edge, the SQL and
//! in-memory filter evaluators, and the auth `_locked`/`_verified` reader — the
//! divergence that let a `_locked = "on"` read as *not locked* (fail-open). One
//! reading of a number spelled as text, so validation never rejects a number
//! the write stores.

use serde_json::Value;

/// Parse a boolean spelling, tri-state: `Some(true)` for `1`/`true`/`yes`/`on`,
/// `Some(false)` for `0`/`false`/`no`/`off`, `None` for anything else. Trimmed
/// and case-insensitive.
#[must_use]
pub fn parse_bool(s: &str) -> Option<bool> {
    let s = s.trim();
    if ["1", "true", "yes", "on"]
        .iter()
        .any(|t| s.eq_ignore_ascii_case(t))
    {
        return Some(true);
    }
    if ["0", "false", "no", "off"]
        .iter()
        .any(|t| s.eq_ignore_ascii_case(t))
    {
        return Some(false);
    }
    None
}

/// Whether a number counts as checked: anything other than zero. `NaN` does
/// not (the comparison is written so it fails closed rather than passing).
fn non_zero(f: f64) -> bool {
    f.abs() > 0.0
}

/// Read a checkbox value, tri-state — the one rule its column, its value inside
/// a JSON-stored row, the write-coerce edge and every display share:
///
/// - a bool is itself;
/// - a number is checked when it is anything other than zero (`2`, `0.5` and
///   `-1` are checked; `0` and `0.0` are not);
/// - a string is checked when it is a true spelling ([`parse_bool`]:
///   `1`/`true`/`yes`/`on`, trimmed and case-insensitive) or a non-zero number
///   ([`parse_number`]), so a numeric string agrees with the number it spells
///   (`"2"` is checked, exactly as `2` is);
/// - anything else — `null`, an array, an object, an unrecognized string — is
///   **not a checkbox value** and reads as `None`, which the truthy forms take
///   as unchecked and validation can reject.
#[must_use]
pub fn checkbox_value(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(checked) => Some(*checked),
        Value::Number(n) => Some(n.as_f64().is_some_and(non_zero)),
        Value::String(s) => parse_checkbox_str(s),
        _ => None,
    }
}

/// [`checkbox_value`] for a value spelled as text.
fn parse_checkbox_str(s: &str) -> Option<bool> {
    parse_bool(s).or_else(|| parse_number(s).map(non_zero))
}

/// Truthy test: `true` only for a recognized checked spelling — a true spelling
/// (`1`/`true`/`yes`/`on`, trimmed + case-insensitive) or a non-zero number;
/// every other value — unrecognized OR unchecked — is `false`. The string form
/// of [`checkbox_value`], used at the write-coerce edge and by the auth flag
/// reader (where the *safe* direction for `_locked` is "any truthy spelling
/// counts as locked").
#[must_use]
pub fn parse_truthy(s: &str) -> bool {
    parse_checkbox_str(s) == Some(true)
}

/// Truthy test for a JSON value: [`checkbox_value`], with an unrecognized value
/// reading as unchecked.
#[must_use]
pub fn json_truthy(value: &Value) -> bool {
    checkbox_value(value) == Some(true)
}

/// JS-style truthiness of a JSON value — the one rule the admin form's
/// client-side condition evaluator (`static/components/conditions.js`), the
/// server-side condition evaluator and the template helpers share, so a
/// condition decides the same way in the browser and on the server:
///
/// - `null`, `false`, `""` and `0` are falsy;
/// - an **empty array** is falsy (the one deliberate departure from `Boolean()`:
///   an empty has-many/array field means "nothing selected");
/// - every object is truthy, empty or not, as in JS;
/// - everything else is truthy.
///
/// Distinct from [`json_truthy`], the checkbox rule: there a string is read for
/// the flag it spells (`"off"` is unchecked), here any non-empty string is
/// truthy.
#[must_use]
pub fn value_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty(),
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::Array(a) => !a.is_empty(),
        Value::Object(_) => true,
    }
}

/// Read a number spelled as text, surrounding whitespace ignored — the one
/// reading every number value shares (the write, validation, and has-many
/// elements). `None` when the text isn't a number. A non-finite spelling
/// (`inf`, `NaN`) reads as its value: the write stores nothing for it and
/// validation rejects it as not finite.
#[must_use]
pub fn parse_number(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn json_truthy_follows_the_string_spellings_and_non_zero_numbers() {
        for checked in [
            json!(true),
            json!("On"),
            json!(" yes "),
            json!(1),
            json!(2),
            json!(1.0),
            json!(0.5),
            json!(-1),
            json!(1e-20),
        ] {
            assert!(json_truthy(&checked), "{checked}");
        }
        for unchecked in [
            json!(false),
            json!("off"),
            json!(""),
            json!(0),
            json!(0.0),
            json!(null),
            json!([1]),
            json!({ "a": 1 }),
        ] {
            assert!(!json_truthy(&unchecked), "{unchecked}");
        }
    }

    /// A numeric string means what the number means: `"2"` is checked because
    /// `2` is. Before, a numeric spelling fell through the boolean token set and
    /// read as unchecked, so the same value disagreed with itself depending on
    /// whether it arrived typed or as text.
    #[test]
    fn a_numeric_string_agrees_with_the_number_it_spells() {
        for checked in [json!("2"), json!(" 2 "), json!("0.5"), json!("-1")] {
            assert_eq!(checkbox_value(&checked), Some(true), "{checked}");
            assert!(json_truthy(&checked), "{checked}");
        }

        for unchecked in [json!("0"), json!(" 0.0 "), json!("-0")] {
            assert_eq!(checkbox_value(&unchecked), Some(false), "{unchecked}");
        }
    }

    /// The tri-state: a value that is neither a bool spelling nor a number is
    /// not a checkbox value at all, so a caller can reject it instead of
    /// silently reading it as unchecked.
    #[test]
    fn checkbox_value_is_none_for_a_value_that_is_not_a_checkbox() {
        for known in [
            (json!(true), true),
            (json!(false), false),
            (json!(2), true),
            (json!(0.5), true),
            (json!(0), false),
            (json!("off"), false),
        ] {
            assert_eq!(checkbox_value(&known.0), Some(known.1), "{}", known.0);
        }

        for unknown in [json!("maybe"), json!(""), json!([]), json!({}), json!(null)] {
            assert_eq!(checkbox_value(&unknown), None, "{unknown}");
            assert!(!json_truthy(&unknown), "{unknown}");
        }
    }

    /// JS truthiness, as the browser-side condition evaluator computes it: an
    /// object is truthy however empty, an empty array is not. The two evaluators
    /// disagreed on `{}` — a condition could show a field in the browser and
    /// hide it on the server.
    #[test]
    fn value_truthy_follows_the_client_side_rule() {
        for truthy in [
            json!(true),
            json!("0"),
            json!("off"),
            json!(1),
            json!(-1),
            json!([1]),
            json!({}),
            json!({ "a": 1 }),
        ] {
            assert!(value_truthy(&truthy), "{truthy}");
        }

        for falsy in [json!(null), json!(false), json!(""), json!(0), json!(0.0)] {
            assert!(!value_truthy(&falsy), "{falsy}");
        }
    }

    #[test]
    fn parse_number_ignores_surrounding_whitespace() {
        for (input, expected) in [("5", 5.0), (" 5", 5.0), ("5 ", 5.0), ("\t-2.5\n", -2.5)] {
            assert_eq!(parse_number(input), Some(expected), "{input:?}");
        }

        for not_a_number in ["", " ", "abc", "5 5", "1,5"] {
            assert_eq!(parse_number(not_a_number), None, "{not_a_number:?}");
        }
    }

    /// A non-finite spelling reads as its value, so the callers can tell "not
    /// finite" apart from "not a number".
    #[test]
    fn parse_number_reads_non_finite_spellings() {
        assert!(parse_number(" inf ").is_some_and(f64::is_infinite));
        assert!(parse_number("NaN").is_some_and(f64::is_nan));
    }

    #[test]
    fn parse_bool_tristate_case_insensitive() {
        for t in [
            "1", "true", "TRUE", "True", "yes", "YES", "on", "On", " on ",
        ] {
            assert_eq!(parse_bool(t), Some(true), "{t:?}");
        }
        for f in ["0", "false", "FALSE", "no", "NO", "off", "Off"] {
            assert_eq!(parse_bool(f), Some(false), "{f:?}");
        }
        for u in ["", "maybe", "2", "yep"] {
            assert_eq!(parse_bool(u), None, "{u:?}");
        }
    }

    #[test]
    fn parse_truthy_recognizes_on_and_case() {
        // The fail-open regression: `on`/`On` must be truthy (was missing from
        // the auth flag reader, which also skipped case-folding).
        assert!(parse_truthy("on"));
        assert!(parse_truthy("On"));
        assert!(parse_truthy("TRUE"));
        // A numeric spelling means what the number means.
        assert!(parse_truthy("2"));
        assert!(parse_truthy(" -0.5 "));
        // Unrecognized / false spellings are not truthy.
        assert!(!parse_truthy("off"));
        assert!(!parse_truthy("0"));
        assert!(!parse_truthy(""));
        assert!(!parse_truthy("nope"));
    }
}
