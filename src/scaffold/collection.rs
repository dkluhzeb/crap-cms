//! `make collection` command — generate collection Lua files.

use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

/// Valid field types for collection definitions.
pub const VALID_FIELD_TYPES: &[&str] = &[
    "text", "number", "textarea", "select", "checkbox", "date",
    "email", "json", "richtext", "relationship", "array", "group",
    "upload", "blocks",
];

struct FieldStub {
    name: String,
    field_type: String,
    required: bool,
    localized: bool,
}

/// Generate a collection Lua file at `<config_dir>/collections/<slug>.lua`.
///
/// Optionally accepts inline field shorthand (e.g., "title:text:required,body:textarea").
pub fn make_collection(
    config_dir: &Path,
    slug: &str,
    fields_shorthand: Option<&str>,
    no_timestamps: bool,
    auth: bool,
    upload: bool,
    versions: bool,
    force: bool,
) -> Result<()> {
    super::validate_slug(slug)?;

    let collections_dir = config_dir.join("collections");
    fs::create_dir_all(&collections_dir)
        .context("Failed to create collections/ directory")?;

    let file_path = collections_dir.join(format!("{}.lua", slug));
    if file_path.exists() && !force {
        anyhow::bail!(
            "File '{}' already exists — use --force to overwrite",
            file_path.display()
        );
    }

    let singular_slug = singularize(slug);
    let label_singular = super::to_title_case(&singular_slug);
    let label_plural = pluralize(&label_singular);
    let timestamps = if no_timestamps { "false" } else { "true" };

    let fields = match fields_shorthand {
        Some(s) => parse_fields_shorthand(s)?,
        None if upload => vec![FieldStub {
            name: "alt".to_string(),
            field_type: "text".to_string(),
            required: false,
            localized: false,
        }],
        None => vec![FieldStub {
            name: "title".to_string(),
            field_type: "text".to_string(),
            required: true,
            localized: false,
        }],
    };

    let title_field = fields.first().map(|f| f.name.as_str()).unwrap_or("title");

    let mut lua = String::new();
    lua.push_str(&format!("crap.collections.define(\"{}\", {{\n", slug));
    lua.push_str("    labels = {\n");
    lua.push_str(&format!("        singular = \"{}\",\n", label_singular));
    lua.push_str(&format!("        plural = \"{}\",\n", label_plural));
    lua.push_str("    },\n");
    lua.push_str(&format!("    timestamps = {},\n", timestamps));
    if auth {
        lua.push_str("    auth = true,\n");
    }
    if upload {
        lua.push_str("    upload = true,\n");
    }
    if versions {
        lua.push_str("    versions = true,\n");
    }
    lua.push_str("    admin = {\n");
    lua.push_str(&format!("        use_as_title = \"{}\",\n",
        if auth { "email" } else { title_field }));
    lua.push_str("    },\n");
    lua.push_str("    fields = {\n");

    for field in &fields {
        lua.push_str("        {\n");
        lua.push_str(&format!("            name = \"{}\",\n", field.name));
        lua.push_str(&format!("            type = \"{}\",\n", field.field_type));
        if field.required {
            lua.push_str("            required = true,\n");
        }
        if field.localized {
            lua.push_str("            localized = true,\n");
        }
        lua.push_str("        },\n");
    }

    lua.push_str("    },\n");
    lua.push_str("})\n");

    fs::write(&file_path, &lua)
        .with_context(|| format!("Failed to write {}", file_path.display()))?;

    println!("Created {}", file_path.display());

    Ok(())
}

