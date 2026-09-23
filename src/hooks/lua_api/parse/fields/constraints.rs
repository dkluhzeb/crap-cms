//! Constraint parsing: numeric ranges, length bounds, default values, and the
//! `Constraints` struct that aggregates them for `validate_constraints`.

use anyhow::{Result, anyhow, bail};
use mlua::{Table, Value};
use serde_json::{Number as JsonNumber, Value as JsonValue};

use crate::core::{FieldType, PickerAppearance};

use super::super::helpers::{get_bool, get_string};

pub(super) fn parse_default_value(
    field_tbl: &Table,
    name: &str,
    field_type: &FieldType,
) -> Result<Option<JsonValue>> {
    let val: Value = field_tbl.get("default_value").unwrap_or(Value::Nil);
    let default_value = match val {
        Value::Boolean(b) => Some(JsonValue::Bool(b)),
        Value::Integer(i) => Some(JsonValue::Number(JsonNumber::from(i))),
        Value::Number(n) => JsonNumber::from_f64(n).map(JsonValue::Number),
        Value::String(s) => Some(JsonValue::String(s.to_str()?.to_string())),
        _ => None,
    };

    if let Some(ref dv) = default_value {
        let expected = match field_type {
            FieldType::Checkbox => Some(("boolean", dv.is_boolean())),
            FieldType::Number => Some(("number", dv.is_number())),
            FieldType::Text
            | FieldType::Textarea
            | FieldType::Email
            | FieldType::Code
            | FieldType::Richtext
            | FieldType::Select
            | FieldType::Radio
            | FieldType::Date => Some(("string", dv.is_string())),
            _ => None,
        };

        if let Some((expected_type, false)) = expected {
            let got = match dv {
                JsonValue::Bool(_) => "boolean",
                JsonValue::Number(_) => "number",
                JsonValue::String(_) => "string",
                _ => "unknown",
            };
            bail!(
                "Field '{name}': default_value type mismatch — expected {expected_type} but got {got}"
            );
        }
    }

    Ok(default_value)
}

pub(super) fn parse_date_config(
    field_tbl: &Table,
    name: &str,
    field_type: &FieldType,
) -> Result<(Option<PickerAppearance>, bool, Option<String>)> {
    if *field_type != FieldType::Date {
        return Ok((None, false, None));
    }

    let picker_appearance = match get_string(field_tbl, "picker_appearance") {
        Some(raw) => match raw.parse::<PickerAppearance>() {
            Ok(p) => Some(p),
            Err(e) => {
                tracing::warn!("Field '{}': {}; ignoring", name, e);
                None
            }
        },
        None => None,
    };

    let timezone = {
        let tz = get_bool(field_tbl, "timezone", false)?;
        let supports_tz = matches!(picker_appearance, Some(PickerAppearance::DayAndTime));

        if tz && !supports_tz {
            let appearance = picker_appearance
                .as_ref()
                .map_or("dayOnly", PickerAppearance::as_str);
            tracing::warn!(
                "Field '{}': timezone is not supported for '{}' picker; ignoring",
                name,
                appearance
            );
            false
        } else {
            tz
        }
    };

    let default_timezone = if timezone {
        get_string(field_tbl, "default_timezone")
    } else {
        None
    };

    Ok((picker_appearance, timezone, default_timezone))
}

pub(super) struct Constraints {
    pub(super) min_rows: Option<usize>,
    pub(super) max_rows: Option<usize>,
    pub(super) min_length: Option<usize>,
    pub(super) max_length: Option<usize>,
    pub(super) min: Option<f64>,
    pub(super) max: Option<f64>,
    pub(super) integer: bool,
}

pub(super) fn validate_constraints(name: &str, c: &Constraints) -> Result<()> {
    if let (Some(mn), Some(mx)) = (c.min_rows, c.max_rows)
        && mn > mx
    {
        bail!("Field '{name}': min_rows ({mn}) must not exceed max_rows ({mx})");
    }

    if let (Some(mn), Some(mx)) = (c.min_length, c.max_length)
        && mn > mx
    {
        bail!("Field '{name}': min_length ({mn}) must not exceed max_length ({mx})");
    }

    if let (Some(mn), Some(mx)) = (c.min, c.max)
        && mn > mx
    {
        bail!("Field '{name}': min ({mn}) must not exceed max ({mx})");
    }

    Ok(())
}

