//! `make field` — scaffold a per-field render template binding via
//! `admin.template = "fields/<name>"`. Three files:
//!
//!   1. `templates/fields/<name>.hbs` — the per-field template.
//!   2. `plugins/<name>.lua` — a `field()` factory wrapping
//!      `crap.fields.<base_type>` and pre-setting `admin.template`.
//!   3. `static/components/<name>.js` — Web Component skeleton.
//!
//! Plus a printed snippet for how to register the component in
//! `custom.js`. Files are coordinated: the field name is the same
//! across all three so the binding is consistent.

use std::{fs, path::Path};

use anyhow::{Context as _, Result, bail};

use crate::{
    cli,
    scaffold::{component, to_title_case, validate_slug},
};

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
    let tpl_dir = opts.config_dir.join("templates").join("fields");
    fs::create_dir_all(&tpl_dir).context("Failed to create templates/fields/ directory")?;
    let tpl_path = tpl_dir.join(format!("{}.hbs", opts.name));
    if tpl_path.exists() && !opts.force {
        bail!(
            "File '{}' already exists — use --force to overwrite",
            tpl_path.display()
        );
    }
    fs::write(&tpl_path, render_template_hbs(opts.name, &component_tag))
        .with_context(|| format!("Failed to write {}", tpl_path.display()))?;

    // 2. Lua plugin wrapper
    let plug_dir = opts.config_dir.join("plugins");
    fs::create_dir_all(&plug_dir).context("Failed to create plugins/ directory")?;
    let plug_path = plug_dir.join(format!("{}.lua", opts.name));
    if plug_path.exists() && !opts.force {
        bail!(
            "File '{}' already exists — use --force to overwrite",
            plug_path.display()
        );
    }
    fs::write(&plug_path, render_plugin_lua(opts.name, base_type, &label))
        .with_context(|| format!("Failed to write {}", plug_path.display()))?;

    // 3. Web Component — reuse the make_component generator so the
    //    skeleton stays consistent with `make component`.
    component::make_component(&component::MakeComponentOptions {
        config_dir: opts.config_dir,
        tag: &component_tag,
        force: opts.force,
    })?;

    cli::success(&format!(
        "Created field '{}' — three files wired together via admin.template.",
        opts.name
    ));
    cli::info(&format!(
        "\nUse it in a collection:\n\n  local {name} = require(\"plugins.{name}\")\n\n  crap.collections.define(\"products\", {{\n    fields = {{\n      {name}.field({{ name = \"my_{name}\" }}),\n      ...\n    }},\n  }})",
        name = opts.name,
    ));

    Ok(())
}

fn render_template_hbs(name: &str, tag: &str) -> String {
    format!(
        r#"{{{{!--
  Per-field render template for fields configured with
  `admin.template = "fields/{name}"` (see `plugins/{name}.lua`).

  Reads optional config from `{{{{extra.<key>}}}}`; the wrapper plugin
  populates `admin.extra` with whatever per-field knobs you want
  (color, icon, max value, etc.).

  Standard `partials/field` chrome wraps the input — label, required
  marker, error, help text — same as built-in field types.
--}}}}
{{{{#> partials/field}}}}
  <{tag}
    id="field-{{{{name}}}}"
    name="{{{{name}}}}"
    value="{{{{value}}}}"
    {{{{#if has_min}}}}data-min="{{{{min}}}}"{{{{/if}}}}
    {{{{#if has_max}}}}data-max="{{{{max}}}}"{{{{/if}}}}
    {{{{#if extra.color}}}}data-color="{{{{extra.color}}}}"{{{{/if}}}}
    {{{{#if required}}}}required{{{{/if}}}}
    {{{{#if readonly}}}}readonly{{{{/if}}}}
    {{{{#if error}}}}data-error{{{{/if}}}}
  ></{tag}>
{{{{/partials/field}}}}
"#,
        name = name,
        tag = tag,
    )
}

fn render_plugin_lua(name: &str, base_type: &str, label: &str) -> String {
    format!(
        r#"--- {label} field plugin: pre-configures `crap.fields.{base_type}` with
--- `admin.template = "fields/{name}"` so a single-instance per-field
--- template (`templates/fields/{name}.hbs`) and a Web Component
--- (`static/components/crap-{name}.js`) render the field's UI.
---
--- Storage stays as `{base_type}` — validation, SQL schema, and list-view
--- formatting all flow through the built-in type. Only the admin
--- rendering is custom.
---
--- Use it in a collection like:
---
---   local {name} = require("plugins.{name}")
---
---   crap.collections.define("products", {{
---     fields = {{
---       {name}.field({{ name = "my_{name}" }}),
---     }},
---   }})

local M = {{}}

---@param opts table?
---@return table
function M.field(opts)
  opts = opts or {{}}
  local admin = opts.admin or {{}}

  -- Per-instance template binding. The Web Component
  -- (`<crap-{name}>` registered via `static/components/custom.js`)
  -- handles the actual UI; this template wraps it in the standard
  -- field chrome.
  admin.template = "fields/{name}"
  admin.extra = admin.extra or {{}}

  -- Default extras passed to the template as `{{{{extra.<key>}}}}`.
  -- Override per-field by setting `admin.extra.color = "..."` in the
  -- collection definition.
  if admin.extra.color == nil then
    admin.extra.color = "amber"
  end

  return crap.fields.{base_type}({{
    name = opts.name or "{name}",
    required = opts.required,
    default_value = opts.default_value,
    admin = admin,
  }})
end

return M
"#,
        name = name,
        base_type = base_type,
        label = label,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
