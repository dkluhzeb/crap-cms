//! `make field` -- scaffold a per-field render template binding via
//! `admin.template = "fields/<name>"`. Three files:
//!
//!   1. `templates/fields/<name>.hbs` -- the per-field template.
//!   2. `plugins/<name>.lua` -- a `field()` factory wrapping
//!      `crap.fields.<base_type>` and pre-setting `admin.template`.
//!   3. `static/components/<name>.js` -- Web Component skeleton.
//!
//! Plus a printed snippet for how to register the component in
//! `custom.js`. Files are coordinated: the field name is the same
//! across all three so the binding is consistent.

use std::{fs, path::Path};

use anyhow::{Context as _, Result, bail};
use serde::Serialize;

use crate::{
    cli,
    scaffold::{
        component, guards::refuse_file_overwrite, paths, render, to_title_case, validate_slug,
    },
};

#[derive(Serialize)]
struct FieldTemplateCtx<'a> {
    name: &'a str,
    tag: &'a str,
}

#[derive(Serialize)]
struct FieldPluginCtx<'a> {
    name: &'a str,
    base_type: &'a str,
    label: &'a str,
}

/// Built-in field types that can be wrapped. Restricts to scalar types
/// (no array/group/blocks/relationship) since per-field templates only
/// make sense for atomic data shapes.
const VALID_BASE_TYPES: &[&str] = &[
    "text", "number", "textarea", "select", "radio", "checkbox", "date", "email", "json", "code",
];

/// Options for `make_field`.
pub struct MakeFieldOptions<'a> {
    pub config_dir: &'a Path,
    /// Field name (also the template name and component tag suffix).
    pub name: &'a str,
    /// Base field type to wrap (default: `"number"`).
    pub base_type: Option<&'a str>,
    pub force: bool,
}

/// Scaffold the three files.
///
/// # Errors
///
/// Returns an error if the name is invalid, the base type is unknown, any
/// target file already exists without `--force`, or writing fails.
pub fn make_field(opts: &MakeFieldOptions) -> Result<()> {
    validate_slug(opts.name)?;
    let base_type = opts.base_type.unwrap_or("number");
    if !VALID_BASE_TYPES.contains(&base_type) {
        bail!(
            "invalid base type '{}' (allowed: {})",
            base_type,
            VALID_BASE_TYPES.join(", ")
        );
    }

    let label = to_title_case(opts.name);
    let component_tag = format!("crap-{}", opts.name);

    // 1. Per-field template
    let tpl_dir = paths::templates_fields_dir(opts.config_dir);
    fs::create_dir_all(&tpl_dir).context("Failed to create templates/fields/ directory")?;
    let tpl_path = tpl_dir.join(format!("{}.hbs", opts.name));
    refuse_file_overwrite(&tpl_path, opts.force)?;
    fs::write(&tpl_path, render_template_hbs(opts.name, &component_tag)?)
        .with_context(|| format!("Failed to write {}", tpl_path.display()))?;

    // 2. Lua plugin wrapper
    let plug_dir = paths::plugins_dir(opts.config_dir);
    fs::create_dir_all(&plug_dir).context("Failed to create plugins/ directory")?;
    let plug_path = plug_dir.join(format!("{}.lua", opts.name));
    refuse_file_overwrite(&plug_path, opts.force)?;
    fs::write(&plug_path, render_plugin_lua(opts.name, base_type, &label)?)
        .with_context(|| format!("Failed to write {}", plug_path.display()))?;

    // 3. Web Component -- reuse the make_component generator so the
    //    skeleton stays consistent with `make component`.
    component::make_component(&component::MakeComponentOptions {
        config_dir: opts.config_dir,
        tag: &component_tag,
        force: opts.force,
    })?;

    cli::success(&format!(
        "Created field '{}' -- three files wired together via admin.template.",
        opts.name
    ));
    cli::info(&format!(
        "\nUse it in a collection:\n\n  local {name} = require(\"plugins.{name}\")\n\n  crap.collections.define(\"products\", {{\n    fields = {{\n      {name}.field({{ name = \"my_{name}\" }}),\n      ...\n    }},\n  }})",
        name = opts.name,
    ));

    Ok(())
}

fn render_template_hbs(name: &str, tag: &str) -> Result<String> {
    render::render("field_template", &FieldTemplateCtx { name, tag })
}

fn render_plugin_lua(name: &str, base_type: &str, label: &str) -> Result<String> {
    render::render(
        "field_plugin",
        &FieldPluginCtx {
            name,
            base_type,
            label,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The scaffolded field template must already satisfy `crap-cms fmt`
    /// — otherwise a fresh `make field` immediately fails the user's
    /// pre-commit `fmt --check`.
    #[test]
    fn generated_template_is_formatter_clean() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_field(&MakeFieldOptions {
            config_dir: tmp.path(),
            name: "rating",
            base_type: None,
            force: false,
        })
        .unwrap();

        let src = fs::read_to_string(tmp.path().join("templates/fields/rating.hbs")).unwrap();
        let formatted = crate::fmt::format(&src).unwrap();
        assert_eq!(formatted, src, "make field output must be fmt-clean");
    }

    #[test]
    fn writes_three_coordinated_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_field(&MakeFieldOptions {
            config_dir: tmp.path(),
            name: "rating",
            base_type: Some("number"),
            force: false,
        })
        .unwrap();

        let tpl = tmp.path().join("templates/fields/rating.hbs");
        let plug = tmp.path().join("plugins/rating.lua");
        let comp = tmp.path().join("static/components/crap-rating.js");
        assert!(tpl.exists(), "template should be created");
        assert!(plug.exists(), "plugin lua should be created");
        assert!(comp.exists(), "web component should be created");

        let tpl_body = fs::read_to_string(&tpl).unwrap();
        assert!(tpl_body.contains("<crap-rating"));
        assert!(tpl_body.contains("partials/field"));

        let plug_body = fs::read_to_string(&plug).unwrap();
        assert!(plug_body.contains(r#"admin.template = "fields/rating""#));
        assert!(plug_body.contains("crap.fields.number("));

        let comp_body = fs::read_to_string(&comp).unwrap();
        assert!(comp_body.contains("class CrapRating extends HTMLElement"));
    }

    #[test]
    fn rejects_invalid_base_type() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let err = make_field(&MakeFieldOptions {
            config_dir: tmp.path(),
            name: "rating",
            base_type: Some("array"),
            force: false,
        })
        .unwrap_err();
        assert!(err.to_string().contains("invalid base type"));
    }

    #[test]
    fn refuses_to_overwrite_without_force() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = MakeFieldOptions {
            config_dir: tmp.path(),
            name: "rating",
            base_type: None,
            force: false,
        };
        make_field(&opts).unwrap();
        let err = make_field(&opts).unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }
}
