//! Constraint parsing: numeric ranges, length bounds, default values, and the
//! `Constraints` struct that aggregates them for `validate_constraints`.

use anyhow::{Result, bail};
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
    let min_rows = field_tbl.get::<Option<usize>>("min_rows").ok().flatten();
    let max_rows = field_tbl.get::<Option<usize>>("max_rows").ok().flatten();
    let min_length = field_tbl.get::<Option<usize>>("min_length").ok().flatten();
    let max_length = field_tbl.get::<Option<usize>>("max_length").ok().flatten();

    let min = match field_tbl.get::<Value>("min") {
        Ok(Value::Number(n)) => Some(n),
        Ok(Value::Integer(i)) => i32::try_from(i).ok().map(f64::from),
        _ => None,
    };

    let max = match field_tbl.get::<Value>("max") {
        Ok(Value::Number(n)) => Some(n),
        Ok(Value::Integer(i)) => i32::try_from(i).ok().map(f64::from),
        _ => None,
    };

    let integer = field_tbl
        .get::<Option<bool>>("integer")
        .ok()
        .flatten()
        .unwrap_or(false);

    let constraints = Constraints {
        min_rows,
        max_rows,
        min_length,
        max_length,
        min,
        max,
        integer,
    };

    validate_constraints(name, &constraints)?;

    Ok(constraints)
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
}
