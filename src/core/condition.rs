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

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

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

        let field_val = data.get(&self.field).unwrap_or(&Value::Null);

        match &self.op {
            ConditionOp::Equals(v) => field_val == v,
            ConditionOp::NotEquals(v) => field_val != v,
            ConditionOp::In(list) => list.contains(field_val),
            ConditionOp::NotIn(list) => !list.contains(field_val),
            ConditionOp::IsTruthy(true) => is_truthy(field_val),
            ConditionOp::IsFalsy(true) => !is_truthy(field_val),
            ConditionOp::IsTruthy(false) | ConditionOp::IsFalsy(false) => true,
        }
    }
}

/// Field-presence truthiness, matching the JS evaluator's `isTruthy`.
/// `0`, `0.0`, `""`, `null`, `[]`, `{}` are falsy; everything else is truthy.
fn is_truthy(val: &Value) -> bool {
    match val {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty(),
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

#[cfg(test)]
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

    #[test]
    fn equals_evaluates() {
        let expr = parse(json!({ "field": "status", "equals": "published" }));
        assert!(expr.evaluate(&json!({ "status": "published" })));
        assert!(!expr.evaluate(&json!({ "status": "draft" })));
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
