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

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail};
use serde::Serialize;

use crate::{
    cli,
    scaffold::{
        EMBEDDED_TEMPLATES,
        component::{self, component_path},
        guards::refuse_file_overwrite,
        paths, render, to_title_case, validate_slug,
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

/// The three files `make field` writes.
struct FieldTargets {
    template: PathBuf,
    plugin: PathBuf,
    component: PathBuf,
}

/// Refuse an unknown base type, naming the allowed ones.
fn validate_base_type(base_type: &str) -> Result<()> {
    if VALID_BASE_TYPES.contains(&base_type) {
        return Ok(());
    }

    bail!(
        "invalid base type '{}' (allowed: {})",
        base_type,
        VALID_BASE_TYPES.join(", ")
    );
}

/// Refuse a name whose template would shadow a built-in field template:
/// `templates/fields/<name>.hbs` overlays the built-in of that name, so every
/// field of that built-in type would render the scaffold instead.
fn refuse_builtin_field_name(name: &str) -> Result<()> {
    if EMBEDDED_TEMPLATES
        .get_file(format!("fields/{name}.hbs"))
        .is_none()
    {
        return Ok(());
    }

    bail!(
        "'{name}' is a built-in field template -- a field named that would replace the \
         template of every '{name}' field; pick another name"
    );
}

/// Resolve the three targets and check all of them before any is written, so
/// a refusal — an existing file without `--force`, or a name that makes no
/// valid component tag — leaves nothing half-scaffolded.
fn field_targets(opts: &MakeFieldOptions, component_tag: &str) -> Result<FieldTargets> {
    let targets = FieldTargets {
        template: paths::templates_fields_dir(opts.config_dir).join(format!("{}.hbs", opts.name)),
        plugin: paths::plugins_dir(opts.config_dir).join(format!("{}.lua", opts.name)),
        component: component_path(opts.config_dir, component_tag)?,
    };

    for path in [&targets.template, &targets.plugin, &targets.component] {
        refuse_file_overwrite(path, opts.force)?;
    }

    Ok(targets)
}

/// Write `content` to `path`, creating its directory.
fn write_target(path: &Path, content: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("Failed to create {}", dir.display()))?;
    }

    fs::write(path, content).with_context(|| format!("Failed to write {}", path.display()))
}

/// Scaffold the three files.
///
/// # Errors
///
/// Returns an error if the name is invalid, the base type is unknown, any
/// target file already exists without `--force`, or writing fails. Every
/// refusal comes before the first file is written.
pub fn make_field(opts: &MakeFieldOptions) -> Result<()> {
    validate_slug(opts.name)?;
    refuse_builtin_field_name(opts.name)?;

    let base_type = opts.base_type.unwrap_or("number");
    validate_base_type(base_type)?;

    let label = to_title_case(opts.name);
    let component_tag = format!("crap-{}", opts.name);
    let targets = field_targets(opts, &component_tag)?;

    // Render before writing, so a render failure leaves nothing behind either.
    let template = render_template_hbs(opts.name, &component_tag)?;
    let plugin = render_plugin_lua(opts.name, base_type, &label)?;

    write_target(&targets.template, &template)?;
    write_target(&targets.plugin, &plugin)?;

    // The Web Component reuses the make_component generator so the skeleton
    // stays consistent with `make component`.
    component::make_component(&component::MakeComponentOptions {
        config_dir: opts.config_dir,
        tag: &component_tag,
        force: opts.force,
    })?;

    print_field_usage(opts.name);

    Ok(())
}

/// Tell the user the field exists and how to use it in a collection.
fn print_field_usage(name: &str) {
    cli::success(&format!(
        "Created field '{name}' -- three files wired together via admin.template."
    ));
    cli::info(&format!(
        "\nUse it in a collection:\n\n  local {name} = require(\"plugins.{name}\")\n\n  crap.collections.define(\"products\", {{\n    fields = {{\n      {name}.field({{ name = \"my_{name}\" }}),\n      ...\n    }},\n  }})",
    ));
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

    /// Regression: each of the three files was checked just before it was
    /// written, so an existing component left a template and a plugin behind.
    #[test]
    fn an_existing_component_refuses_before_anything_is_written() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let comp_dir = tmp.path().join("static/components");
        fs::create_dir_all(&comp_dir).unwrap();
        fs::write(comp_dir.join("crap-rating.js"), "// mine").unwrap();

        let err = make_field(&MakeFieldOptions {
            config_dir: tmp.path(),
            name: "rating",
            base_type: None,
            force: false,
        })
        .unwrap_err();

        assert!(err.to_string().contains("already exists"), "{err}");
        assert!(!tmp.path().join("templates/fields/rating.hbs").exists());
        assert!(!tmp.path().join("plugins/rating.lua").exists());
    }

    /// A name that is a valid slug but no valid component tag (`_` is not
    /// allowed in a custom-element name) is refused before any file exists.
    #[test]
    fn a_name_without_a_valid_component_tag_writes_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");

        let result = make_field(&MakeFieldOptions {
            config_dir: tmp.path(),
            name: "star_rating",
            base_type: None,
            force: false,
        });

        assert!(result.is_err());
        assert!(!tmp.path().join("templates/fields/star_rating.hbs").exists());
        assert!(!tmp.path().join("plugins/star_rating.lua").exists());
    }

    /// Regression: `make field code` wrote `templates/fields/code.hbs`, which
    /// replaced the template of every built-in `code` field (and its
    /// `crap-code` element collided with the built-in one).
    #[test]
    fn a_built_in_field_name_is_refused_before_anything_is_written() {
        let tmp = tempfile::tempdir().expect("tempdir");

        let err = make_field(&MakeFieldOptions {
            config_dir: tmp.path(),
            name: "code",
            base_type: None,
            force: false,
        })
        .unwrap_err()
        .to_string();

        assert!(err.contains("built-in field template"), "{err}");
        assert!(!tmp.path().join("templates/fields/code.hbs").exists());
        assert!(!tmp.path().join("plugins/code.lua").exists());
    }
}
