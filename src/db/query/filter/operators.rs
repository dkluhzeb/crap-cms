//! Operator SQL generation for individual filter conditions.

use anyhow::{Result, bail};
use tracing::warn;

use crate::core::FieldType;
use crate::db::{
    DbConnection, DbValue, Filter, FilterOp,
    query::{helpers::normalize_date_value, is_valid_identifier},
};

/// Coerce a filter input string to the correct [`DbValue`] variant for the
/// target field type.
///
/// Unlike the write-path `coerce_value` helper:
/// - Invalid numeric input falls back to `DbValue::Text` (and logs a warning)
///   rather than becoming `DbValue::Null`, so the user's intent — find rows
///   literally matching `"not-a-number"` — is preserved, while the mismatch
///   is observable.
/// - Empty strings remain as `DbValue::Text("")` rather than `Null`, because
///   filter semantics differ from write semantics (matching an empty column
///   vs. writing a null).
/// - Dates are normalized so filter inputs like `"2024-01-15"` align with
///   the ISO 8601 format written by the write path.
///
/// Text-only operators (`Like`, `Contains`) always bind as `DbValue::Text`
/// regardless of the field type — no numeric/date casting is meaningful.
pub(super) fn coerce_filter_value(
    field_type: Option<&FieldType>,
    op: &FilterOp,
    value: &str,
) -> DbValue {
    if is_text_only_op(op) {
        return DbValue::Text(value.to_string());
    }

    let Some(ft) = field_type else {
        return DbValue::Text(value.to_string());
    };

    match ft {
        FieldType::Number => match value.parse::<f64>() {
            Ok(n) if n.is_finite() => DbValue::Real(n),
            _ => {
                warn!(
                    "Filter value '{}' is not a valid finite number for Number field; \
                     falling back to Text comparison",
                    value
                );
                DbValue::Text(value.to_string())
            }
        },
        FieldType::Checkbox => match value {
            "1" | "true" | "yes" | "on" => DbValue::Integer(1),
            "0" | "false" | "no" | "off" => DbValue::Integer(0),
            _ => {
                warn!(
                    "Filter value '{}' is not a recognized boolean for Checkbox field; \
                     falling back to Text comparison",
                    value
                );
                DbValue::Text(value.to_string())
            }
        },
        FieldType::Date => DbValue::Text(normalize_date_value(value)),
        _ => DbValue::Text(value.to_string()),
    }
}

/// `Like` and `Contains` operate on string patterns and never benefit from
/// numeric or date casting, even when the target column is numeric.
fn is_text_only_op(op: &FilterOp) -> bool {
    matches!(op, FilterOp::Like(_) | FilterOp::Contains(_))
}

