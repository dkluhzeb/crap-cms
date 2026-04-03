//! Handlebars template loading with overlay (config dir overrides compiled defaults).

mod helpers;

use std::{fs, path::Path, str, sync::Arc};

use anyhow::{Context as _, Result};
use handlebars::Handlebars;
use include_dir::{Dir, include_dir};
use tracing::debug;

use crate::admin::Translations;

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
    helpers::register_helpers(&mut hbs, translations);

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
            let name_str = path.with_extension("").to_string_lossy().to_string();
            let content = str::from_utf8(file.contents())
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
    let entries = fs::read_dir(dir)
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
            let content = fs::read_to_string(&path)
                .with_context(|| format!("Failed to read template: {}", path.display()))?;

            debug!("Overlay template: {}", name);

            hbs.register_template_string(&name, &content)
                .with_context(|| format!("Failed to register overlay template: {}", name))?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn create_handlebars_loads_templates() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let translations = Arc::new(Translations::load(tmp.path()));
        let hbs = create_handlebars(tmp.path(), false, translations).expect("create_handlebars");
        let result = hbs.render(
            "auth/login",
            &json!({
                "title": "Login",
                "collections": [],
            }),
        );
        assert!(
            result.is_ok(),
            "Should render auth/login template: {:?}",
            result.err()
        );
    }

    #[test]
    fn overlay_templates_override_compiled_defaults() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let templates_dir = tmp.path().join("templates").join("auth");
        fs::create_dir_all(&templates_dir).unwrap();
        fs::write(templates_dir.join("login.hbs"), "CUSTOM_LOGIN_PAGE").unwrap();

        let translations = Arc::new(Translations::load(tmp.path()));
        let hbs = create_handlebars(tmp.path(), false, translations).expect("create_handlebars");
        let result = hbs.render("auth/login", &json!({})).unwrap();
        assert_eq!(result, "CUSTOM_LOGIN_PAGE");
    }

    #[test]
    fn overlay_templates_nested_subdirectory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let nested_dir = tmp.path().join("templates").join("custom").join("deep");
        fs::create_dir_all(&nested_dir).unwrap();
        fs::write(nested_dir.join("page.hbs"), "DEEP_NESTED").unwrap();

        let translations = Arc::new(Translations::load(tmp.path()));
        let hbs = create_handlebars(tmp.path(), false, translations).expect("create_handlebars");
        let result = hbs.render("custom/deep/page", &json!({})).unwrap();
        assert_eq!(result, "DEEP_NESTED");
    }

    #[test]
    fn non_hbs_files_are_ignored_in_overlay() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let templates_dir = tmp.path().join("templates");
        fs::create_dir_all(&templates_dir).unwrap();
        fs::write(templates_dir.join("notes.txt"), "not a template").unwrap();
        fs::write(templates_dir.join("custom.hbs"), "IS_TEMPLATE").unwrap();

        let translations = Arc::new(Translations::load(tmp.path()));
        let hbs = create_handlebars(tmp.path(), false, translations).expect("create_handlebars");
        assert!(hbs.render("custom", &json!({})).is_ok());
        assert!(hbs.render("notes", &json!({})).is_err());
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

    #[test]
    fn embedded_templates_include_dashboard() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let translations = Arc::new(Translations::load(tmp.path()));
        let hbs = create_handlebars(tmp.path(), false, translations).expect("hbs");
        let result = hbs.render(
            "dashboard/index",
            &json!({
                "title": "Dashboard",
                "collections": [],
                "globals": [],
            }),
        );
        assert!(
            result.is_ok(),
            "dashboard/index should be registered: {:?}",
            result.err()
        );
    }

    #[test]
    fn embedded_templates_include_field_partials() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let translations = Arc::new(Translations::load(tmp.path()));
        let hbs = create_handlebars(tmp.path(), false, translations).expect("hbs");
        let result = hbs.render(
            "fields/text",
            &json!({
                "name": "test",
                "label": "Test",
                "value": "hello",
            }),
        );
        assert!(
            result.is_ok(),
            "fields/text should be registered: {:?}",
            result.err()
        );
    }
}
