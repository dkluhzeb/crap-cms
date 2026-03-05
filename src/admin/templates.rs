//! Handlebars template loading with overlay (config dir overrides compiled defaults).

use anyhow::{Context, Result};
use handlebars::{Handlebars, RenderError, RenderContext, Helper, HelperDef, ScopedJson};
use include_dir::{include_dir, Dir};
use std::cmp::Ordering;
use std::path::Path;
use std::sync::Arc;

use super::translations::Translations;

static TEMPLATES_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/templates");

/// Create a Handlebars instance with embedded defaults, config overlays, and helpers.
pub fn create_handlebars(
    config_dir: &Path,
    dev_mode: bool,
    translations: Arc<Translations>,
) -> Result<Arc<Handlebars<'static>>> {
    let mut hbs = Handlebars::new();
    hbs.set_dev_mode(dev_mode);
    hbs.set_strict_mode(false);

    // Register embedded templates (compiled defaults)
    register_embedded_templates(&mut hbs)?;

    // Overlay with config directory templates (if present)
    let templates_dir = config_dir.join("templates");
    if templates_dir.exists() {
        register_dir_templates(&mut hbs, &templates_dir)?;
    }

    // Register helpers
    hbs.register_helper("render_field", Box::new(RenderFieldHelper));
    hbs.register_helper("eq", Box::new(EqHelper));
    hbs.register_helper("t", Box::new(TranslationHelper { translations }));
    hbs.register_helper("not", Box::new(NotHelper));
    hbs.register_helper("and", Box::new(AndHelper));
    hbs.register_helper("or", Box::new(OrHelper));
    hbs.register_helper("gt", Box::new(CompareHelper(Ordering::Greater)));
    hbs.register_helper("lt", Box::new(CompareHelper(Ordering::Less)));
    hbs.register_helper("gte", Box::new(CompareHelper2(Ordering::Greater, Ordering::Equal)));
    hbs.register_helper("lte", Box::new(CompareHelper2(Ordering::Less, Ordering::Equal)));
    hbs.register_helper("contains", Box::new(ContainsHelper));
    hbs.register_helper("json", Box::new(JsonHelper));
    hbs.register_helper("default", Box::new(DefaultHelper));
    hbs.register_helper("concat", Box::new(ConcatHelper));

    Ok(Arc::new(hbs))
}

fn register_embedded_templates(hbs: &mut Handlebars) -> Result<()> {
    register_embedded_dir(hbs, &TEMPLATES_DIR)?;
    Ok(())
}

fn register_embedded_dir(hbs: &mut Handlebars, dir: &Dir) -> Result<()> {
    for file in dir.files() {
        let path = file.path();
        if path.extension().is_some_and(|ext| ext == "hbs") {
            // file.path() already returns the full relative path (e.g. "dashboard/index.hbs")
            let name_str = path.with_extension("").to_string_lossy().to_string();
            let content = std::str::from_utf8(file.contents())
                .with_context(|| format!("Invalid UTF-8 in template: {}", name_str))?;
            hbs.register_template_string(&name_str, content)
                .with_context(|| format!("Failed to register template: {}", name_str))?;
        }
    }

    for subdir in dir.dirs() {
        register_embedded_dir(hbs, subdir)?;
    }

    Ok(())
}

fn register_dir_templates(hbs: &mut Handlebars, dir: &Path) -> Result<()> {
    register_dir_recursive(hbs, dir, dir)?;
    Ok(())
}

fn register_dir_recursive(hbs: &mut Handlebars, base: &Path, dir: &Path) -> Result<()> {
    let entries = std::fs::read_dir(dir)
        .with_context(|| format!("Failed to read directory: {}", dir.display()))?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            register_dir_recursive(hbs, base, &path)?;
        } else if path.extension().is_some_and(|ext| ext == "hbs") {
            let relative = match path.strip_prefix(base) {
                Ok(r) => r,
                Err(_) => continue,
            };
            let name = relative.with_extension("").to_string_lossy().to_string();
            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("Failed to read template: {}", path.display()))?;
            tracing::debug!("Overlay template: {}", name);
            hbs.register_template_string(&name, &content)
                .with_context(|| format!("Failed to register overlay template: {}", name))?;
        }
    }

    Ok(())
}

/// Handlebars helper that renders the appropriate field partial.
/// Usage: {{render_field field_context}}
struct RenderFieldHelper;

impl HelperDef for RenderFieldHelper {
    fn call_inner<'reg: 'rc, 'rc>(
        &self,
        h: &Helper<'rc>,
        r: &'reg Handlebars<'reg>,
        _ctx: &'rc handlebars::Context,
        _rc: &mut RenderContext<'reg, 'rc>,
    ) -> Result<ScopedJson<'rc>, RenderError> {
        let param = h.param(0)
            .ok_or_else(|| RenderError::from(handlebars::RenderErrorReason::ParamNotFoundForIndex("render_field", 0)))?;

        let field_data = param.value();
        let field_type = field_data.get("field_type")
            .and_then(|v| v.as_str())
            .unwrap_or("text");

        let template_name = format!("fields/{}", field_type);

        let rendered = r.render(&template_name, field_data)
            .unwrap_or_else(|e| {
                tracing::warn!("Failed to render template '{}': {}, falling back to fields/text", template_name, e);
                r.render("fields/text", field_data)
                    .unwrap_or_default()
            });

        Ok(ScopedJson::Derived(serde_json::Value::String(rendered)))
    }
}

/// Handlebars helper for equality comparison.
struct EqHelper;