/// Generate a SQL condition applying a [`FilterOp`] to an arbitrary SQL
/// expression, appending bind parameters to `params`.
///
/// `field_type` informs how operand values are bound: `None` means the
/// caller could not determine the field type and we fall back to `Text`
/// (today's default behavior). See [`coerce_filter_value`] for the casting
/// rules.
pub(super) fn build_op_condition(
    conn: &dyn DbConnection,
    expr: &str,
    op: &FilterOp,
    field_type: Option<&FieldType>,
    params: &mut Vec<DbValue>,
) -> String {
    match op {
        FilterOp::Equals(v) => {
            let ph = conn.placeholder(params.len() + 1);
            params.push(coerce_filter_value(field_type, op, v));
            format!("{} = {}", expr, ph)
        }
        FilterOp::NotEquals(v) => {
            let ph = conn.placeholder(params.len() + 1);
            params.push(coerce_filter_value(field_type, op, v));
            format!("{} != {}", expr, ph)
        }
        FilterOp::Like(v) => {
            let ph = conn.placeholder(params.len() + 1);
            params.push(DbValue::Text(v.clone()));
            format!("{} {} {}", expr, conn.like_operator(), ph)
        }
        FilterOp::Contains(v) => {
            let escaped = v.replace('%', "\\%").replace('_', "\\_");
            let ph = conn.placeholder(params.len() + 1);
            params.push(DbValue::Text(format!("%{}%", escaped)));
            format!("{} {} {} ESCAPE '\\'", expr, conn.like_operator(), ph)
        }
        FilterOp::GreaterThan(v) => {
            let ph = conn.placeholder(params.len() + 1);
            params.push(coerce_filter_value(field_type, op, v));
            format!("{} > {}", expr, ph)
        }
        FilterOp::LessThan(v) => {
            let ph = conn.placeholder(params.len() + 1);
            params.push(coerce_filter_value(field_type, op, v));
            format!("{} < {}", expr, ph)
        }
        FilterOp::GreaterThanOrEqual(v) => {
            let ph = conn.placeholder(params.len() + 1);
            params.push(coerce_filter_value(field_type, op, v));
            format!("{} >= {}", expr, ph)
        }
        FilterOp::LessThanOrEqual(v) => {
            let ph = conn.placeholder(params.len() + 1);
            params.push(coerce_filter_value(field_type, op, v));
            format!("{} <= {}", expr, ph)
        }
        FilterOp::In(vals) => {
            // Empty IN list: "x IN ()" is a SQL error, so emit always-false.
            // Semantically correct: nothing is "in" an empty set.
            if vals.is_empty() {
                return "0 = 1".to_string();
            }

            let placeholders: Vec<_> = vals
                .iter()
                .map(|v| {
                    let ph = conn.placeholder(params.len() + 1);
                    params.push(coerce_filter_value(field_type, op, v));
                    ph
                })
                .collect();
            format!("{} IN ({})", expr, placeholders.join(", "))
        }
        FilterOp::NotIn(vals) => {
            // Empty NOT IN list: everything is "not in" an empty set.
            // Vacuously true — emit always-true to avoid SQL error.
            if vals.is_empty() {
                return "1 = 1".to_string();
            }

            let placeholders: Vec<_> = vals
                .iter()
                .map(|v| {
                    let ph = conn.placeholder(params.len() + 1);
                    params.push(coerce_filter_value(field_type, op, v));
                    ph
                })
                .collect();
            format!("{} NOT IN ({})", expr, placeholders.join(", "))
        }
        FilterOp::Exists => {
            format!("{} IS NOT NULL", expr)
        }
        FilterOp::NotExists => {
            format!("{} IS NULL", expr)
        }
    }
}

