//! Handlebars template loading with overlay (config dir overrides compiled defaults).

use std::{fs, path::Path, str, sync::Arc};

use anyhow::{Context as _, Result};
use handlebars::Handlebars;
use include_dir::{Dir, include_dir};
use tracing::debug;

use crate::{admin::Translations, hooks::HookRunner};

use super::helpers;

static TEMPLATES_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/templates");

/// Create a Handlebars instance with embedded defaults, config overlays, and helpers.
///
/// `hook_runner` enables the Lua-backed `{{data "name"}}` helper. Tests
/// pass `None` to skip Lua wiring.
///
/// # Errors
///
/// Returns an error if a compiled-in or overlay template fails to register.
pub fn create_handlebars(
    config_dir: &Path,
    dev_mode: bool,
    translations: Arc<Translations>,
    hook_runner: Option<Arc<HookRunner>>,
) -> Result<Arc<Handlebars<'static>>> {
    let mut hbs = Handlebars::new();
    hbs.set_dev_mode(dev_mode);
    hbs.set_strict_mode(false);

    register_embedded_templates(&mut hbs)?;

    let templates_dir = config_dir.join("templates");
    if templates_dir.exists() {
        register_dir_templates(&mut hbs, &templates_dir)?;
    }

    helpers::register_helpers(&mut hbs, translations, hook_runner);

    Ok(Arc::new(hbs))
}

/// Register all compiled-in `.hbs` templates from the embedded directory.
fn register_embedded_templates(hbs: &mut Handlebars) -> Result<()> {
    register_embedded_dir(hbs, &TEMPLATES_DIR)
}

/// Recursively walk an embedded directory, registering each `.hbs` file as a template.
fn register_embedded_dir(hbs: &mut Handlebars, dir: &Dir) -> Result<()> {
    for file in dir.files() {
        let path = file.path();
        if path.extension().is_some_and(|ext| ext == "hbs") {
            let name_str = path.with_extension("").to_string_lossy().to_string();
            let content = str::from_utf8(file.contents())
                .with_context(|| format!("Invalid UTF-8 in template: {name_str}"))?;

            hbs.register_template_string(&name_str, content)
                .with_context(|| format!("Failed to register template: {name_str}"))?;
        }
    }

    for subdir in dir.dirs() {
        register_embedded_dir(hbs, subdir)?;
    }

    Ok(())
}

/// Register config-dir overlay templates, overriding compiled defaults where present.
fn register_dir_templates(hbs: &mut Handlebars, dir: &Path) -> Result<()> {
    register_dir_recursive(hbs, dir, dir)
}

/// Recursively walk a filesystem directory, registering each `.hbs` file as a template.
/// Template names are derived from the path relative to `base` (without extension).
fn register_dir_recursive(hbs: &mut Handlebars, base: &Path, dir: &Path) -> Result<()> {
    let entries = fs::read_dir(dir)
        .with_context(|| format!("Failed to read directory: {}", dir.display()))?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            register_dir_recursive(hbs, base, &path)?;
        } else if path.extension().is_some_and(|ext| ext == "hbs") {
            let Ok(relative) = path.strip_prefix(base) else {
                continue;
            };
            let name = relative.with_extension("").to_string_lossy().to_string();

            debug!("Overlay template: {}", name);

            // Register by path, not by string: that keeps the file as the
            // template's source, which is what makes dev mode re-read it from
            // disk on every render. Compiled-in defaults stay strings — they
            // have no file to watch.
            hbs.register_template_file(&name, &path)
                .with_context(|| format!("Failed to register overlay template: {name}"))?;
        }
    }

    Ok(())
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
mod tests;