impl HelperDef for EqHelper {
    fn call_inner<'reg: 'rc, 'rc>(
        &self,
        h: &Helper<'rc>,
        _r: &'reg Handlebars<'reg>,
        _ctx: &'rc handlebars::Context,
        _rc: &mut RenderContext<'reg, 'rc>,
    ) -> Result<ScopedJson<'rc>, RenderError> {
        let a = h.param(0).map(|p| p.value());
        let b = h.param(1).map(|p| p.value());
        let result = a == b;
        Ok(ScopedJson::Derived(serde_json::Value::Bool(result)))
    }
}

/// Handlebars helper for admin UI translations.
/// Usage: `{{t "key"}}` or with interpolation: `{{t "key" name=value}}`
/// Interpolation replaces `{{var}}` placeholders in the translation string.
#[allow(dead_code)]
struct TranslationHelper {
    translations: Arc<Translations>,
}

impl HelperDef for TranslationHelper {
    fn call_inner<'reg: 'rc, 'rc>(
        &self,
        h: &Helper<'rc>,
        _r: &'reg Handlebars<'reg>,
        ctx: &'rc handlebars::Context,
        _rc: &mut RenderContext<'reg, 'rc>,
    ) -> Result<ScopedJson<'rc>, RenderError> {
        let key = h.param(0)
            .and_then(|p| p.value().as_str())
            .unwrap_or("");

        // Read locale from template context (_locale), default to "en"
        let locale = ctx.data()
            .get("_locale")
            .and_then(|v| v.as_str())
            .unwrap_or("en");

        let hash = h.hash();
        if hash.is_empty() {
            let translated = self.translations.get(locale, key);
            Ok(ScopedJson::Derived(serde_json::Value::String(translated.to_string())))
        } else {
            let mut params = std::collections::HashMap::new();
            for (k, v) in hash {
                let val = match v.value() {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Number(n) => n.to_string(),
                    serde_json::Value::Bool(b) => b.to_string(),
                    other => other.to_string(),
                };
                params.insert(k.to_string(), val);
            }
            let translated = self.translations.get_interpolated(locale, key, &params);
            Ok(ScopedJson::Derived(serde_json::Value::String(translated)))
        }
    }
}

/// Boolean negation helper: `{{#if (not val)}}`.
struct NotHelper;

impl HelperDef for NotHelper {
    fn call_inner<'reg: 'rc, 'rc>(
        &self,
        h: &Helper<'rc>,
        _r: &'reg Handlebars<'reg>,
        _ctx: &'rc handlebars::Context,
        _rc: &mut RenderContext<'reg, 'rc>,
    ) -> Result<ScopedJson<'rc>, RenderError> {
        let val = h.param(0).map(|p| p.value()).unwrap_or(&serde_json::Value::Null);
        let truthy = is_truthy(val);
        Ok(ScopedJson::Derived(serde_json::Value::Bool(!truthy)))
    }
}

/// Logical AND helper: `{{#if (and a b)}}`.
struct AndHelper;

impl HelperDef for AndHelper {
    fn call_inner<'reg: 'rc, 'rc>(
        &self,
        h: &Helper<'rc>,
        _r: &'reg Handlebars<'reg>,
        _ctx: &'rc handlebars::Context,
        _rc: &mut RenderContext<'reg, 'rc>,
    ) -> Result<ScopedJson<'rc>, RenderError> {
        let a = h.param(0).map(|p| p.value()).unwrap_or(&serde_json::Value::Null);
        let b = h.param(1).map(|p| p.value()).unwrap_or(&serde_json::Value::Null);
        Ok(ScopedJson::Derived(serde_json::Value::Bool(is_truthy(a) && is_truthy(b))))
    }
}

/// Logical OR helper: `{{#if (or a b)}}`.
struct OrHelper;

impl HelperDef for OrHelper {
    fn call_inner<'reg: 'rc, 'rc>(
        &self,
        h: &Helper<'rc>,
        _r: &'reg Handlebars<'reg>,
        _ctx: &'rc handlebars::Context,
        _rc: &mut RenderContext<'reg, 'rc>,
    ) -> Result<ScopedJson<'rc>, RenderError> {
        let a = h.param(0).map(|p| p.value()).unwrap_or(&serde_json::Value::Null);
        let b = h.param(1).map(|p| p.value()).unwrap_or(&serde_json::Value::Null);
        Ok(ScopedJson::Derived(serde_json::Value::Bool(is_truthy(a) || is_truthy(b))))
    }
}

/// Numeric comparison helper (single ordering, for gt/lt).
struct CompareHelper(Ordering);

impl HelperDef for CompareHelper {
    fn call_inner<'reg: 'rc, 'rc>(
        &self,
        h: &Helper<'rc>,
        _r: &'reg Handlebars<'reg>,
        _ctx: &'rc handlebars::Context,
        _rc: &mut RenderContext<'reg, 'rc>,
    ) -> Result<ScopedJson<'rc>, RenderError> {
        let a = h.param(0).and_then(|p| as_f64(p.value())).unwrap_or(0.0);
        let b = h.param(1).and_then(|p| as_f64(p.value())).unwrap_or(0.0);
        let result = a.partial_cmp(&b).map(|o| o == self.0).unwrap_or(false);
        Ok(ScopedJson::Derived(serde_json::Value::Bool(result)))
    }
}

/// Numeric comparison helper (two orderings, for gte/lte).
struct CompareHelper2(Ordering, Ordering);