/// Naive English singularization: strip trailing "s", "es", or "ies" → "y".
fn singularize(s: &str) -> String {
    let lower = s.to_lowercase();
    if lower.ends_with("ies") && lower.len() > 3 {
        format!("{}y", &s[..s.len() - 3])
    } else if lower.ends_with("ses") || lower.ends_with("xes") || lower.ends_with("zes")
        || lower.ends_with("shes") || lower.ends_with("ches")
    {
        s[..s.len() - 2].to_string()
    } else if lower.ends_with('s') && !lower.ends_with("ss") && lower.len() > 1 {
        s[..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

/// Naive English pluralization: add "s" (or "es" for sibilants, "ies" for consonant+y).
fn pluralize(s: &str) -> String {
    if s.is_empty() {
        return s.to_string();
    }
    let lower = s.to_lowercase();
    if lower.ends_with("s") || lower.ends_with("x") || lower.ends_with("z")
        || lower.ends_with("sh") || lower.ends_with("ch")
    {
        format!("{}es", s)
    } else if lower.ends_with("y")
        && !lower.ends_with("ay")
        && !lower.ends_with("ey")
        && !lower.ends_with("oy")
        && !lower.ends_with("uy")
    {
        format!("{}ies", &s[..s.len() - 1])
    } else {
        format!("{}s", s)
    }
}

/// Parse inline field shorthand: "title:text:required,status:select,body:textarea:localized"
///
/// Modifiers after the type are order-independent flags: `required`, `localized`.
/// E.g., `"title:text:required:localized"` or `"title:text:localized:required"`.
fn parse_fields_shorthand(s: &str) -> Result<Vec<FieldStub>> {

    let mut fields = Vec::new();
    for part in s.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let segments: Vec<&str> = part.split(':').collect();
        if segments.len() < 2 {
            anyhow::bail!(
                "Invalid field shorthand '{}' — expected 'name:type[:required][:localized]'",
                part
            );
        }
        let name = segments[0].to_string();
        let field_type = segments[1].to_lowercase();
        if !VALID_FIELD_TYPES.contains(&field_type.as_str()) {
            anyhow::bail!(
                "Unknown field type '{}' — valid types: {}",
                field_type,
                VALID_FIELD_TYPES.join(", ")
            );
        }
        let mut required = false;
        let mut localized = false;
        for seg in &segments[2..] {
            match *seg {
                "required" => required = true,
                "localized" => localized = true,
                other => anyhow::bail!(
                    "Unknown modifier '{}' in field '{}' — valid: required, localized",
                    other, name
                ),
            }
        }
        fields.push(FieldStub { name, field_type, required, localized });
    }

    if fields.is_empty() {
        anyhow::bail!("No fields parsed from shorthand");
    }

    Ok(fields)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_pluralize() {
        assert_eq!(pluralize("Post"), "Posts");
        assert_eq!(pluralize("Category"), "Categories");
        assert_eq!(pluralize("Tag"), "Tags");
        assert_eq!(pluralize("Address"), "Addresses");
        assert_eq!(pluralize("Box"), "Boxes");
        assert_eq!(pluralize("Key"), "Keys");
    }

    #[test]
    fn test_parse_fields_shorthand() {
        let fields = parse_fields_shorthand("title:text:required,body:textarea,published:checkbox").unwrap();
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0].name, "title");
        assert_eq!(fields[0].field_type, "text");
        assert!(fields[0].required);
        assert!(!fields[0].localized);
        assert_eq!(fields[1].name, "body");
        assert_eq!(fields[1].field_type, "textarea");
        assert!(!fields[1].required);
        assert_eq!(fields[2].name, "published");
        assert_eq!(fields[2].field_type, "checkbox");
    }

    #[test]
    fn test_parse_fields_shorthand_localized() {
        // localized only
        let fields = parse_fields_shorthand("title:text:localized").unwrap();
        assert_eq!(fields[0].name, "title");
        assert!(!fields[0].required);
        assert!(fields[0].localized);

        // required + localized (order 1)
        let fields = parse_fields_shorthand("title:text:required:localized").unwrap();
        assert!(fields[0].required);
        assert!(fields[0].localized);

        // localized + required (order 2)
        let fields = parse_fields_shorthand("title:text:localized:required").unwrap();
        assert!(fields[0].required);
        assert!(fields[0].localized);

        // mixed fields: one localized, one not
        let fields = parse_fields_shorthand("title:text:required:localized,slug:text:required").unwrap();
        assert_eq!(fields.len(), 2);
        assert!(fields[0].localized);
        assert!(!fields[1].localized);
    }

    #[test]
    fn test_parse_fields_shorthand_invalid() {
        assert!(parse_fields_shorthand("title").is_err());
        assert!(parse_fields_shorthand("title:unknown").is_err());
        assert!(parse_fields_shorthand("").is_err());
        // unknown modifier
        assert!(parse_fields_shorthand("title:text:bogus").is_err());
    }

    #[test]
    fn test_make_collection_default() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_collection(tmp.path(), "posts", None, false, false, false, false, false).unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/posts.lua")).unwrap();
        assert!(content.contains("crap.collections.define(\"posts\""));
        assert!(content.contains("singular = \"Post\""));
        assert!(content.contains("plural = \"Posts\""));
        assert!(content.contains("timestamps = true"));
        assert!(content.contains("name = \"title\""));
        assert!(content.contains("type = \"text\""));
        assert!(content.contains("required = true"));
    }

    #[test]
    fn test_make_collection_with_fields() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_collection(
            tmp.path(), "articles",
            Some("headline:text:required,body:richtext,draft:checkbox"),
            true, false, false, false, false,
        ).unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/articles.lua")).unwrap();
        assert!(content.contains("timestamps = false"));
        assert!(content.contains("name = \"headline\""));
        assert!(content.contains("name = \"body\""));
        assert!(content.contains("type = \"richtext\""));
        assert!(content.contains("name = \"draft\""));
        assert!(content.contains("use_as_title = \"headline\""));
    }

    #[test]
    fn test_singularize() {
        // ies -> y
        assert_eq!(singularize("categories"), "category");
        assert_eq!(singularize("stories"), "story");
        // ses, xes, zes, shes, ches
        assert_eq!(singularize("addresses"), "address");
        assert_eq!(singularize("boxes"), "box");
        assert_eq!(singularize("buzzes"), "buzz");
        assert_eq!(singularize("dishes"), "dish");
        assert_eq!(singularize("watches"), "watch");
        // regular s
        assert_eq!(singularize("posts"), "post");
        assert_eq!(singularize("tags"), "tag");
        // no change (doesn't end in s, or ends in ss)
        assert_eq!(singularize("address"), "address");
        assert_eq!(singularize("glass"), "glass");
        // single char
        assert_eq!(singularize("s"), "s");
    }

    #[test]
    fn test_pluralize_more_cases() {
        assert_eq!(pluralize(""), "");
        assert_eq!(pluralize("Quiz"), "Quizes");
        assert_eq!(pluralize("Brush"), "Brushes");
        assert_eq!(pluralize("Church"), "Churches");
        assert_eq!(pluralize("Boy"), "Boys");
        assert_eq!(pluralize("Day"), "Days");
        assert_eq!(pluralize("Toy"), "Toys");
        assert_eq!(pluralize("Guy"), "Guys");
    }

    #[test]
    fn test_make_collection_auth() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_collection(tmp.path(), "users", None, false, true, false, false, false).unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/users.lua")).unwrap();
        assert!(content.contains("auth = true"));
        assert!(content.contains("use_as_title = \"email\""));
    }

    #[test]
    fn test_make_collection_upload() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_collection(tmp.path(), "media", None, false, false, true, false, false).unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/media.lua")).unwrap();
        assert!(content.contains("upload = true"));
        assert!(content.contains("name = \"alt\""));
    }

    #[test]
    fn test_make_collection_versions() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_collection(tmp.path(), "posts", None, false, false, false, true, false).unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/posts.lua")).unwrap();
        assert!(content.contains("versions = true"));
    }

    #[test]
    fn test_make_collection_with_localized_fields() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_collection(
            tmp.path(), "posts",
            Some("title:text:required:localized,body:textarea:localized"),
            false, false, false, false, false,
        ).unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/posts.lua")).unwrap();
        assert!(content.contains("localized = true"));
    }

    #[test]
    fn test_make_collection_invalid_slug() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let result = make_collection(tmp.path(), "Bad Slug", None, false, false, false, false, false);
        assert!(result.is_err());
    }

    #[test]
    fn test_make_collection_refuses_overwrite() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_collection(tmp.path(), "posts", None, false, false, false, false, false).unwrap();
        let result = make_collection(tmp.path(), "posts", None, false, false, false, false, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("--force"));
    }

    #[test]
    fn test_make_collection_force_overwrite() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_collection(tmp.path(), "posts", None, false, false, false, false, false).unwrap();
        assert!(make_collection(tmp.path(), "posts", None, false, false, false, false, true).is_ok());
    }
}
