//! `make global` — generate global Lua files.

use std::{fs, path::Path};

use anyhow::{Context as _, Result, bail};
use serde_json::json;

use crate::cli;
use crate::scaffold::{
    FieldStub, collection::write_field_lua, render::render, to_title_case, validate_slug,
};

/// Generate a global Lua file at `<config_dir>/globals/<slug>.lua`.
///
/// Accepts pre-parsed field stubs or `None` for defaults.
pub fn make_global(
    config_dir: &Path,
    slug: &str,
    fields: Option<&[FieldStub]>,
    force: bool,
) -> Result<()> {
    validate_slug(slug)?;

    let globals_dir = config_dir.join("globals");
    fs::create_dir_all(&globals_dir).context("Failed to create globals/ directory")?;

    let file_path = globals_dir.join(format!("{}.lua", slug));

    if file_path.exists() && !force {
        bail!(
            "File '{}' already exists — use --force to overwrite",
            file_path.display()
        );
    }

    let lua = render_global_lua(slug, fields)?;

    fs::write(&file_path, &lua)
        .with_context(|| format!("Failed to write {}", file_path.display()))?;

    cli::success(&format!("Created {}", file_path.display()));

    Ok(())
}

/// Render the full global Lua definition via Handlebars.
fn render_global_lua(slug: &str, fields: Option<&[FieldStub]>) -> Result<String> {
    let label = to_title_case(slug);

    let default_fields;
    let fields = match fields {
        Some(f) => f,
        None => {
            default_fields = [FieldStub {
                name: "title".to_string(),
                field_type: "text".to_string(),
                required: true,
                localized: false,
                fields: vec![],
                blocks: vec![],
                tabs: vec![],
            }];
            &default_fields
        }
    };

    let mut fields_lua = String::new();
    for field in fields {
        write_field_lua(&mut fields_lua, field, 8);
    }

    render(
        "global",
        &json!({
            "slug": slug,
            "label": label,
            "fields_lua": fields_lua,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scaffold::collection::parse_fields_shorthand;

    #[test]
    fn make_default() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_global(tmp.path(), "site_settings", None, false).unwrap();

        let content = fs::read_to_string(tmp.path().join("globals/site_settings.lua")).unwrap();
        assert!(content.contains("crap.globals.define(\"site_settings\""));
        assert!(content.contains("singular = \"Site Settings\""));
        assert!(content.contains("crap.fields.text({"));
        assert!(content.contains("name = \"title\""));
        assert!(content.contains("required = true"));
    }

    #[test]
    fn access_block() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_global(tmp.path(), "nav", None, false).unwrap();

        let content = fs::read_to_string(tmp.path().join("globals/nav.lua")).unwrap();
        assert!(content.contains("-- access = {"));
        assert!(content.contains("--     read   = \"access.anyone\""));
        assert!(content.contains("--     update = \"access.admin_only\""));
    }

    #[test]
    fn with_fields() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fields = parse_fields_shorthand("title:text:required,links:array").unwrap();
        make_global(tmp.path(), "nav", Some(&fields), false).unwrap();

        let content = fs::read_to_string(tmp.path().join("globals/nav.lua")).unwrap();
        assert!(content.contains("name = \"title\""));
        assert!(content.contains("crap.fields.array({"));
    }

    #[test]
    fn refuses_overwrite() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_global(tmp.path(), "nav", None, false).unwrap();
        let result = make_global(tmp.path(), "nav", None, false);
        assert!(result.unwrap_err().to_string().contains("--force"));
    }

    #[test]
    fn force_overwrite() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_global(tmp.path(), "nav", None, false).unwrap();
        assert!(make_global(tmp.path(), "nav", None, true).is_ok());
    }

    #[test]
    fn invalid_slug() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(make_global(tmp.path(), "Bad Slug", None, false).is_err());
    }

    #[test]
    fn with_nested_group() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fields =
            parse_fields_shorthand("seo:group(meta_title:text:required,meta_desc:textarea)")
                .unwrap();
        make_global(tmp.path(), "site_seo", Some(&fields), false).unwrap();

        let content = fs::read_to_string(tmp.path().join("globals/site_seo.lua")).unwrap();
        assert!(content.contains("crap.fields.group({"));
        assert!(content.contains("name = \"seo\""));
    }

    #[test]
    fn all_containers() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fields = parse_fields_shorthand(
            "items:array(label:text),meta:group(key:text),layout:blocks(hero|Hero(title:text)),panels:tabs(General(name:text))",
        ).unwrap();
        make_global(tmp.path(), "kitchen_sink", Some(&fields), false).unwrap();

        let content = fs::read_to_string(tmp.path().join("globals/kitchen_sink.lua")).unwrap();
        assert!(content.contains("crap.fields.array({"));
        assert!(content.contains("crap.fields.group({"));
        assert!(content.contains("crap.fields.blocks({"));
        assert!(content.contains("crap.fields.tabs({"));
    }
}