impl HelperDef for CompareHelper2 {
    fn call_inner<'reg: 'rc, 'rc>(
        &self,
        h: &Helper<'rc>,
        _r: &'reg Handlebars<'reg>,
        _ctx: &'rc handlebars::Context,
        _rc: &mut RenderContext<'reg, 'rc>,
    ) -> Result<ScopedJson<'rc>, RenderError> {
        let a = h.param(0).and_then(|p| as_f64(p.value())).unwrap_or(0.0);
        let b = h.param(1).and_then(|p| as_f64(p.value())).unwrap_or(0.0);
        let result = a.partial_cmp(&b).map(|o| o == self.0 || o == self.1).unwrap_or(false);
        Ok(ScopedJson::Derived(serde_json::Value::Bool(result)))
    }
}

/// String/array contains helper: `{{#if (contains arr val)}}`.
struct ContainsHelper;

impl HelperDef for ContainsHelper {
    fn call_inner<'reg: 'rc, 'rc>(
        &self,
        h: &Helper<'rc>,
        _r: &'reg Handlebars<'reg>,
        _ctx: &'rc handlebars::Context,
        _rc: &mut RenderContext<'reg, 'rc>,
    ) -> Result<ScopedJson<'rc>, RenderError> {
        let haystack = h.param(0).map(|p| p.value()).unwrap_or(&serde_json::Value::Null);
        let needle = h.param(1).map(|p| p.value()).unwrap_or(&serde_json::Value::Null);
        let result = match haystack {
            serde_json::Value::String(s) => {
                needle.as_str().map(|n| s.contains(n)).unwrap_or(false)
            }
            serde_json::Value::Array(arr) => arr.contains(needle),
            _ => false,
        };
        Ok(ScopedJson::Derived(serde_json::Value::Bool(result)))
    }
}

/// JSON serialization helper: `{{{json value}}}`.
struct JsonHelper;

impl HelperDef for JsonHelper {
    fn call_inner<'reg: 'rc, 'rc>(
        &self,
        h: &Helper<'rc>,
        _r: &'reg Handlebars<'reg>,
        _ctx: &'rc handlebars::Context,
        _rc: &mut RenderContext<'reg, 'rc>,
    ) -> Result<ScopedJson<'rc>, RenderError> {
        let val = h.param(0).map(|p| p.value()).unwrap_or(&serde_json::Value::Null);
        let json_str = serde_json::to_string(val).unwrap_or_default();
        Ok(ScopedJson::Derived(serde_json::Value::String(json_str)))
    }
}

/// Default value helper: `{{default val fallback}}`.
struct DefaultHelper;

impl HelperDef for DefaultHelper {
    fn call_inner<'reg: 'rc, 'rc>(
        &self,
        h: &Helper<'rc>,
        _r: &'reg Handlebars<'reg>,
        _ctx: &'rc handlebars::Context,
        _rc: &mut RenderContext<'reg, 'rc>,
    ) -> Result<ScopedJson<'rc>, RenderError> {
        let val = h.param(0).map(|p| p.value()).unwrap_or(&serde_json::Value::Null);
        let fallback = h.param(1).map(|p| p.value()).unwrap_or(&serde_json::Value::Null);
        if is_truthy(val) {
            Ok(ScopedJson::Derived(val.clone()))
        } else {
            Ok(ScopedJson::Derived(fallback.clone()))
        }
    }
}

/// String concatenation helper: `{{concat a b c}}`.
struct ConcatHelper;

impl HelperDef for ConcatHelper {
    fn call_inner<'reg: 'rc, 'rc>(
        &self,
        h: &Helper<'rc>,
        _r: &'reg Handlebars<'reg>,
        _ctx: &'rc handlebars::Context,
        _rc: &mut RenderContext<'reg, 'rc>,
    ) -> Result<ScopedJson<'rc>, RenderError> {
        let mut result = String::new();
        for i in 0.. {
            match h.param(i) {
                Some(p) => match p.value() {
                    serde_json::Value::String(s) => result.push_str(s),
                    serde_json::Value::Number(n) => result.push_str(&n.to_string()),
                    serde_json::Value::Bool(b) => result.push_str(&b.to_string()),
                    serde_json::Value::Null => {}
                    other => result.push_str(&other.to_string()),
                },
                None => break,
            }
        }
        Ok(ScopedJson::Derived(serde_json::Value::String(result)))
    }
}

