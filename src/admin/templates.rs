//! Handlebars template loading with overlay (config dir overrides compiled defaults).

use anyhow::{Context, Result};
use handlebars::{Handlebars, RenderError, RenderContext, Helper, HelperDef, ScopedJson};
use include_dir::{include_dir, Dir};
use std::path::Path;
use std::sync::Arc;

static TEMPLATES_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/templates");

/// Create a Handlebars instance with embedded defaults, config overlays, and helpers.
pub fn create_handlebars(config_dir: &Path, dev_mode: bool) -> Result<Arc<Handlebars<'static>>> {
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
