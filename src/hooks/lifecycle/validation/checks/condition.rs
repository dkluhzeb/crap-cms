use serde_json::Value;

/// Evaluate a condition table (JSON) against form data.
/// A single condition object has `{ field, equals|not_equals|in|not_in|is_truthy|is_falsy }`.
/// An array of conditions means AND (all must be true).
pub fn evaluate_condition_table(condition: &Value, data: &Value) -> bool {
    match condition {
        Value::Array(arr) => arr.iter().all(|c| evaluate_condition_table(c, data)),
        Value::Object(obj) => {
            let field_name = obj.get("field").and_then(|v| v.as_str()).unwrap_or("");
            let field_val = data.get(field_name).unwrap_or(&Value::Null);

            if let Some(eq) = obj.get("equals") {
                return field_val == eq;
            }
            if let Some(neq) = obj.get("not_equals") {
                return field_val != neq;
            }
            if let Some(Value::Array(list)) = obj.get("in") {
                return list.contains(field_val);
            }
            if let Some(Value::Array(list)) = obj.get("not_in") {
                return !list.contains(field_val);
            }
            if obj
                .get("is_truthy")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                return condition_is_truthy(field_val);
            }
            if obj
                .get("is_falsy")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                return !condition_is_truthy(field_val);
            }
            tracing::warn!(
                "Unknown display condition operator for field '{}' — defaulting to show",
                field_name
            );
            true
        }
        _ => true,
    }
}

/// Check if a JSON value is "truthy" for display condition evaluation.
/// Follows standard truthiness: 0 and 0.0 are falsy, all other numbers are truthy.
pub(crate) fn condition_is_truthy(val: &Value) -> bool {
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
    use super::*;
    use serde_json::json;

    #[test]
    fn test_condition_is_truthy_null() {
        assert!(!condition_is_truthy(&json!(null)));
    }

    #[test]
    fn test_condition_is_truthy_bool() {
        assert!(condition_is_truthy(&json!(true)));
        assert!(!condition_is_truthy(&json!(false)));
    }

    #[test]
    fn test_condition_is_truthy_string() {
        assert!(condition_is_truthy(&json!("hello")));
        assert!(!condition_is_truthy(&json!("")));
    }

    #[test]
    fn test_condition_is_truthy_number() {
        assert!(!condition_is_truthy(&json!(0)));
        assert!(!condition_is_truthy(&json!(0.0)));
        assert!(condition_is_truthy(&json!(42)));
        assert!(condition_is_truthy(&json!(-1)));
        assert!(condition_is_truthy(&json!(0.5)));
    }

    #[test]
    fn test_condition_is_truthy_array() {
        assert!(condition_is_truthy(&json!([1, 2])));
        assert!(!condition_is_truthy(&json!([])));
    }

    #[test]
    fn test_condition_is_truthy_object() {
        assert!(condition_is_truthy(&json!({"key": "value"})));
        assert!(!condition_is_truthy(&json!({})));
    }

    #[test]
    fn test_condition_equals() {
        let data = json!({"status": "published"});
        let cond = json!({"field": "status", "equals": "published"});
        assert!(evaluate_condition_table(&cond, &data));

        let cond_miss = json!({"field": "status", "equals": "draft"});
        assert!(!evaluate_condition_table(&cond_miss, &data));
    }

    #[test]
    fn test_condition_not_equals() {
        let data = json!({"status": "published"});
        let cond = json!({"field": "status", "not_equals": "draft"});
        assert!(evaluate_condition_table(&cond, &data));

        let cond_miss = json!({"field": "status", "not_equals": "published"});
        assert!(!evaluate_condition_table(&cond_miss, &data));
    }

    #[test]
    fn test_condition_in() {
        let data = json!({"category": "tech"});
        let cond = json!({"field": "category", "in": ["tech", "science"]});
        assert!(evaluate_condition_table(&cond, &data));

        let cond_miss = json!({"field": "category", "in": ["art", "music"]});
        assert!(!evaluate_condition_table(&cond_miss, &data));
    }

    #[test]
    fn test_condition_not_in() {
        let data = json!({"category": "tech"});
        let cond = json!({"field": "category", "not_in": ["art", "music"]});
        assert!(evaluate_condition_table(&cond, &data));

        let cond_miss = json!({"field": "category", "not_in": ["tech", "science"]});
        assert!(!evaluate_condition_table(&cond_miss, &data));
    }

    #[test]
    fn test_condition_is_truthy_op() {
        let data = json!({"featured": true});
        let cond = json!({"field": "featured", "is_truthy": true});
        assert!(evaluate_condition_table(&cond, &data));

        let data_false = json!({"featured": false});
        assert!(!evaluate_condition_table(&cond, &data_false));
    }

    #[test]
    fn test_condition_is_falsy_op() {
        let data = json!({"featured": false});
        let cond = json!({"field": "featured", "is_falsy": true});
        assert!(evaluate_condition_table(&cond, &data));

        let data_true = json!({"featured": true});
        assert!(!evaluate_condition_table(&cond, &data_true));
    }

    #[test]
    fn test_condition_array_and() {
        let data = json!({"status": "published", "featured": true});
        let cond = json!([
            {"field": "status", "equals": "published"},
            {"field": "featured", "is_truthy": true}
        ]);
        assert!(evaluate_condition_table(&cond, &data));

        let data_fail = json!({"status": "draft", "featured": true});
        assert!(!evaluate_condition_table(&cond, &data_fail));
    }

    #[test]
    fn test_condition_missing_field() {
        let data = json!({"status": "published"});
        let cond = json!({"field": "nonexistent", "equals": "something"});
        assert!(!evaluate_condition_table(&cond, &data));
    }

    #[test]
    fn test_condition_unknown_operator_shows() {
        let data = json!({"status": "published"});
        let cond = json!({"field": "status"});
        // Unknown operator → show (returns true)
        assert!(evaluate_condition_table(&cond, &data));
    }

    #[test]
    fn test_condition_non_object_non_array_shows() {
        let data = json!({"status": "published"});
        // Non-object, non-array → true
        assert!(evaluate_condition_table(&json!("string"), &data));
        assert!(evaluate_condition_table(&json!(42), &data));
        assert!(evaluate_condition_table(&json!(null), &data));
    }
}
