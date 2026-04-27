mod admin_i18n;
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

use std::{cmp::Ordering, sync::Arc};

use handlebars::Handlebars;
use serde_json::Value;

use crate::admin::Translations;

use self::admin_i18n::AdminI18nHelper;
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
    hbs.register_helper(
        "t",
        Box::new(TranslationHelper {
            translations: translations.clone(),
        }),
    );
    hbs.register_helper("admin_i18n", Box::new(AdminI18nHelper { translations }));
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
pub(super) fn is_truthy(val: &Value) -> bool {
    match val {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(_) => true,
    }
}

/// Try to extract a float from a JSON value.
pub(super) fn as_f64(val: &Value) -> Option<f64> {
    match val {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse::<f64>().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod test_helpers {
    use std::{path::Path, sync::Arc};

    use crate::admin::{templates::create_handlebars, translations::Translations};

    pub fn test_hbs() -> handlebars::Handlebars<'static> {
        let tmp = tempfile::tempdir().expect("tempdir");
        test_hbs_with_translations(tmp.path())
    }

    pub fn test_hbs_with_translations(config_dir: &Path) -> handlebars::Handlebars<'static> {
        let translations = Arc::new(Translations::load(config_dir));
        let hbs = create_handlebars(config_dir, false, translations).expect("create_handlebars");
        (*hbs).clone()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    // --- is_truthy tests ---

    #[test]
    fn is_truthy_edge_cases() {
        assert!(!is_truthy(&Value::Null));
        assert!(!is_truthy(&json!(false)));
        assert!(is_truthy(&json!(true)));
        assert!(!is_truthy(&json!(0)));
        assert!(is_truthy(&json!(1)));
        assert!(is_truthy(&json!(-1)));
        assert!(!is_truthy(&json!("")));
        assert!(is_truthy(&json!("hello")));
        assert!(!is_truthy(&json!([])));
        assert!(is_truthy(&json!([1])));
        assert!(is_truthy(&json!({})));
    }

    #[test]
    fn is_truthy_float_zero() {
        assert!(!is_truthy(&json!(0.0)));
        assert!(is_truthy(&json!(0.1)));
    }

    // --- as_f64 tests ---

    #[test]
    fn as_f64_extracts_numbers() {
        assert_eq!(as_f64(&json!(42)), Some(42.0));
        assert_eq!(as_f64(&json!(3.15)), Some(3.15));
        assert_eq!(as_f64(&json!("2.5")), Some(2.5));
        assert_eq!(as_f64(&json!("not_a_number")), None);
        assert_eq!(as_f64(&json!(null)), None);
        assert_eq!(as_f64(&json!(true)), None);
    }

    #[test]
    fn as_f64_with_array_returns_none() {
        assert_eq!(as_f64(&json!([1, 2, 3])), None);
    }

    #[test]
    fn as_f64_with_object_returns_none() {
        assert_eq!(as_f64(&json!({"a": 1})), None);
    }

    #[test]
    fn as_f64_with_negative_string() {
        assert_eq!(as_f64(&json!("-3.5")), Some(-3.5));
    }

    // --- Composition tests (nested helpers) ---

    fn test_hbs() -> handlebars::Handlebars<'static> {
        let tmp = tempfile::tempdir().expect("tempdir");
        let translations = Arc::new(crate::admin::translations::Translations::load(tmp.path()));
        let hbs = crate::admin::templates::create_handlebars(tmp.path(), false, translations)
            .expect("create_handlebars");
        (*hbs).clone()
    }

    #[test]
    fn nested_helpers_work() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (and (not a) (or b c))}}YES{{else}}NO{{/if}}")
            .unwrap();
        assert_eq!(
            hbs.render("t", &json!({"a": false, "b": true, "c": false}))
                .unwrap(),
            "YES"
        );
        assert_eq!(
            hbs.render("t", &json!({"a": true, "b": true, "c": true}))
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
                &json!({"status": "active", "tags": ["vip", "premium"]})
            )
            .unwrap(),
            "YES"
        );
        assert_eq!(
            hbs.render(
                "t",
                &json!({"status": "inactive", "tags": ["vip", "premium"]})
            )
            .unwrap(),
            "NO"
        );
        assert_eq!(
            hbs.render("t", &json!({"status": "active", "tags": ["basic"]}))
                .unwrap(),
            "NO"
        );
    }

    #[test]
    fn default_helper_with_object_is_truthy() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (not (not val))}}TRUTHY{{else}}FALSY{{/if}}")
            .unwrap();
        assert_eq!(hbs.render("t", &json!({"val": {}})).unwrap(), "TRUTHY");
    }
}
