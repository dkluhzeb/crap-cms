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
        _ctx: &'rc handlebars::Context,
        _rc: &mut RenderContext<'reg, 'rc>,
    ) -> Result<ScopedJson<'rc>, RenderError> {
        let key = h.param(0)
            .and_then(|p| p.value().as_str())
            .unwrap_or("");

        let hash = h.hash();
        if hash.is_empty() {
            let translated = self.translations.get(key);
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
            let translated = self.translations.get_interpolated(key, &params);
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
        let translations = Arc::new(Translations::load(tmp.path(), "en"));
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
        let translations = Arc::new(Translations::load(tmp.path(), "en"));
        let hbs = create_handlebars(tmp.path(), false, translations).expect("create_handlebars");
        // Register a test template using the eq helper
        let mut hbs_mut = (*hbs).clone();
        hbs_mut.register_template_string("test_eq", "{{#if (eq a b)}}EQUAL{{else}}NOT_EQUAL{{/if}}")
            .expect("register");
        let result = hbs_mut.render("test_eq", &serde_json::json!({"a": "foo", "b": "foo"})).unwrap();
        assert_eq!(result, "EQUAL");
        let result = hbs_mut.render("test_eq", &serde_json::json!({"a": "foo", "b": "bar"})).unwrap();
        assert_eq!(result, "NOT_EQUAL");
    }
}
