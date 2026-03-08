mod and;
mod compare;
mod concat;
mod contains;
mod default_val;
mod eq;
mod json;
mod not;
mod or;
mod render_field;
mod translation;

use std::cmp::Ordering;
use std::sync::Arc;

use handlebars::Handlebars;

use crate::admin::translations::Translations;

use self::and::AndHelper;
use self::compare::CompareHelper;
use self::concat::ConcatHelper;
use self::contains::ContainsHelper;
use self::default_val::DefaultHelper;
use self::eq::EqHelper;
use self::json::JsonHelper;
use self::not::NotHelper;
use self::or::OrHelper;
use self::render_field::RenderFieldHelper;
use self::translation::TranslationHelper;

/// Register all Handlebars helpers.
pub(super) fn register_helpers(hbs: &mut Handlebars, translations: Arc<Translations>) {
    hbs.register_helper("render_field", Box::new(RenderFieldHelper));
    hbs.register_helper("eq", Box::new(EqHelper));
    hbs.register_helper("t", Box::new(TranslationHelper { translations }));
    hbs.register_helper("not", Box::new(NotHelper));
    hbs.register_helper("and", Box::new(AndHelper));
    hbs.register_helper("or", Box::new(OrHelper));
    hbs.register_helper("gt", Box::new(CompareHelper(&[Ordering::Greater])));
    hbs.register_helper("lt", Box::new(CompareHelper(&[Ordering::Less])));
    hbs.register_helper(
        "gte",
        Box::new(CompareHelper(&[Ordering::Greater, Ordering::Equal])),
    );
    hbs.register_helper(
        "lte",
        Box::new(CompareHelper(&[Ordering::Less, Ordering::Equal])),
    );
    hbs.register_helper("contains", Box::new(ContainsHelper));
    hbs.register_helper("json", Box::new(JsonHelper));
    hbs.register_helper("default", Box::new(DefaultHelper));
    hbs.register_helper("concat", Box::new(ConcatHelper));
}

/// Check if a JSON value is "truthy" (not null, not false, not empty string, not 0).
pub(super) fn is_truthy(val: &serde_json::Value) -> bool {
    match val {
        serde_json::Value::Null => false,
        serde_json::Value::Bool(b) => *b,
        serde_json::Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        serde_json::Value::String(s) => !s.is_empty(),
        serde_json::Value::Array(a) => !a.is_empty(),
        serde_json::Value::Object(_) => true,
    }
}

/// Try to extract a float from a JSON value.
pub(super) fn as_f64(val: &serde_json::Value) -> Option<f64> {
    match val {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.parse::<f64>().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- is_truthy tests ---

    #[test]
    fn is_truthy_edge_cases() {
        assert!(!is_truthy(&serde_json::Value::Null));
        assert!(!is_truthy(&serde_json::json!(false)));
        assert!(is_truthy(&serde_json::json!(true)));
        assert!(!is_truthy(&serde_json::json!(0)));
        assert!(is_truthy(&serde_json::json!(1)));
        assert!(is_truthy(&serde_json::json!(-1)));
        assert!(!is_truthy(&serde_json::json!("")));
        assert!(is_truthy(&serde_json::json!("hello")));
        assert!(!is_truthy(&serde_json::json!([])));
        assert!(is_truthy(&serde_json::json!([1])));
        assert!(is_truthy(&serde_json::json!({})));
    }

    #[test]
    fn is_truthy_float_zero() {
        assert!(!is_truthy(&serde_json::json!(0.0)));
        assert!(is_truthy(&serde_json::json!(0.1)));
    }

    // --- as_f64 tests ---

    #[test]
    fn as_f64_extracts_numbers() {
        assert_eq!(as_f64(&serde_json::json!(42)), Some(42.0));
        assert_eq!(as_f64(&serde_json::json!(3.14)), Some(3.14));
        assert_eq!(as_f64(&serde_json::json!("2.5")), Some(2.5));
        assert_eq!(as_f64(&serde_json::json!("not_a_number")), None);
        assert_eq!(as_f64(&serde_json::json!(null)), None);
        assert_eq!(as_f64(&serde_json::json!(true)), None);
    }

    #[test]
    fn as_f64_with_array_returns_none() {
        assert_eq!(as_f64(&serde_json::json!([1, 2, 3])), None);
    }

    #[test]
    fn as_f64_with_object_returns_none() {
        assert_eq!(as_f64(&serde_json::json!({"a": 1})), None);
    }

    #[test]
    fn as_f64_with_negative_string() {
        assert_eq!(as_f64(&serde_json::json!("-3.5")), Some(-3.5));
    }

    // --- Composition tests (nested helpers) ---

    fn test_hbs() -> handlebars::Handlebars<'static> {
        let tmp = tempfile::tempdir().expect("tempdir");
        let translations =
            Arc::new(crate::admin::translations::Translations::load(tmp.path()));
        let hbs = crate::admin::templates::create_handlebars(tmp.path(), false, translations)
            .expect("create_handlebars");
        (*hbs).clone()
    }

    #[test]
    fn nested_helpers_work() {
        let mut hbs = test_hbs();
        hbs.register_template_string(
            "t",
            "{{#if (and (not a) (or b c))}}YES{{else}}NO{{/if}}",
        )
        .unwrap();
        assert_eq!(
            hbs.render(
                "t",
                &serde_json::json!({"a": false, "b": true, "c": false})
            )
            .unwrap(),
            "YES"
        );
        assert_eq!(
            hbs.render(
                "t",
                &serde_json::json!({"a": true, "b": true, "c": true})
            )
            .unwrap(),
            "NO"
        );
    }

    #[test]
    fn eq_with_contains_composition() {
        let mut hbs = test_hbs();
        hbs.register_template_string(
            "t",
            "{{#if (and (eq status \"active\") (contains tags \"vip\"))}}YES{{else}}NO{{/if}}",
        )
        .unwrap();
        assert_eq!(
            hbs.render(
                "t",
                &serde_json::json!({"status": "active", "tags": ["vip", "premium"]})
            )
            .unwrap(),
            "YES"
        );
        assert_eq!(
            hbs.render(
                "t",
                &serde_json::json!({"status": "inactive", "tags": ["vip", "premium"]})
            )
            .unwrap(),
            "NO"
        );
        assert_eq!(
            hbs.render(
                "t",
                &serde_json::json!({"status": "active", "tags": ["basic"]})
            )
            .unwrap(),
            "NO"
        );
    }

    #[test]
    fn default_helper_with_object_is_truthy() {
        let mut hbs = test_hbs();
        hbs.register_template_string(
            "t",
            "{{#if (not (not val))}}TRUTHY{{else}}FALSY{{/if}}",
        )
        .unwrap();
        assert_eq!(
            hbs.render("t", &serde_json::json!({"val": {}})).unwrap(),
            "TRUTHY"
        );
    }
}
