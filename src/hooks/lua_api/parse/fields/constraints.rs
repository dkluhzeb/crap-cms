//! Constraint parsing: numeric ranges, length bounds, default values, and the
//! `Constraints` struct that aggregates them for `validate_constraints`.

use anyhow::{Result, bail};
use mlua::{Table, Value};
use serde_json::{Number as JsonNumber, Value as JsonValue};

use crate::core::FieldType;

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
) -> Result<(Option<String>, bool, Option<String>)> {
    if *field_type != FieldType::Date {
        return Ok((None, false, None));
    }

    let picker_appearance = get_string(field_tbl, "picker_appearance");

    let timezone = {
        let tz = get_bool(field_tbl, "timezone", false)?;
        let appearance = picker_appearance.as_deref().unwrap_or("dayOnly");

        if tz && matches!(appearance, "dayOnly" | "timeOnly" | "monthOnly") {
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

    let constraints = Constraints {
        min_rows,
        max_rows,
        min_length,
        max_length,
        min,
        max,
    };

    validate_constraints(name, &constraints)?;

    Ok(constraints)
}