pub(super) fn parse_constraints(field_tbl: &Table, name: &str) -> Result<Constraints> {
    let constraints = Constraints {
        min_rows: get_count(field_tbl, name, "min_rows")?,
        max_rows: get_count(field_tbl, name, "max_rows")?,
        min_length: get_count(field_tbl, name, "min_length")?,
        max_length: get_count(field_tbl, name, "max_length")?,
        min: get_bound(field_tbl, name, "min")?,
        max: get_bound(field_tbl, name, "max")?,
        integer: get_bool(field_tbl, "integer", false)?,
    };

    validate_constraints(name, &constraints)?;

    Ok(constraints)
}

/// A count bound (`min_rows`, `max_length`, …). Absent is `None`; present, it
/// must be a non-negative whole number — a negative, fractional or wrong-typed
/// value is a load error, never silently dropped.
fn get_count(tbl: &Table, name: &str, key: &str) -> Result<Option<usize>> {
    let value = tbl.get::<Value>(key)?;

    let count = match &value {
        Value::Nil => return Ok(None),
        Value::Integer(i) => usize::try_from(*i).ok(),
        Value::Number(n) => whole_number(*n).and_then(|i| usize::try_from(i).ok()),
        _ => None,
    };

    count.map(Some).ok_or_else(|| {
        anyhow!(
            "Field '{name}': {key} must be a non-negative whole number, got {}",
            describe(&value)
        )
    })
}

/// A numeric bound (`min` / `max`). Absent is `None`; present, it must be a
/// finite number, and an integer must convert to `f64` exactly — anything else
/// is a load error, never silently dropped.
fn get_bound(tbl: &Table, name: &str, key: &str) -> Result<Option<f64>> {
    let value = tbl.get::<Value>(key)?;

    let bound = match &value {
        Value::Nil => return Ok(None),
        Value::Integer(i) => exact_f64(*i),
        Value::Number(n) => n.is_finite().then_some(*n),
        _ => None,
    };

    bound.map(Some).ok_or_else(|| {
        anyhow!(
            "Field '{name}': {key} must be a finite number (integers within ±2^53), got {}",
            describe(&value)
        )
    })
}

/// `n` as an integer when it is a whole number within the exactly
/// representable range.
#[allow(clippy::cast_possible_truncation, clippy::float_cmp)]
fn whole_number(n: f64) -> Option<i64> {
    // Exactness is checked before the cast: a whole number of magnitude
    // below 2^53 converts without truncation.
    (n.fract() == 0.0 && n.abs() < 9_007_199_254_740_992.0).then_some(n as i64)
}

/// `i` as `f64` when the conversion is exact (magnitude at most 2^53).
#[allow(clippy::cast_precision_loss)]
fn exact_f64(i: i64) -> Option<f64> {
    // Range-checked: every integer of magnitude ≤ 2^53 is exactly
    // representable as an `f64`.
    (i.unsigned_abs() <= 1 << 53).then_some(i as f64)
}

