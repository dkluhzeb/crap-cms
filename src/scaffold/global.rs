//! `make global` command — generate global Lua files.

use anyhow::{Context as _, Result, bail};
use std::{fs, path::Path};

/// Generate a global Lua file at `<config_dir>/globals/<slug>.lua`.
///
/// Accepts pre-parsed field stubs or `None` for defaults.
pub fn make_global(
    config_dir: &Path,
    slug: &str,
    fields: Option<&[super::collection::FieldStub]>,
    force: bool,
) -> Result<()> {
    super::validate_slug(slug)?;

    let globals_dir = config_dir.join("globals");
    fs::create_dir_all(&globals_dir).context("Failed to create globals/ directory")?;

    let file_path = globals_dir.join(format!("{}.lua", slug));

    if file_path.exists() && !force {
        bail!(
            "File '{}' already exists — use --force to overwrite",
            file_path.display()
        );
    }

    let label = super::to_title_case(slug);

    let default_fields;
    let fields = match fields {
        Some(f) => f,
        None => {
            default_fields = [super::collection::FieldStub {
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

    let mut lua = String::new();
    lua.push_str(&format!("crap.globals.define(\"{}\", {{\n", slug));
    lua.push_str("    labels = {\n");
    lua.push_str(&format!("        singular = \"{}\",\n", label));
    lua.push_str("    },\n");
    lua.push_str("    fields = {\n");

    for field in fields {
        super::collection::write_field_lua(&mut lua, field, 8);
    }

    lua.push_str("    },\n");
    lua.push_str("    -- access = {\n");
    lua.push_str("    --     read   = \"access.anyone\",\n");
    lua.push_str("    --     update = \"access.admin_only\",\n");
    lua.push_str("    -- },\n");
    lua.push_str("})\n");

    fs::write(&file_path, &lua)
        .with_context(|| format!("Failed to write {}", file_path.display()))?;

    println!("Created {}", file_path.display());

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_make_global() {
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
    fn test_make_global_access_block() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_global(tmp.path(), "nav", None, false).unwrap();

        let content = fs::read_to_string(tmp.path().join("globals/nav.lua")).unwrap();
        assert!(content.contains("-- access = {"));
        assert!(content.contains("--     read   = \"access.anyone\""));
        assert!(content.contains("--     update = \"access.admin_only\""));
    }

    #[test]
    fn test_make_global_with_fields() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fields =
            crate::scaffold::collection::parse_fields_shorthand("title:text:required,links:array")
                .unwrap();
        make_global(tmp.path(), "nav", Some(&fields), false).unwrap();

        let content = fs::read_to_string(tmp.path().join("globals/nav.lua")).unwrap();
        assert!(content.contains("crap.fields.text({"));
        assert!(content.contains("name = \"title\""));
        assert!(content.contains("crap.fields.array({"));
        assert!(content.contains("name = \"links\""));
    }

    #[test]
    fn test_make_global_refuses_overwrite() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_global(tmp.path(), "nav", None, false).unwrap();
        let result = make_global(tmp.path(), "nav", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("--force"));
    }

    #[test]
    fn test_make_global_force_overwrite() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_global(tmp.path(), "nav", None, false).unwrap();
        assert!(make_global(tmp.path(), "nav", None, true).is_ok());
    }

    #[test]
    fn test_make_global_invalid_slug() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let result = make_global(tmp.path(), "Bad Slug", None, false);
        assert!(result.is_err());
    }

    #[test]
    fn test_make_global_with_nested_group() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fields = crate::scaffold::collection::parse_fields_shorthand(
            "seo:group(meta_title:text:required,meta_desc:textarea)",
        )
        .unwrap();
        make_global(tmp.path(), "site_seo", Some(&fields), false).unwrap();

        let content = fs::read_to_string(tmp.path().join("globals/site_seo.lua")).unwrap();
        assert!(content.contains("crap.globals.define(\"site_seo\""));
        assert!(content.contains("crap.fields.group({"));
        assert!(content.contains("name = \"seo\""));
        assert!(content.contains("name = \"meta_title\""));
        assert!(content.contains("required = true"));
        assert!(content.contains("name = \"meta_desc\""));
    }

    #[test]
    fn test_make_global_all_containers() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fields = crate::scaffold::collection::parse_fields_shorthand(
            "items:array(label:text),meta:group(key:text),layout:blocks(hero|Hero(title:text)),panels:tabs(General(name:text))",
        )
        .unwrap();
        make_global(tmp.path(), "kitchen_sink", Some(&fields), false).unwrap();

        let content = fs::read_to_string(tmp.path().join("globals/kitchen_sink.lua")).unwrap();
        assert!(content.contains("crap.fields.array({"));
        assert!(content.contains("crap.fields.group({"));
        assert!(content.contains("crap.fields.blocks({"));
        assert!(content.contains("crap.fields.tabs({"));
    }
}