/// Build a single [`Filter`] into a SQL condition string and append its bind
/// parameters to `params`.
///
/// `field_type` is used to cast filter operand values to the correct
/// [`DbValue`] variant for comparison operators. Pass `None` when the field
/// type cannot be determined (values are bound as `DbValue::Text`, matching
/// today's default).
///
/// Defense-in-depth: rejects field names that are not valid SQL identifiers
/// (alphanumeric + underscore), even though higher-level validation should
/// have caught them already.
pub fn build_filter_condition(
    conn: &dyn DbConnection,
    f: &Filter,
    field_type: Option<&FieldType>,
    params: &mut Vec<DbValue>,
) -> Result<String> {
    if !is_valid_identifier(&f.field) {
        bail!(
            "Invalid field name '{}': must be alphanumeric/underscore",
            f.field
        );
    }
    Ok(build_op_condition(
        conn, &f.field, &f.op, field_type, params,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sqlite::InMemoryConn;
    use crate::db::{
        DbValue,
        query::{Filter, FilterOp},
    };

    fn conn() -> InMemoryConn {
        InMemoryConn::open()
    }

    #[test]
    fn filter_condition_equals() {
        let c = conn();
        let f = Filter {
            field: "status".into(),
            op: FilterOp::Equals("active".into()),
        };
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_filter_condition(&c, &f, None, &mut params).unwrap();
        assert_eq!(sql, "status = ?1");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn filter_condition_not_equals() {
        let c = conn();
        let f = Filter {
            field: "status".into(),
            op: FilterOp::NotEquals("draft".into()),
        };
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_filter_condition(&c, &f, None, &mut params).unwrap();
        assert_eq!(sql, "status != ?1");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn filter_condition_like() {
        let c = conn();
        let f = Filter {
            field: "title".into(),
            op: FilterOp::Like("%hello%".into()),
        };
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_filter_condition(&c, &f, None, &mut params).unwrap();
        assert_eq!(sql, "title LIKE ?1");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn filter_condition_contains() {
        let c = conn();
        let f = Filter {
            field: "body".into(),
            op: FilterOp::Contains("search term".into()),
        };
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_filter_condition(&c, &f, None, &mut params).unwrap();
        assert_eq!(sql, "body LIKE ?1 ESCAPE '\\'");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn filter_condition_greater_than() {
        let c = conn();
        let f = Filter {
            field: "age".into(),
            op: FilterOp::GreaterThan("18".into()),
        };
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_filter_condition(&c, &f, None, &mut params).unwrap();
        assert_eq!(sql, "age > ?1");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn filter_condition_less_than() {
        let c = conn();
        let f = Filter {
            field: "price".into(),
            op: FilterOp::LessThan("100".into()),
        };
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_filter_condition(&c, &f, None, &mut params).unwrap();
        assert_eq!(sql, "price < ?1");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn filter_condition_greater_than_or_equal() {
        let c = conn();
        let f = Filter {
            field: "score".into(),
            op: FilterOp::GreaterThanOrEqual("50".into()),
        };
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_filter_condition(&c, &f, None, &mut params).unwrap();
        assert_eq!(sql, "score >= ?1");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn filter_condition_less_than_or_equal() {
        let c = conn();
        let f = Filter {
            field: "rating".into(),
            op: FilterOp::LessThanOrEqual("5".into()),
        };
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_filter_condition(&c, &f, None, &mut params).unwrap();
        assert_eq!(sql, "rating <= ?1");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn filter_condition_in() {
        let c = conn();
        let f = Filter {
            field: "status".into(),
            op: FilterOp::In(vec!["a".into(), "b".into(), "c".into()]),
        };
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_filter_condition(&c, &f, None, &mut params).unwrap();
        assert_eq!(sql, "status IN (?1, ?2, ?3)");
        assert_eq!(params.len(), 3);
    }

    #[test]
    fn filter_condition_not_in() {
        let c = conn();
        let f = Filter {
            field: "role".into(),
            op: FilterOp::NotIn(vec!["banned".into(), "suspended".into()]),
        };
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_filter_condition(&c, &f, None, &mut params).unwrap();
        assert_eq!(sql, "role NOT IN (?1, ?2)");
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn filter_condition_exists() {
        let c = conn();
        let f = Filter {
            field: "avatar".into(),
            op: FilterOp::Exists,
        };
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_filter_condition(&c, &f, None, &mut params).unwrap();
        assert_eq!(sql, "avatar IS NOT NULL");
        assert_eq!(params.len(), 0);
    }

    #[test]
    fn filter_condition_not_exists() {
        let c = conn();
        let f = Filter {
            field: "deleted_at".into(),
            op: FilterOp::NotExists,
        };
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_filter_condition(&c, &f, None, &mut params).unwrap();
        assert_eq!(sql, "deleted_at IS NULL");
        assert_eq!(params.len(), 0);
    }

    #[test]
    fn filter_condition_in_empty_is_false() {
        let c = conn();
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_op_condition(&c, "status", &FilterOp::In(vec![]), None, &mut params);
        assert_eq!(sql, "0 = 1");
        assert_eq!(params.len(), 0);
    }

    #[test]
    fn filter_condition_not_in_empty_is_true() {
        let c = conn();
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_op_condition(&c, "status", &FilterOp::NotIn(vec![]), None, &mut params);
        assert_eq!(sql, "1 = 1");
        assert_eq!(params.len(), 0);
    }

    #[test]
    fn filter_condition_rejects_invalid_identifier() {
        let c = conn();
        let f = Filter {
            field: "field name".into(),
            op: FilterOp::Equals("v".into()),
        };
        let mut params: Vec<DbValue> = Vec::new();
        let result = build_filter_condition(&c, &f, None, &mut params);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Invalid field name")
        );
    }

    // ── Type-aware coercion regression tests ──────────────────────────────

    #[test]
    fn filter_number_gt_binds_real() {
        let c = conn();
        let f = Filter {
            field: "age".into(),
            op: FilterOp::GreaterThan("42".into()),
        };
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_filter_condition(&c, &f, Some(&FieldType::Number), &mut params).unwrap();
        assert_eq!(sql, "age > ?1");
        assert_eq!(params, vec![DbValue::Real(42.0)]);
    }

    #[test]
    fn filter_number_eq_binds_real() {
        let c = conn();
        let f = Filter {
            field: "score".into(),
            op: FilterOp::Equals("2.5".into()),
        };
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_filter_condition(&c, &f, Some(&FieldType::Number), &mut params).unwrap();
        assert_eq!(sql, "score = ?1");
        assert_eq!(params, vec![DbValue::Real(2.5)]);
    }

    #[test]
    fn filter_number_negative_binds_real() {
        // Regression: negative numbers must parse and bind as Real.
        let c = conn();
        let f = Filter {
            field: "balance".into(),
            op: FilterOp::LessThan("-12.5".into()),
        };
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_filter_condition(&c, &f, Some(&FieldType::Number), &mut params).unwrap();
        assert_eq!(sql, "balance < ?1");
        assert_eq!(params, vec![DbValue::Real(-12.5)]);
    }

    #[test]
    fn filter_number_scientific_notation_binds_real() {
        // Regression: scientific notation (Rust f64 parser accepts "1e3").
        let c = conn();
        let f = Filter {
            field: "big".into(),
            op: FilterOp::GreaterThanOrEqual("1e3".into()),
        };
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_filter_condition(&c, &f, Some(&FieldType::Number), &mut params).unwrap();
        assert_eq!(sql, "big >= ?1");
        assert_eq!(params, vec![DbValue::Real(1000.0)]);
    }

    #[test]
    fn filter_number_invalid_input_falls_back_to_text() {
        let c = conn();
        let f = Filter {
            field: "age".into(),
            op: FilterOp::GreaterThan("not-a-number".into()),
        };
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_filter_condition(&c, &f, Some(&FieldType::Number), &mut params).unwrap();
        assert_eq!(sql, "age > ?1");
        assert_eq!(params, vec![DbValue::Text("not-a-number".into())]);
    }

    #[test]
    fn filter_number_nan_and_infinity_fall_back_to_text() {
        // Regression: NaN/Infinity must not poison comparisons; fall back to Text.
        for bad in &["NaN", "inf", "infinity", "-inf"] {
            let c = conn();
            let f = Filter {
                field: "age".into(),
                op: FilterOp::GreaterThan((*bad).into()),
            };
            let mut params: Vec<DbValue> = Vec::new();
            build_filter_condition(&c, &f, Some(&FieldType::Number), &mut params).unwrap();
            assert_eq!(
                params,
                vec![DbValue::Text((*bad).into())],
                "expected Text fallback for '{}'",
                bad
            );
        }
    }

    #[test]
    fn filter_checkbox_true_binds_integer_1() {
        let c = conn();
        let f = Filter {
            field: "active".into(),
            op: FilterOp::Equals("true".into()),
        };
        let mut params: Vec<DbValue> = Vec::new();
        build_filter_condition(&c, &f, Some(&FieldType::Checkbox), &mut params).unwrap();
        assert_eq!(params, vec![DbValue::Integer(1)]);
    }

    #[test]
    fn filter_checkbox_various_boolean_inputs() {
        for (input, expected) in &[
            ("1", 1),
            ("true", 1),
            ("yes", 1),
            ("on", 1),
            ("0", 0),
            ("false", 0),
            ("no", 0),
            ("off", 0),
        ] {
            let c = conn();
            let f = Filter {
                field: "active".into(),
                op: FilterOp::Equals((*input).into()),
            };
            let mut params: Vec<DbValue> = Vec::new();
            build_filter_condition(&c, &f, Some(&FieldType::Checkbox), &mut params).unwrap();
            assert_eq!(
                params,
                vec![DbValue::Integer(*expected)],
                "'{}' should parse to {}",
                input,
                expected
            );
        }
    }

    #[test]
    fn filter_checkbox_unknown_input_falls_back_to_text() {
        let c = conn();
        let f = Filter {
            field: "active".into(),
            op: FilterOp::Equals("maybe".into()),
        };
        let mut params: Vec<DbValue> = Vec::new();
        build_filter_condition(&c, &f, Some(&FieldType::Checkbox), &mut params).unwrap();
        assert_eq!(params, vec![DbValue::Text("maybe".into())]);
    }

    #[test]
    fn filter_date_binds_normalized_text() {
        // A plain calendar date gets normalized to the stored ISO 8601 format.
        let c = conn();
        let f = Filter {
            field: "published_at".into(),
            op: FilterOp::GreaterThan("2024-01-15".into()),
        };
        let mut params: Vec<DbValue> = Vec::new();
        build_filter_condition(&c, &f, Some(&FieldType::Date), &mut params).unwrap();
        assert_eq!(
            params,
            vec![DbValue::Text("2024-01-15T12:00:00.000Z".into())],
            "Date filter values should be normalized to match stored ISO format"
        );
    }

    #[test]
    fn filter_text_contains_still_binds_text_even_with_number_type() {
        // Text-only operator must stay Text regardless of field type.
        let c = conn();
        let f = Filter {
            field: "title".into(),
            op: FilterOp::Contains("42".into()),
        };
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_filter_condition(&c, &f, Some(&FieldType::Number), &mut params).unwrap();
        assert!(sql.contains("LIKE"));
        assert_eq!(params, vec![DbValue::Text("%42%".into())]);
    }

    #[test]
    fn filter_text_like_binds_text_even_with_number_type() {
        let c = conn();
        let f = Filter {
            field: "code".into(),
            op: FilterOp::Like("10%".into()),
        };
        let mut params: Vec<DbValue> = Vec::new();
        build_filter_condition(&c, &f, Some(&FieldType::Number), &mut params).unwrap();
        assert_eq!(params, vec![DbValue::Text("10%".into())]);
    }

    #[test]
    fn filter_text_field_binds_text() {
        // Sanity check: plain Text fields still bind as Text.
        let c = conn();
        let f = Filter {
            field: "name".into(),
            op: FilterOp::Equals("alice".into()),
        };
        let mut params: Vec<DbValue> = Vec::new();
        build_filter_condition(&c, &f, Some(&FieldType::Text), &mut params).unwrap();
        assert_eq!(params, vec![DbValue::Text("alice".into())]);
    }

    #[test]
    fn filter_number_in_list_binds_real_values() {
        let c = conn();
        let f = Filter {
            field: "age".into(),
            op: FilterOp::In(vec!["18".into(), "21".into(), "30".into()]),
        };
        let mut params: Vec<DbValue> = Vec::new();
        build_filter_condition(&c, &f, Some(&FieldType::Number), &mut params).unwrap();
        assert_eq!(
            params,
            vec![
                DbValue::Real(18.0),
                DbValue::Real(21.0),
                DbValue::Real(30.0),
            ]
        );
    }
}