/// A config value as an error message shows it: numbers by value, anything
/// else by type.
fn describe(value: &Value) -> String {
    match value {
        Value::Integer(i) => i.to_string(),
        Value::Number(n) => n.to_string(),
        other => other.type_name().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::Lua;
    use serde_json::json;

    fn empty() -> Constraints {
        Constraints {
            min_rows: None,
            max_rows: None,
            min_length: None,
            max_length: None,
            min: None,
            max: None,
            integer: false,
        }
    }

    #[test]
    fn min_rows_exceeding_max_rows_errors() {
        let c = Constraints {
            min_rows: Some(5),
            max_rows: Some(3),
            ..empty()
        };
        let err = validate_constraints("items", &c).unwrap_err().to_string();
        assert!(err.contains("min_rows"), "{err}");
    }

    #[test]
    fn min_length_exceeding_max_length_errors() {
        let c = Constraints {
            min_length: Some(10),
            max_length: Some(2),
            ..empty()
        };
        let err = validate_constraints("title", &c).unwrap_err().to_string();
        assert!(err.contains("min_length"), "{err}");
    }

    #[test]
    fn min_exceeding_max_errors() {
        let c = Constraints {
            min: Some(10.0),
            max: Some(5.0),
            ..empty()
        };
        let err = validate_constraints("score", &c).unwrap_err().to_string();
        assert!(err.contains("min"), "{err}");
    }

    #[test]
    fn equal_ordered_and_absent_bounds_pass() {
        // equal is allowed (not strictly greater)
        assert!(
            validate_constraints(
                "x",
                &Constraints {
                    min: Some(5.0),
                    max: Some(5.0),
                    ..empty()
                }
            )
            .is_ok()
        );
        // properly ordered
        assert!(
            validate_constraints(
                "x",
                &Constraints {
                    min_length: Some(1),
                    max_length: Some(9),
                    ..empty()
                }
            )
            .is_ok()
        );
        // no bounds at all
        assert!(validate_constraints("x", &empty()).is_ok());
    }

    #[test]
    fn default_value_type_must_match_field_type() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();

        // Number default on a Number field is accepted.
        tbl.set("default_value", 42_i64).unwrap();
        assert_eq!(
            parse_default_value(&tbl, "count", &FieldType::Number).unwrap(),
            Some(json!(42))
        );

        // Number default on a Text field is a type mismatch.
        let err = parse_default_value(&tbl, "title", &FieldType::Text)
            .unwrap_err()
            .to_string();
        assert!(err.contains("type mismatch"), "{err}");

        // String default on a Text field is accepted.
        tbl.set("default_value", "hello").unwrap();
        assert_eq!(
            parse_default_value(&tbl, "title", &FieldType::Text).unwrap(),
            Some(json!("hello"))
        );

        // Bool fits Checkbox; a string does not.
        tbl.set("default_value", true).unwrap();
        assert_eq!(
            parse_default_value(&tbl, "active", &FieldType::Checkbox).unwrap(),
            Some(json!(true))
        );
        tbl.set("default_value", "yes").unwrap();
        assert!(parse_default_value(&tbl, "active", &FieldType::Checkbox).is_err());
    }

    fn constraints_from(src: &str) -> Result<Constraints> {
        let lua = Lua::new();
        let tbl: Table = lua.load(src).eval().unwrap();

        parse_constraints(&tbl, "f")
    }

    /// Regression: an integer `min`/`max` outside `i32` was silently dropped,
    /// leaving the field unbounded.
    #[test]
    fn large_integer_bounds_are_kept_exactly() {
        let c = constraints_from("{ min = 3000000000, max = 9007199254740992 }").unwrap();

        assert_eq!(c.min, Some(3_000_000_000.0));
        assert_eq!(c.max, Some(9_007_199_254_740_992.0));
        assert!(
            constraints_from("{ max = 9007199254740993 }").is_err(),
            "not exact in f64"
        );
        assert_eq!(constraints_from("{ min = -1.5 }").unwrap().min, Some(-1.5));
    }

    /// Regression: present-but-invalid bounds were silently dropped instead of
    /// failing the load.
    #[test]
    fn invalid_bounds_are_load_errors() {
        for src in [
            "{ min_rows = -1 }",
            "{ max_rows = 2.5 }",
            "{ min_length = '3' }",
            "{ max_length = true }",
            "{ min = 'low' }",
            "{ max = 0/0 }",
            "{ integer = 'yes' }",
        ] {
            let err = constraints_from(src).err();
            assert!(err.is_some(), "{src} must be rejected");
        }

        let err = constraints_from("{ min_rows = -1 }")
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("min_rows") && err.contains("-1"), "{err}");
    }

    #[test]
    fn whole_float_counts_are_accepted() {
        let c = constraints_from("{ min_rows = 2.0, max_length = 10 }").unwrap();

        assert_eq!(c.min_rows, Some(2));
        assert_eq!(c.max_length, Some(10));
    }
}
