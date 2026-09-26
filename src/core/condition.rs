//! Display-condition expressions emitted by Lua hooks.
//!
//! A typed contract between server-side condition hooks (Lua →
//! [`ConditionExpr`]) and the browser-side evaluator in
//! `static/components/conditions.js`. Both sides MUST stay in sync —
//! [`ConditionExpr::evaluate`] and the JS `evaluate()` function should
//! produce identical results for the same inputs.
//!
//! # Grammar
//!
//! ```text
//! ConditionExpr := ConditionRow | [ConditionRow, ...]   # array = AND
//! ConditionRow  := { field: String, <op> }
//! op            := equals: Value
//!                | not_equals: Value
//!                | in: [Value, ...]
//!                | not_in: [Value, ...]
//!                | is_truthy: bool
//!                | is_falsy: bool
//! ```
//!
//! Lua hooks may return either a single row or an array of rows; the
//! `untagged` enum chooses at deserialize time. Top-level
//! `is_truthy`/`is_falsy` operators take a literal `true` to opt in;
//! `false` is a documented no-op (legacy behavior — kept for
//! grep-equivalence with the JS evaluator).

use std::cmp::Ordering;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::value_truthy;

/// A display-condition expression. Accepts either a single row or an
/// array of rows AND'd together.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ConditionExpr {
    /// AND of multiple condition rows.
    All(Vec<ConditionRow>),

    /// Single condition row.
    Single(ConditionRow),
}

/// One row of a [`ConditionExpr`]: a field reference plus an operator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConditionRow {
    pub field: String,

    #[serde(flatten)]
    pub op: ConditionOp,
}

/// Per-row comparison operator.
///
/// Externally tagged: serializes as `{ "<op>": <payload> }`, so a row
/// reads as `{ "field": "...", "<op>": <payload> }` after `flatten`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConditionOp {
    Equals(Value),
    NotEquals(Value),
    In(Vec<Value>),
    NotIn(Vec<Value>),
    IsTruthy(bool),
    IsFalsy(bool),
}

impl ConditionExpr {
    /// Evaluate against form data. Returns whether the field should be visible.
    ///
    /// `data` is the form snapshot, indexed by field name. Missing fields
    /// resolve to JSON `null`.
    #[must_use]
    pub fn evaluate(&self, data: &Value) -> bool {
        match self {
            Self::All(rows) => rows.iter().all(|r| r.evaluate(data)),
            Self::Single(row) => row.evaluate(data),
        }
    }
}

impl ConditionRow {
    fn evaluate(&self, data: &Value) -> bool {
        if self.field.is_empty() {
            // Empty field reference — same default as the legacy evaluator.
            return true;
        }

        let field_val = field_value(data, &self.field);

        match &self.op {
            ConditionOp::Equals(v) => same_value(field_val, v),
            ConditionOp::NotEquals(v) => !same_value(field_val, v),
            ConditionOp::In(list) => list.iter().any(|v| same_value(field_val, v)),
            ConditionOp::NotIn(list) => !list.iter().any(|v| same_value(field_val, v)),
            ConditionOp::IsTruthy(true) => value_truthy(field_val),
            ConditionOp::IsFalsy(true) => !value_truthy(field_val),
            ConditionOp::IsTruthy(false) | ConditionOp::IsFalsy(false) => true,
        }
    }
}

/// Structural equality of two JSON values with every number compared by its
/// value — `5` and `5.0` are one number, as they are to the browser
/// evaluator's `sameValue`. A whole number is stored as an integer, while a
/// condition written as `equals = 5.0` arrives as a float.
fn same_value(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            x.as_f64().partial_cmp(&y.as_f64()) == Some(Ordering::Equal)
        }
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(x, y)| same_value(x, y))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(key, v)| y.get(key).is_some_and(|w| same_value(v, w)))
        }
        _ => a == b,
    }
}

/// The value a condition row's `field` names in the condition data: a
/// top-level field by its name, a field inside a group by its dotted
/// (`seo.title`) or form-name (`seo__title`) path. A missing value is `null`.
///
/// The browser evaluator (`static/components/conditions.js`, `fieldValue`)
/// resolves a path the same way.
fn field_value<'a>(data: &'a Value, field: &str) -> &'a Value {
    field
        .split('.')
        .flat_map(|segment| segment.split("__"))
        .try_fold(data, |level, key| level.get(key))
        .unwrap_or(&Value::Null)
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::items_after_statements,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::unreadable_literal,
    clippy::used_underscore_binding
)]
mod tests {
    use serde_json::json;

    use super::*;

    fn parse(v: Value) -> ConditionExpr {
        serde_json::from_value(v).expect("parse")
    }

    fn round_trip(v: Value) -> Value {
        let expr: ConditionExpr = serde_json::from_value(v.clone()).expect("parse");
        serde_json::to_value(&expr).expect("re-encode")
    }

    /// Regression: a condition on a group sub-field looked up the literal key
    /// `seo__title`, which the nested condition data never has, so the field
    /// hid on every server render while the browser (keyed by input name)
    /// showed it.
    #[test]
    fn a_group_sub_field_resolves_by_dotted_and_form_path() {
        let data = json!({ "seo": { "title": "Hi" } });

        for field in ["seo.title", "seo__title"] {
            let expr = parse(json!({ "field": field, "equals": "Hi" }));
            assert!(expr.evaluate(&data), "{field}");
        }

        let missing = parse(json!({ "field": "seo.missing", "is_falsy": true }));
        assert!(missing.evaluate(&data));
    }

    #[test]
    fn equals_evaluates() {
        let expr = parse(json!({ "field": "status", "equals": "published" }));
        assert!(expr.evaluate(&json!({ "status": "published" })));
        assert!(!expr.evaluate(&json!({ "status": "draft" })));
    }

