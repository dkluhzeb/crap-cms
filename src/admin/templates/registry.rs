//! Handlebars template loading with overlay (config dir overrides compiled defaults).

use std::{fs, path::Path, str, sync::Arc};

use anyhow::{Context as _, Result};
use handlebars::Handlebars;
use include_dir::{Dir, include_dir};
use tracing::debug;

use crate::admin::Translations;

use super::helpers;

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

    register_embedded_templates(&mut hbs)?;

    let templates_dir = config_dir.join("templates");
    if templates_dir.exists() {
        register_dir_templates(&mut hbs, &templates_dir)?;
    }

    helpers::register_helpers(&mut hbs, translations);

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

    /// Regression: the `crap-i18n` data island in `layout/base.hbs`
    /// must render as parseable JSON. Earlier the body was hand-written
    /// per-key with `{{t "..."}}` interpolation inside JSON string
    /// values, which the template formatter then split across lines —
    /// producing JSON strings with literal newlines and a silent
    /// `JSON.parse` failure in `static/components/i18n.js` (every
    /// admin label fell back to its raw key). The current implementation
    /// uses the `{{{admin_i18n}}}` helper which emits a single,
    /// formatter-opaque JSON object.
    #[test]
    fn base_layout_i18n_island_renders_valid_json() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let translations = Arc::new(Translations::load(tmp.path()));
        let mut hbs = (*create_handlebars(tmp.path(), false, translations).expect("hbs")).clone();
        // Wrap the layout — base.hbs uses partial-block syntax for the
        // page body slot.
        hbs.register_template_string("page", "{{#> layout/base}}body{{/layout/base}}")
            .expect("register page");

        let html = hbs
            .render(
                "page",
                &json!({
                    "_locale": "en",
                    "crap": { "csp_nonce": "test-nonce" },
                    "nav": { "collections": [], "globals": [] },
                    "available_locales": ["en"],
                    "page": { "title": "x", "title_name": "" },
                }),
            )
            .expect("render");

        // Pull the body of the i18n data island and parse.
        let needle = r#"id="crap-i18n""#;
        let start_attr = html.find(needle).expect("data island present");
        let body_start = html[start_attr..]
            .find('>')
            .map(|p| start_attr + p + 1)
            .expect("opening tag close");
        let body_end = body_start
            + html[body_start..]
                .find("</script>")
                .expect("closing </script>");
        let body = html[body_start..body_end].trim();

        let parsed: serde_json::Value = serde_json::from_str(body)
            .unwrap_or_else(|err| panic!("i18n island invalid JSON: {err}\n---body---\n{body}"));
        let obj = parsed.as_object().expect("must be JSON object");

        // Spot-check: a known key resolves to its English translation.
        assert_eq!(
            obj.get("save").and_then(|v| v.as_str()),
            Some("Save"),
            "expected 'save' to resolve to 'Save' in en, got: {body}"
        );
    }

    /// Regression: textarea-style fields (textarea, json, code, richtext)
    /// must not leave any whitespace between the rendered value and
    /// `</textarea>`. HTML5 only strips a single leading LF after
    /// `<textarea>`; everything else — indentation, trailing newlines —
    /// becomes part of the field's submitted value. With surrounding
    /// whitespace, every save round-trip wraps the value with another
    /// layer of indentation. The fix is `>{{value}}</textarea>` flush
    /// in the source plus formatter support for inline close tags on
    /// raw-content elements.
    ///
    /// Round-trip simulation: render the field with a stable `value`,
    /// then "submit" the resulting browser-visible value back through
    /// the renderer. The output must converge — same body two passes
    /// in a row. If the indent regression returns, the second pass
    /// produces a longer body than the first.
    #[test]
    fn textarea_field_value_does_not_accrete_whitespace_on_round_trip() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let translations = Arc::new(Translations::load(tmp.path()));
        let hbs = create_handlebars(tmp.path(), false, translations).expect("hbs");

        // What the browser would set the textarea's `value` property to,
        // given its DOM children. Per HTML5: a single leading LF is
        // stripped; everything else is preserved verbatim.
        fn browser_textarea_value(rendered: &str) -> String {
            let open = rendered
                .find("<textarea")
                .expect("rendered HTML must contain <textarea");
            let body_start = rendered[open..]
                .find('>')
                .map(|p| open + p + 1)
                .expect("opening tag close");
            let body_end = body_start
                + rendered[body_start..]
                    .find("</textarea>")
                    .expect("closing </textarea>");
            let body = &rendered[body_start..body_end];
            body.strip_prefix('\n').unwrap_or(body).to_string()
        }

        for field_type in ["textarea", "json", "code", "richtext"] {
            let pass1 = hbs
                .render(
                    &format!("fields/{field_type}"),
                    &json!({"name": "body", "value": "hello", "rows": 4}),
                )
                .unwrap_or_else(|e| panic!("render {field_type}: {e}"));
            let v1 = browser_textarea_value(&pass1);

            let pass2 = hbs
                .render(
                    &format!("fields/{field_type}"),
                    &json!({"name": "body", "value": v1.clone(), "rows": 4}),
                )
                .unwrap_or_else(|e| panic!("re-render {field_type}: {e}"));
            let v2 = browser_textarea_value(&pass2);

            assert_eq!(
                v1, v2,
                "{field_type}: value accreted whitespace on round-trip\n  pass1: {v1:?}\n  pass2: {v2:?}"
            );
            assert_eq!(
                v1, "hello",
                "{field_type}: round-trip value should equal input, got {v1:?}"
            );
        }
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

    #[test]
    fn htmx_nav_link_partial_renders_button_with_hx_attrs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let translations = Arc::new(Translations::load(tmp.path()));
        let mut hbs = (*create_handlebars(tmp.path(), false, translations).expect("hbs")).clone();
        hbs.register_template_string(
            "t",
            r#"{{> partials/htmx-nav-link href="/admin/foo" label_key="cancel" variant="primary" icon="arrow_back"}}"#,
        )
        .expect("register caller");

        let html = hbs.render("t", &json!({})).expect("render");

        assert!(html.contains(r#"href="/admin/foo""#), "{html}");
        assert!(html.contains(r#"hx-get="/admin/foo""#), "{html}");
        assert!(html.contains(r#"hx-target="body""#), "{html}");
        assert!(html.contains(r#"hx-push-url="true""#), "{html}");
        assert!(html.contains(r#"button button--primary"#), "{html}");
        assert!(
            html.contains(r#"<span class="material-symbols-outlined">arrow_back</span>"#),
            "default-size icon (no size param given): {html}"
        );
    }

    #[test]
    fn htmx_nav_link_partial_small_size_uses_small_icon_class() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let translations = Arc::new(Translations::load(tmp.path()));
        let mut hbs = (*create_handlebars(tmp.path(), false, translations).expect("hbs")).clone();
        hbs.register_template_string(
            "t",
            r#"{{> partials/htmx-nav-link href="/admin" label="View" size="small" icon="open_in_new"}}"#,
        )
        .expect("register caller");

        let html = hbs.render("t", &json!({})).expect("render");

        assert!(html.contains("button--small"), "{html}");
        assert!(
            html.contains(r#"<span class="material-symbols-outlined icon--sm">"#),
            "small button gets small icon: {html}"
        );
    }

    #[test]
    fn htmx_nav_link_partial_defaults_to_ghost_variant() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let translations = Arc::new(Translations::load(tmp.path()));
        let mut hbs = (*create_handlebars(tmp.path(), false, translations).expect("hbs")).clone();
        hbs.register_template_string(
            "t",
            r#"{{> partials/htmx-nav-link href="/admin" label="Back"}}"#,
        )
        .expect("register caller");

        let html = hbs.render("t", &json!({})).expect("render");

        assert!(html.contains(r#"button button--ghost"#), "{html}");
        assert!(html.contains("Back"), "{html}");
        assert!(
            !html.contains("material-symbols"),
            "no icon expected: {html}"
        );
    }

    #[test]
    fn status_badge_partial_renders_badge_with_status_modifier() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let translations = Arc::new(Translations::load(tmp.path()));
        let mut hbs = (*create_handlebars(tmp.path(), false, translations).expect("hbs")).clone();
        hbs.register_template_string("t", r#"{{> partials/status-badge status="published"}}"#)
            .expect("register caller");

        let html = hbs.render("t", &json!({})).expect("render");
        assert!(
            html.contains(r#"<span class="badge badge--published">published</span>"#),
            "{html}"
        );
    }

    #[test]
    fn warning_card_partial_renders_title_and_slot_body() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let translations = Arc::new(Translations::load(tmp.path()));
        let mut hbs = (*create_handlebars(tmp.path(), false, translations).expect("hbs")).clone();
        hbs.register_template_string(
            "t",
            r#"{{#> partials/warning-card title_key="delete_has_references"}}<p>3 incoming refs</p>{{/partials/warning-card}}"#,
        )
        .expect("register caller");

        let html = hbs.render("t", &json!({})).expect("render");

        assert!(
            html.contains(r#"<div class="card card--warning">"#),
            "{html}"
        );
        assert!(html.contains("<strong>"), "{html}");
        assert!(html.contains("<p>3 incoming refs</p>"), "{html}");
    }

    #[test]
    fn loading_indicator_partial_inline_variant_is_default() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let translations = Arc::new(Translations::load(tmp.path()));
        let mut hbs = (*create_handlebars(tmp.path(), false, translations).expect("hbs")).clone();
        hbs.register_template_string("t", r#"{{> partials/loading-indicator}}"#)
            .expect("register caller");

        let html = hbs.render("t", &json!({})).expect("render");

        assert!(html.contains(r#"id="upload-loading""#), "{html}");
        assert!(
            html.contains("loading-indicator") && !html.contains("edit-sidebar__save-indicator"),
            "inline variant must use plain loading-indicator class: {html}"
        );
    }

    #[test]
    fn loading_indicator_partial_sidebar_variant_uses_sidebar_class() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let translations = Arc::new(Translations::load(tmp.path()));
        let mut hbs = (*create_handlebars(tmp.path(), false, translations).expect("hbs")).clone();
        hbs.register_template_string("t", r#"{{> partials/loading-indicator variant="sidebar"}}"#)
            .expect("register caller");

        let html = hbs.render("t", &json!({})).expect("render");

        assert!(html.contains("edit-sidebar__save-indicator"), "{html}");
    }

    #[test]
    fn array_row_header_partial_renders_drag_toggle_buttons() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let translations = Arc::new(Translations::load(tmp.path()));
        let mut hbs = (*create_handlebars(tmp.path(), false, translations).expect("hbs")).clone();
        hbs.register_template_string(
            "t",
            "{{#> partials/array-row-header expanded=true has_errors=true}}Title 0{{/partials/array-row-header}}",
        )
        .expect("register caller");

        let html = hbs.render("t", &json!({})).expect("render");

        assert!(html.contains(r#"data-action="toggle-array-row""#), "{html}");
        assert!(html.contains(r#"class="form__array-row-drag""#), "{html}");
        assert!(html.contains(r#"aria-expanded="true""#), "{html}");
        assert!(
            html.contains(r#"<span class="form__array-row-title">Title 0</span>"#),
            "{html}"
        );
        assert!(html.contains(r#"form__array-row-error-badge"#), "{html}");
        assert!(html.contains(r#"data-action="move-row-up""#), "{html}");
        assert!(html.contains(r#"data-action="move-row-down""#), "{html}");
        assert!(html.contains(r#"data-action="duplicate-row""#), "{html}");
        assert!(html.contains(r#"data-action="remove-array-row""#), "{html}");
    }

    #[test]
    fn array_row_header_partial_collapsed_no_errors() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let translations = Arc::new(Translations::load(tmp.path()));
        let mut hbs = (*create_handlebars(tmp.path(), false, translations).expect("hbs")).clone();
        hbs.register_template_string(
            "t",
            "{{#> partials/array-row-header expanded=false has_errors=false}}foo{{/partials/array-row-header}}",
        )
        .expect("register caller");

        let html = hbs.render("t", &json!({})).expect("render");

        assert!(html.contains(r#"aria-expanded="false""#), "{html}");
        assert!(!html.contains("form__array-row-error-badge"), "{html}");
    }

    #[test]
    fn field_partial_fieldset_variant_wraps_radio_in_legend() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let translations = Arc::new(Translations::load(tmp.path()));
        let hbs = create_handlebars(tmp.path(), false, translations).expect("hbs");

        let html = hbs
            .render(
                "fields/radio",
                &json!({
                    "name": "color",
                    "label": "Color",
                    "required": true,
                    "locale_locked": true,
                    "error": "pick one",
                    "options": [
                        { "value": "red", "label": "Red", "selected": false },
                        { "value": "blue", "label": "Blue", "selected": true },
                    ],
                }),
            )
            .expect("render");

        assert!(
            html.contains(r#"<fieldset class="form__radio-group">"#),
            "{html}"
        );
        assert!(html.contains("<legend>"), "{html}");
        assert!(
            html.contains(r#"<span class="required">*</span>"#),
            "{html}"
        );
        assert!(html.contains(r#"form__locale-badge"#), "{html}");
        assert!(html.contains(r#"type="radio""#), "{html}");
        assert!(
            html.contains(r#"<p class="form__error">pick one</p>"#),
            "{html}"
        );
        assert!(
            !html.contains(r#"<label for="field-color">"#),
            "fieldset variant must not emit a label-for: {html}"
        );
    }

    #[test]
    fn field_partial_checkbox_variant_renders_input_then_label() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let translations = Arc::new(Translations::load(tmp.path()));
        let hbs = create_handlebars(tmp.path(), false, translations).expect("hbs");

        let html = hbs
            .render(
                "fields/checkbox",
                &json!({
                    "name": "tos",
                    "label": "I agree",
                    "required": true,
                    "checked": false,
                    "description": "Terms of service",
                }),
            )
            .expect("render");

        assert!(html.contains(r#"<div class="form__checkbox">"#), "{html}");
        assert!(html.contains(r#"type="checkbox""#), "{html}");
        assert!(html.contains(r#"<label for="field-tos">"#), "{html}");
        assert!(
            html.contains(r#"<span class="required">*</span>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<p class="form__help">Terms of service</p>"#),
            "{html}"
        );

        let input_pos = html.find("<input").expect("input present");
        let label_pos = html
            .find(r#"<label for="field-tos""#)
            .expect("label present");
        assert!(
            input_pos < label_pos,
            "input must come before label in checkbox variant: {html}"
        );
    }

    #[test]
    fn field_partial_explicit_params_override_inherited_context() {
        // Locks in the documented behaviour that explicit `{{#> partials/field
        // label="..." error="..."}}` arguments win over keys with the same
        // name in the parent context.
        let tmp = tempfile::tempdir().expect("tempdir");
        let translations = Arc::new(Translations::load(tmp.path()));
        let mut hbs = (*create_handlebars(tmp.path(), false, translations).expect("hbs")).clone();
        hbs.register_template_string(
            "t",
            r#"{{#> partials/field name="custom" label="Override Label" error="explicit error"}}<input type="text" />{{/partials/field}}"#,
        )
        .expect("register caller");

        // Parent context has different label/error values; the explicit
        // partial-call values must take precedence.
        let html = hbs
            .render(
                "t",
                &json!({
                    "name": "inherited",
                    "label": "Inherited Label",
                    "error": "inherited error",
                }),
            )
            .expect("render");

        assert!(html.contains(r#"<label for="field-custom">"#), "{html}");
        assert!(html.contains("Override Label"), "{html}");
        assert!(
            !html.contains("Inherited Label"),
            "must not render inherited label: {html}"
        );
        assert!(
            html.contains(r#"<p class="form__error">explicit error</p>"#),
            "{html}"
        );
        assert!(
            !html.contains("inherited error"),
            "must not render inherited error: {html}"
        );
    }

    #[test]
    fn field_partial_wraps_label_required_locale_badge_error_help() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let translations = Arc::new(Translations::load(tmp.path()));
        let hbs = create_handlebars(tmp.path(), false, translations).expect("hbs");

        let html = hbs
            .render(
                "fields/email",
                &json!({
                    "name": "contact",
                    "label": "Contact",
                    "value": "a@b.c",
                    "required": true,
                    "locale_locked": true,
                    "error": "bad",
                    "description": "help text",
                }),
            )
            .expect("render");

        assert!(html.contains(r#"<label for="field-contact">"#), "{html}");
        assert!(
            html.contains(r#"<span class="required">*</span>"#),
            "{html}"
        );
        assert!(html.contains(r#"form__locale-badge"#), "{html}");
        assert!(
            html.contains(r#"<input"#) && html.contains(r#"type="email""#),
            "{html}"
        );
        assert!(html.contains(r#"<p class="form__error">bad</p>"#), "{html}");
        assert!(
            html.contains(r#"<p class="form__help">help text</p>"#),
            "{html}"
        );
    }
}