/// Check if a JSON value is "truthy" (not null, not false, not empty string, not 0).
fn is_truthy(val: &serde_json::Value) -> bool {
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
fn as_f64(val: &serde_json::Value) -> Option<f64> {
    match val {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.parse::<f64>().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_handlebars_loads_templates() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let translations = Arc::new(Translations::load(tmp.path()));
        let hbs = create_handlebars(tmp.path(), false, translations).expect("create_handlebars");
        // Should have at least the compiled-in templates (dashboard/index, auth/login, etc.)
        // Rendering dashboard/index with minimal data should work (non-strict mode)
        let result = hbs.render("auth/login", &serde_json::json!({
            "title": "Login",
            "collections": [],
        }));
        assert!(result.is_ok(), "Should render auth/login template: {:?}", result.err());
    }

    #[test]
    fn eq_helper_works() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let translations = Arc::new(Translations::load(tmp.path()));
        let hbs = create_handlebars(tmp.path(), false, translations).expect("create_handlebars");
        let mut hbs_mut = (*hbs).clone();
        hbs_mut.register_template_string("test_eq", "{{#if (eq a b)}}EQUAL{{else}}NOT_EQUAL{{/if}}")
            .expect("register");
        let result = hbs_mut.render("test_eq", &serde_json::json!({"a": "foo", "b": "foo"})).unwrap();
        assert_eq!(result, "EQUAL");
        let result = hbs_mut.render("test_eq", &serde_json::json!({"a": "foo", "b": "bar"})).unwrap();
        assert_eq!(result, "NOT_EQUAL");
    }

    /// Helper to create a Handlebars instance with all helpers registered for testing.
    fn test_hbs() -> Handlebars<'static> {
        let tmp = tempfile::tempdir().expect("tempdir");
        let translations = Arc::new(Translations::load(tmp.path()));
        let hbs = create_handlebars(tmp.path(), false, translations).expect("create_handlebars");
        (*hbs).clone()
    }

    #[test]
    fn not_helper() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (not val)}}YES{{else}}NO{{/if}}").unwrap();
        assert_eq!(hbs.render("t", &serde_json::json!({"val": false})).unwrap(), "YES");
        assert_eq!(hbs.render("t", &serde_json::json!({"val": true})).unwrap(), "NO");
        assert_eq!(hbs.render("t", &serde_json::json!({"val": null})).unwrap(), "YES");
        assert_eq!(hbs.render("t", &serde_json::json!({"val": "hello"})).unwrap(), "NO");
        assert_eq!(hbs.render("t", &serde_json::json!({"val": ""})).unwrap(), "YES");
    }

    #[test]
    fn and_helper() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (and a b)}}YES{{else}}NO{{/if}}").unwrap();
        assert_eq!(hbs.render("t", &serde_json::json!({"a": true, "b": true})).unwrap(), "YES");
        assert_eq!(hbs.render("t", &serde_json::json!({"a": true, "b": false})).unwrap(), "NO");
        assert_eq!(hbs.render("t", &serde_json::json!({"a": false, "b": true})).unwrap(), "NO");
    }

    #[test]
    fn or_helper() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (or a b)}}YES{{else}}NO{{/if}}").unwrap();
        assert_eq!(hbs.render("t", &serde_json::json!({"a": true, "b": false})).unwrap(), "YES");
        assert_eq!(hbs.render("t", &serde_json::json!({"a": false, "b": true})).unwrap(), "YES");
        assert_eq!(hbs.render("t", &serde_json::json!({"a": false, "b": false})).unwrap(), "NO");
    }

    #[test]
    fn gt_helper() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (gt a b)}}YES{{else}}NO{{/if}}").unwrap();
        assert_eq!(hbs.render("t", &serde_json::json!({"a": 10, "b": 5})).unwrap(), "YES");
        assert_eq!(hbs.render("t", &serde_json::json!({"a": 5, "b": 10})).unwrap(), "NO");
        assert_eq!(hbs.render("t", &serde_json::json!({"a": 5, "b": 5})).unwrap(), "NO");
    }

    #[test]
    fn lt_helper() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (lt a b)}}YES{{else}}NO{{/if}}").unwrap();
        assert_eq!(hbs.render("t", &serde_json::json!({"a": 3, "b": 7})).unwrap(), "YES");
        assert_eq!(hbs.render("t", &serde_json::json!({"a": 7, "b": 3})).unwrap(), "NO");
    }

    #[test]
    fn gte_helper() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (gte a b)}}YES{{else}}NO{{/if}}").unwrap();
        assert_eq!(hbs.render("t", &serde_json::json!({"a": 5, "b": 5})).unwrap(), "YES");
        assert_eq!(hbs.render("t", &serde_json::json!({"a": 6, "b": 5})).unwrap(), "YES");
        assert_eq!(hbs.render("t", &serde_json::json!({"a": 4, "b": 5})).unwrap(), "NO");
    }

    #[test]
    fn lte_helper() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (lte a b)}}YES{{else}}NO{{/if}}").unwrap();
        assert_eq!(hbs.render("t", &serde_json::json!({"a": 5, "b": 5})).unwrap(), "YES");
        assert_eq!(hbs.render("t", &serde_json::json!({"a": 4, "b": 5})).unwrap(), "YES");
        assert_eq!(hbs.render("t", &serde_json::json!({"a": 6, "b": 5})).unwrap(), "NO");
    }

    #[test]
    fn contains_helper_string() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (contains haystack needle)}}YES{{else}}NO{{/if}}").unwrap();
        assert_eq!(hbs.render("t", &serde_json::json!({"haystack": "hello world", "needle": "world"})).unwrap(), "YES");
        assert_eq!(hbs.render("t", &serde_json::json!({"haystack": "hello world", "needle": "xyz"})).unwrap(), "NO");
    }

    #[test]
    fn contains_helper_array() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (contains arr val)}}YES{{else}}NO{{/if}}").unwrap();
        assert_eq!(hbs.render("t", &serde_json::json!({"arr": ["a", "b", "c"], "val": "b"})).unwrap(), "YES");
        assert_eq!(hbs.render("t", &serde_json::json!({"arr": ["a", "b", "c"], "val": "d"})).unwrap(), "NO");
    }

    #[test]
    fn json_helper() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{{json val}}}").unwrap();
        let result = hbs.render("t", &serde_json::json!({"val": {"key": "value"}})).unwrap();
        assert!(result.contains("\"key\""));
        assert!(result.contains("\"value\""));
    }

    #[test]
    fn default_helper() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{default val fallback}}").unwrap();
        assert_eq!(hbs.render("t", &serde_json::json!({"val": "hello", "fallback": "bye"})).unwrap(), "hello");
        assert_eq!(hbs.render("t", &serde_json::json!({"val": null, "fallback": "bye"})).unwrap(), "bye");
        assert_eq!(hbs.render("t", &serde_json::json!({"val": "", "fallback": "bye"})).unwrap(), "bye");
    }

    #[test]
    fn concat_helper() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{concat a b c}}").unwrap();
        assert_eq!(hbs.render("t", &serde_json::json!({"a": "hello", "b": " ", "c": "world"})).unwrap(), "hello world");
    }

    #[test]
    fn concat_helper_numbers() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{concat a b}}").unwrap();
        assert_eq!(hbs.render("t", &serde_json::json!({"a": "count:", "b": 42})).unwrap(), "count:42");
    }

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
    fn as_f64_extracts_numbers() {
        assert_eq!(as_f64(&serde_json::json!(42)), Some(42.0));
        assert_eq!(as_f64(&serde_json::json!(3.14)), Some(3.14));
        assert_eq!(as_f64(&serde_json::json!("2.5")), Some(2.5));
        assert_eq!(as_f64(&serde_json::json!("not_a_number")), None);
        assert_eq!(as_f64(&serde_json::json!(null)), None);
        assert_eq!(as_f64(&serde_json::json!(true)), None);
    }

    // --- Template overlay loading ---

    #[test]
    fn overlay_templates_override_compiled_defaults() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let templates_dir = tmp.path().join("templates").join("auth");
        std::fs::create_dir_all(&templates_dir).unwrap();
        // Override auth/login with custom content
        std::fs::write(templates_dir.join("login.hbs"), "CUSTOM_LOGIN_PAGE").unwrap();

        let translations = Arc::new(Translations::load(tmp.path()));
        let hbs = create_handlebars(tmp.path(), false, translations).expect("create_handlebars");
        let result = hbs.render("auth/login", &serde_json::json!({})).unwrap();
        assert_eq!(result, "CUSTOM_LOGIN_PAGE");
    }

    #[test]
    fn overlay_templates_nested_subdirectory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let nested_dir = tmp.path().join("templates").join("custom").join("deep");
        std::fs::create_dir_all(&nested_dir).unwrap();
        std::fs::write(nested_dir.join("page.hbs"), "DEEP_NESTED").unwrap();

        let translations = Arc::new(Translations::load(tmp.path()));
        let hbs = create_handlebars(tmp.path(), false, translations).expect("create_handlebars");
        let result = hbs.render("custom/deep/page", &serde_json::json!({})).unwrap();
        assert_eq!(result, "DEEP_NESTED");
    }

    #[test]
    fn non_hbs_files_are_ignored_in_overlay() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let templates_dir = tmp.path().join("templates");
        std::fs::create_dir_all(&templates_dir).unwrap();
        // A .txt file should not be registered
        std::fs::write(templates_dir.join("notes.txt"), "not a template").unwrap();
        // A .hbs file should be registered
        std::fs::write(templates_dir.join("custom.hbs"), "IS_TEMPLATE").unwrap();

        let translations = Arc::new(Translations::load(tmp.path()));
        let hbs = create_handlebars(tmp.path(), false, translations).expect("create_handlebars");
        assert!(hbs.render("custom", &serde_json::json!({})).is_ok());
        // "notes" should not be registered as a template
        assert!(hbs.render("notes", &serde_json::json!({})).is_err());
    }

    #[test]
    fn dev_mode_enables_dev_mode() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let translations = Arc::new(Translations::load(tmp.path()));
        let hbs = create_handlebars(tmp.path(), true, translations).expect("create_handlebars");
        assert!(hbs.dev_mode());
    }

    #[test]
    fn non_dev_mode_disables_dev_mode() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let translations = Arc::new(Translations::load(tmp.path()));
        let hbs = create_handlebars(tmp.path(), false, translations).expect("create_handlebars");
        assert!(!hbs.dev_mode());
    }

    // --- render_field helper ---

    #[test]
    fn render_field_helper_renders_text_field() {
        let mut hbs = test_hbs();
        // Register a minimal text field template
        hbs.register_template_string("fields/text", "TEXT:{{name}}").unwrap();
        hbs.register_template_string("t", "{{{render_field ctx}}}").unwrap();
        let result = hbs.render("t", &serde_json::json!({
            "ctx": {"field_type": "text", "name": "title"}
        })).unwrap();
        assert_eq!(result, "TEXT:title");
    }

    #[test]
    fn render_field_helper_fallback_to_text_on_unknown_type() {
        let mut hbs = test_hbs();
        hbs.register_template_string("fields/text", "FALLBACK:{{name}}").unwrap();
        hbs.register_template_string("t", "{{{render_field ctx}}}").unwrap();
        // "unknown_type" has no template registered, should fall back to fields/text
        let result = hbs.render("t", &serde_json::json!({
            "ctx": {"field_type": "unknown_type", "name": "my_field"}
        })).unwrap();
        assert_eq!(result, "FALLBACK:my_field");
    }

    #[test]
    fn render_field_helper_default_type_is_text() {
        let mut hbs = test_hbs();
        hbs.register_template_string("fields/text", "DEFAULT_TEXT:{{name}}").unwrap();
        hbs.register_template_string("t", "{{{render_field ctx}}}").unwrap();
        // No field_type in context — should default to "text"
        let result = hbs.render("t", &serde_json::json!({
            "ctx": {"name": "untitled"}
        })).unwrap();
        assert_eq!(result, "DEFAULT_TEXT:untitled");
    }

    #[test]
    fn render_field_helper_renders_select_field() {
        let mut hbs = test_hbs();
        hbs.register_template_string("fields/select", "SELECT:{{name}}").unwrap();
        hbs.register_template_string("t", "{{{render_field ctx}}}").unwrap();
        let result = hbs.render("t", &serde_json::json!({
            "ctx": {"field_type": "select", "name": "status"}
        })).unwrap();
        assert_eq!(result, "SELECT:status");
    }

    #[test]
    fn render_field_helper_renders_checkbox_field() {
        let mut hbs = test_hbs();
        hbs.register_template_string("fields/checkbox", "CHECKBOX:{{name}}").unwrap();
        hbs.register_template_string("t", "{{{render_field ctx}}}").unwrap();
        let result = hbs.render("t", &serde_json::json!({
            "ctx": {"field_type": "checkbox", "name": "active"}
        })).unwrap();
        assert_eq!(result, "CHECKBOX:active");
    }

    // --- Translation helper ---

    #[test]
    fn translation_helper_simple_key() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let translations_dir = tmp.path().join("translations");
        std::fs::create_dir_all(&translations_dir).unwrap();
        std::fs::write(
            translations_dir.join("en.json"),
            r#"{"hello": "Hello World"}"#,
        ).unwrap();

        let translations = Arc::new(Translations::load(tmp.path()));
        let hbs = create_handlebars(tmp.path(), false, translations).expect("hbs");
        let mut hbs = (*hbs).clone();
        hbs.register_template_string("t", "{{t \"hello\"}}").unwrap();
        let result = hbs.render("t", &serde_json::json!({})).unwrap();
        assert_eq!(result, "Hello World");
    }

    #[test]
    fn translation_helper_with_interpolation() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let translations_dir = tmp.path().join("translations");
        std::fs::create_dir_all(&translations_dir).unwrap();
        std::fs::write(
            translations_dir.join("en.json"),
            r#"{"greeting": "Hello {{name}}, you have {{count}} items"}"#,
        ).unwrap();

        let translations = Arc::new(Translations::load(tmp.path()));
        let hbs = create_handlebars(tmp.path(), false, translations).expect("hbs");
        let mut hbs = (*hbs).clone();
        hbs.register_template_string("t", "{{t \"greeting\" name=\"Alice\" count=5}}").unwrap();
        let result = hbs.render("t", &serde_json::json!({})).unwrap();
        assert_eq!(result, "Hello Alice, you have 5 items");
    }

    #[test]
    fn translation_helper_interpolation_with_bool() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let translations_dir = tmp.path().join("translations");
        std::fs::create_dir_all(&translations_dir).unwrap();
        std::fs::write(
            translations_dir.join("en.json"),
            r#"{"status": "Active: {{active}}"}"#,
        ).unwrap();

        let translations = Arc::new(Translations::load(tmp.path()));
        let hbs = create_handlebars(tmp.path(), false, translations).expect("hbs");
        let mut hbs = (*hbs).clone();
        hbs.register_template_string("t", "{{t \"status\" active=true}}").unwrap();
        let result = hbs.render("t", &serde_json::json!({})).unwrap();
        assert_eq!(result, "Active: true");
    }

    #[test]
    fn translation_helper_missing_key_returns_key() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let translations = Arc::new(Translations::load(tmp.path()));
        let hbs = create_handlebars(tmp.path(), false, translations).expect("hbs");
        let mut hbs = (*hbs).clone();
        hbs.register_template_string("t", "{{t \"nonexistent.key\"}}").unwrap();
        let result = hbs.render("t", &serde_json::json!({})).unwrap();
        // Missing keys should return the key itself as fallback
        assert_eq!(result, "nonexistent.key");
    }

    // --- contains helper edge cases ---

    #[test]
    fn contains_helper_non_string_non_array_returns_false() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (contains haystack needle)}}YES{{else}}NO{{/if}}").unwrap();
        // Number haystack
        assert_eq!(hbs.render("t", &serde_json::json!({"haystack": 42, "needle": "4"})).unwrap(), "NO");
        // Bool haystack
        assert_eq!(hbs.render("t", &serde_json::json!({"haystack": true, "needle": "t"})).unwrap(), "NO");
        // Null haystack
        assert_eq!(hbs.render("t", &serde_json::json!({"haystack": null, "needle": "x"})).unwrap(), "NO");
        // Object haystack
        assert_eq!(hbs.render("t", &serde_json::json!({"haystack": {"a": 1}, "needle": "a"})).unwrap(), "NO");
    }

    #[test]
    fn contains_helper_string_with_non_string_needle() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (contains haystack needle)}}YES{{else}}NO{{/if}}").unwrap();
        // Needle is a number — needle.as_str() returns None, so should be false
        assert_eq!(hbs.render("t", &serde_json::json!({"haystack": "hello 42 world", "needle": 42})).unwrap(), "NO");
    }

    #[test]
    fn contains_helper_array_with_number_needle() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (contains arr val)}}YES{{else}}NO{{/if}}").unwrap();
        // Array contains checks with JSON value equality
        assert_eq!(hbs.render("t", &serde_json::json!({"arr": [1, 2, 3], "val": 2})).unwrap(), "YES");
        assert_eq!(hbs.render("t", &serde_json::json!({"arr": [1, 2, 3], "val": 4})).unwrap(), "NO");
    }

    // --- concat helper edge cases ---

    #[test]
    fn concat_helper_with_bools() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{concat a b}}").unwrap();
        assert_eq!(hbs.render("t", &serde_json::json!({"a": "is:", "b": true})).unwrap(), "is:true");
    }

    #[test]
    fn concat_helper_with_null() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{concat a b c}}").unwrap();
        // Null values should produce nothing (empty)
        assert_eq!(hbs.render("t", &serde_json::json!({"a": "hello", "b": null, "c": "world"})).unwrap(), "helloworld");
    }

    #[test]
    fn concat_helper_with_array_value() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{concat a b}}").unwrap();
        // Array gets serialized via other.to_string()
        let result = hbs.render("t", &serde_json::json!({"a": "data:", "b": [1, 2]})).unwrap();
        assert!(result.starts_with("data:"));
        assert!(result.contains("[1,2]"));
    }

    #[test]
    fn concat_helper_no_params() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{concat}}").unwrap();
        assert_eq!(hbs.render("t", &serde_json::json!({})).unwrap(), "");
    }

    #[test]
    fn concat_helper_single_param() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{concat a}}").unwrap();
        assert_eq!(hbs.render("t", &serde_json::json!({"a": "only"})).unwrap(), "only");
    }

    // --- default helper edge cases ---

    #[test]
    fn default_helper_with_false_uses_fallback() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{default val fallback}}").unwrap();
        assert_eq!(hbs.render("t", &serde_json::json!({"val": false, "fallback": "fallback_val"})).unwrap(), "fallback_val");
    }

    #[test]
    fn default_helper_with_zero_uses_fallback() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{default val fallback}}").unwrap();
        assert_eq!(hbs.render("t", &serde_json::json!({"val": 0, "fallback": "fallback_val"})).unwrap(), "fallback_val");
    }

    #[test]
    fn default_helper_with_empty_array_uses_fallback() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{default val fallback}}").unwrap();
        assert_eq!(hbs.render("t", &serde_json::json!({"val": [], "fallback": "none"})).unwrap(), "none");
    }

    #[test]
    fn default_helper_with_truthy_number() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{default val fallback}}").unwrap();
        assert_eq!(hbs.render("t", &serde_json::json!({"val": 42, "fallback": "nope"})).unwrap(), "42");
    }

    #[test]
    fn default_helper_with_object_is_truthy() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (not (not val))}}TRUTHY{{else}}FALSY{{/if}}").unwrap();
        // Empty object is truthy
        assert_eq!(hbs.render("t", &serde_json::json!({"val": {}})).unwrap(), "TRUTHY");
    }

    // --- comparison helpers with string numbers ---

    #[test]
    fn gt_helper_with_string_numbers() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (gt a b)}}YES{{else}}NO{{/if}}").unwrap();
        // Strings should be parsed as f64 via as_f64
        assert_eq!(hbs.render("t", &serde_json::json!({"a": "10", "b": "5"})).unwrap(), "YES");
        assert_eq!(hbs.render("t", &serde_json::json!({"a": "3", "b": "7"})).unwrap(), "NO");
    }

    #[test]
    fn lt_helper_with_string_numbers() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (lt a b)}}YES{{else}}NO{{/if}}").unwrap();
        assert_eq!(hbs.render("t", &serde_json::json!({"a": "2.5", "b": "3.5"})).unwrap(), "YES");
    }

    #[test]
    fn gte_helper_with_equal_values() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (gte a b)}}YES{{else}}NO{{/if}}").unwrap();
        assert_eq!(hbs.render("t", &serde_json::json!({"a": "5.0", "b": 5})).unwrap(), "YES");
    }

    #[test]
    fn lte_helper_with_equal_values() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (lte a b)}}YES{{else}}NO{{/if}}").unwrap();
        assert_eq!(hbs.render("t", &serde_json::json!({"a": 5, "b": "5.0"})).unwrap(), "YES");
    }

    #[test]
    fn comparison_helpers_with_non_numeric_defaults_to_zero() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (gt a b)}}YES{{else}}NO{{/if}}").unwrap();
        // Non-numeric values default to 0.0, so gt(null, null) is 0.0 > 0.0 = false
        assert_eq!(hbs.render("t", &serde_json::json!({"a": null, "b": null})).unwrap(), "NO");
        // gt(1, null) is 1.0 > 0.0 = true
        assert_eq!(hbs.render("t", &serde_json::json!({"a": 1, "b": null})).unwrap(), "YES");
    }

    // --- eq helper edge cases ---

    #[test]
    fn eq_helper_with_numbers() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (eq a b)}}YES{{else}}NO{{/if}}").unwrap();
        assert_eq!(hbs.render("t", &serde_json::json!({"a": 42, "b": 42})).unwrap(), "YES");
        assert_eq!(hbs.render("t", &serde_json::json!({"a": 42, "b": 43})).unwrap(), "NO");
    }

    #[test]
    fn eq_helper_with_null() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (eq a b)}}YES{{else}}NO{{/if}}").unwrap();
        assert_eq!(hbs.render("t", &serde_json::json!({"a": null, "b": null})).unwrap(), "YES");
    }

    #[test]
    fn eq_helper_with_bool() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (eq a b)}}YES{{else}}NO{{/if}}").unwrap();
        assert_eq!(hbs.render("t", &serde_json::json!({"a": true, "b": true})).unwrap(), "YES");
        assert_eq!(hbs.render("t", &serde_json::json!({"a": true, "b": false})).unwrap(), "NO");
    }

    #[test]
    fn eq_helper_type_mismatch() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (eq a b)}}YES{{else}}NO{{/if}}").unwrap();
        // String "42" vs number 42 should be NOT equal (different JSON types)
        assert_eq!(hbs.render("t", &serde_json::json!({"a": "42", "b": 42})).unwrap(), "NO");
    }

    // --- not helper with more types ---

    #[test]
    fn not_helper_with_number() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (not val)}}YES{{else}}NO{{/if}}").unwrap();
        assert_eq!(hbs.render("t", &serde_json::json!({"val": 0})).unwrap(), "YES");
        assert_eq!(hbs.render("t", &serde_json::json!({"val": 1})).unwrap(), "NO");
        assert_eq!(hbs.render("t", &serde_json::json!({"val": -1})).unwrap(), "NO");
    }

    #[test]
    fn not_helper_with_array() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (not val)}}YES{{else}}NO{{/if}}").unwrap();
        assert_eq!(hbs.render("t", &serde_json::json!({"val": []})).unwrap(), "YES");
        assert_eq!(hbs.render("t", &serde_json::json!({"val": [1]})).unwrap(), "NO");
    }

    #[test]
    fn not_helper_with_object() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (not val)}}YES{{else}}NO{{/if}}").unwrap();
        // Empty object is truthy
        assert_eq!(hbs.render("t", &serde_json::json!({"val": {}})).unwrap(), "NO");
    }

    // --- and/or with non-bool values ---

    #[test]
    fn and_helper_with_strings() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (and a b)}}YES{{else}}NO{{/if}}").unwrap();
        assert_eq!(hbs.render("t", &serde_json::json!({"a": "hello", "b": "world"})).unwrap(), "YES");
        assert_eq!(hbs.render("t", &serde_json::json!({"a": "hello", "b": ""})).unwrap(), "NO");
    }

    #[test]
    fn and_helper_with_numbers() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (and a b)}}YES{{else}}NO{{/if}}").unwrap();
        assert_eq!(hbs.render("t", &serde_json::json!({"a": 1, "b": 2})).unwrap(), "YES");
        assert_eq!(hbs.render("t", &serde_json::json!({"a": 1, "b": 0})).unwrap(), "NO");
    }

    #[test]
    fn or_helper_with_strings() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (or a b)}}YES{{else}}NO{{/if}}").unwrap();
        assert_eq!(hbs.render("t", &serde_json::json!({"a": "", "b": ""})).unwrap(), "NO");
        assert_eq!(hbs.render("t", &serde_json::json!({"a": "", "b": "x"})).unwrap(), "YES");
    }

    #[test]
    fn or_helper_with_null_and_value() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (or a b)}}YES{{else}}NO{{/if}}").unwrap();
        assert_eq!(hbs.render("t", &serde_json::json!({"a": null, "b": null})).unwrap(), "NO");
        assert_eq!(hbs.render("t", &serde_json::json!({"a": null, "b": 1})).unwrap(), "YES");
    }

    // --- json helper edge cases ---

    #[test]
    fn json_helper_with_null() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{{json val}}}").unwrap();
        let result = hbs.render("t", &serde_json::json!({"val": null})).unwrap();
        assert_eq!(result, "null");
    }

    #[test]
    fn json_helper_with_array() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{{json val}}}").unwrap();
        let result = hbs.render("t", &serde_json::json!({"val": [1, "two", true]})).unwrap();
        assert_eq!(result, r#"[1,"two",true]"#);
    }

    #[test]
    fn json_helper_with_string() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{{json val}}}").unwrap();
        let result = hbs.render("t", &serde_json::json!({"val": "hello"})).unwrap();
        assert_eq!(result, "\"hello\"");
    }

    #[test]
    fn json_helper_with_no_param() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{{json}}}").unwrap();
        // No param -> defaults to null
        let result = hbs.render("t", &serde_json::json!({})).unwrap();
        assert_eq!(result, "null");
    }

    // --- is_truthy additional coverage ---

    #[test]
    fn is_truthy_float_zero() {
        assert!(!is_truthy(&serde_json::json!(0.0)));
        assert!(is_truthy(&serde_json::json!(0.1)));
    }

    // --- as_f64 additional coverage ---

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

    // --- register_embedded_templates coverage ---

    #[test]
    fn embedded_templates_include_dashboard() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let translations = Arc::new(Translations::load(tmp.path()));
        let hbs = create_handlebars(tmp.path(), false, translations).expect("hbs");
        // The compiled-in templates should include dashboard/index
        let result = hbs.render("dashboard/index", &serde_json::json!({
            "title": "Dashboard",
            "collections": [],
            "globals": [],
        }));
        assert!(result.is_ok(), "dashboard/index should be registered: {:?}", result.err());
    }

    #[test]
    fn embedded_templates_include_field_partials() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let translations = Arc::new(Translations::load(tmp.path()));
        let hbs = create_handlebars(tmp.path(), false, translations).expect("hbs");
        // Field partials should be registered
        let result = hbs.render("fields/text", &serde_json::json!({
            "name": "test",
            "label": "Test",
            "value": "hello",
        }));
        assert!(result.is_ok(), "fields/text should be registered: {:?}", result.err());
    }

    // --- Combined helper tests ---

    #[test]
    fn nested_helpers_work() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (and (not a) (or b c))}}YES{{else}}NO{{/if}}").unwrap();
        // not(false) = true, or(true, false) = true, and(true, true) = true
        assert_eq!(hbs.render("t", &serde_json::json!({"a": false, "b": true, "c": false})).unwrap(), "YES");
        // not(true) = false, and(false, anything) = false
        assert_eq!(hbs.render("t", &serde_json::json!({"a": true, "b": true, "c": true})).unwrap(), "NO");
    }

    #[test]
    fn eq_with_contains_composition() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (and (eq status \"active\") (contains tags \"vip\"))}}YES{{else}}NO{{/if}}").unwrap();
        assert_eq!(hbs.render("t", &serde_json::json!({
            "status": "active", "tags": ["vip", "premium"]
        })).unwrap(), "YES");
        assert_eq!(hbs.render("t", &serde_json::json!({
            "status": "inactive", "tags": ["vip", "premium"]
        })).unwrap(), "NO");
        assert_eq!(hbs.render("t", &serde_json::json!({
            "status": "active", "tags": ["basic"]
        })).unwrap(), "NO");
    }
}