    /// Regression: a whole number is stored as an integer, and a condition
    /// written as `equals = 5.0` arrives as a float; the server compared the
    /// two as different values while the browser saw one number.
    #[test]
    fn numbers_compare_by_value_at_every_depth() {
        let data = json!({ "seats": 5, "sizes": [1, 2] });

        assert!(parse(json!({ "field": "seats", "equals": 5.0 })).evaluate(&data));
        assert!(!parse(json!({ "field": "seats", "not_equals": 5.0 })).evaluate(&data));
        assert!(parse(json!({ "field": "seats", "in": [4.0, 5.0] })).evaluate(&data));
        assert!(!parse(json!({ "field": "seats", "not_in": [5.0] })).evaluate(&data));
        assert!(parse(json!({ "field": "sizes", "equals": [1.0, 2.0] })).evaluate(&data));
        assert!(!parse(json!({ "field": "sizes", "equals": [1.0] })).evaluate(&data));
        assert!(!parse(json!({ "field": "seats", "equals": "5" })).evaluate(&data));
    }

    #[test]
    fn not_equals_evaluates() {
        let expr = parse(json!({ "field": "status", "not_equals": "draft" }));
        assert!(expr.evaluate(&json!({ "status": "published" })));
        assert!(!expr.evaluate(&json!({ "status": "draft" })));
    }

    #[test]
    fn in_evaluates() {
        let expr = parse(json!({ "field": "category", "in": ["tech", "science"] }));
        assert!(expr.evaluate(&json!({ "category": "tech" })));
        assert!(!expr.evaluate(&json!({ "category": "art" })));
    }

    #[test]
    fn not_in_evaluates() {
        let expr = parse(json!({ "field": "category", "not_in": ["art", "music"] }));
        assert!(expr.evaluate(&json!({ "category": "tech" })));
        assert!(!expr.evaluate(&json!({ "category": "art" })));
    }

    #[test]
    fn is_truthy_evaluates() {
        let expr = parse(json!({ "field": "featured", "is_truthy": true }));
        assert!(expr.evaluate(&json!({ "featured": true })));
        assert!(!expr.evaluate(&json!({ "featured": false })));
        assert!(!expr.evaluate(&json!({ "featured": "" })));
        assert!(!expr.evaluate(&json!({ "featured": 0 })));
    }

    /// Regression: the server evaluator called an empty object falsy while the
    /// browser's `isTruthy` calls every object truthy, so a condition on an
    /// untouched group field showed a field in the form and hid it on reload.
    /// An empty array stays falsy on both sides.
    #[test]
    fn is_truthy_matches_the_browser_on_an_empty_object() {
        let expr = parse(json!({ "field": "seo", "is_truthy": true }));

        assert!(expr.evaluate(&json!({ "seo": {} })));
        assert!(expr.evaluate(&json!({ "seo": { "title": "x" } })));
        assert!(!expr.evaluate(&json!({ "seo": [] })));
    }

    #[test]
    fn is_falsy_evaluates() {
        let expr = parse(json!({ "field": "featured", "is_falsy": true }));
        assert!(expr.evaluate(&json!({ "featured": false })));
        assert!(!expr.evaluate(&json!({ "featured": true })));
    }

    #[test]
    fn is_truthy_false_is_noop() {
        // `is_truthy: false` is a no-op in the legacy evaluator (the JS
        // `if (condition.is_truthy)` short-circuit). Keep that behavior.
        let expr = parse(json!({ "field": "featured", "is_truthy": false }));
        assert!(expr.evaluate(&json!({ "featured": false })));
        assert!(expr.evaluate(&json!({ "featured": true })));
    }

    #[test]
    fn array_means_and() {
        let expr = parse(json!([
            { "field": "status", "equals": "published" },
            { "field": "featured", "is_truthy": true },
        ]));
        assert!(expr.evaluate(&json!({ "status": "published", "featured": true })));
        assert!(!expr.evaluate(&json!({ "status": "draft", "featured": true })));
        assert!(!expr.evaluate(&json!({ "status": "published", "featured": false })));
    }

    #[test]
    fn missing_field_is_null() {
        // Field not in form data resolves to null, which won't equal "x".
        let expr = parse(json!({ "field": "missing", "equals": "x" }));
        assert!(!expr.evaluate(&json!({ "other": "y" })));
    }

    #[test]
    fn empty_field_string_defaults_to_show() {
        // Mirrors the legacy "empty field key shows" behavior.
        let expr = parse(json!({ "field": "", "equals": "x" }));
        assert!(expr.evaluate(&json!({})));
    }

    #[test]
    fn missing_field_key_fails_to_parse() {
        // Without a `field` key, the deserializer rejects — caller must
        // handle parse failure explicitly (fail-closed at the seam).
        let parsed: Result<ConditionExpr, _> = serde_json::from_value(json!({ "equals": "x" }));
        assert!(parsed.is_err());
    }

    #[test]
    fn unknown_operator_fails_to_parse() {
        let parsed: Result<ConditionExpr, _> =
            serde_json::from_value(json!({ "field": "x", "banana": "y" }));
        assert!(parsed.is_err());
    }

    #[test]
    fn round_trip_single_row() {
        let v = json!({ "field": "status", "equals": "published" });
        assert_eq!(round_trip(v.clone()), v);
    }

    #[test]
    fn round_trip_array_and() {
        let v = json!([
            { "field": "status", "equals": "published" },
            { "field": "featured", "is_truthy": true },
        ]);
        assert_eq!(round_trip(v.clone()), v);
    }

    #[test]
    fn round_trip_in_op() {
        let v = json!({ "field": "category", "in": ["tech", "science"] });
        assert_eq!(round_trip(v.clone()), v);
    }
}
