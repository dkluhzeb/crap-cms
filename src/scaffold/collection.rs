//! `make collection` command — generate collection Lua files.

use anyhow::{Context as _, Result};
use std::fs;
use std::path::Path;

/// Valid field types for collection definitions.
pub const VALID_FIELD_TYPES: &[&str] = &[
    "text",
    "number",
    "textarea",
    "select",
    "radio",
    "checkbox",
    "date",
    "email",
    "json",
    "richtext",
    "code",
    "relationship",
    "array",
    "group",
    "upload",
    "blocks",
    "row",
    "collapsible",
    "tabs",
    "join",
];

/// Boolean flags for collection scaffolding.
pub struct CollectionOptions {
    pub no_timestamps: bool,
    pub auth: bool,
    pub upload: bool,
    pub versions: bool,
    pub force: bool,
}

impl CollectionOptions {
    pub fn new() -> Self {
        Self {
            no_timestamps: false,
            auth: false,
            upload: false,
            versions: false,
            force: false,
        }
    }
}

impl Default for CollectionOptions {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) struct FieldStub {
    pub(crate) name: String,
    pub(crate) field_type: String,
    pub(crate) required: bool,
    pub(crate) localized: bool,
}

/// Generate a collection Lua file at `<config_dir>/collections/<slug>.lua`.
///
/// Optionally accepts inline field shorthand (e.g., "title:text:required,body:textarea").
pub fn make_collection(
    config_dir: &Path,
    slug: &str,
    fields_shorthand: Option<&str>,
    opts: &CollectionOptions,
) -> Result<()> {
    super::validate_slug(slug)?;

    let collections_dir = config_dir.join("collections");
    fs::create_dir_all(&collections_dir).context("Failed to create collections/ directory")?;

    let file_path = collections_dir.join(format!("{}.lua", slug));
    if file_path.exists() && !opts.force {
        anyhow::bail!(
            "File '{}' already exists — use --force to overwrite",
            file_path.display()
        );
    }

    let singular_slug = singularize(slug);
    let label_singular = super::to_title_case(&singular_slug);
    let label_plural = pluralize(&label_singular);
    let timestamps = if opts.no_timestamps { "false" } else { "true" };

    let fields = match fields_shorthand {
        Some(s) => parse_fields_shorthand(s)?,
        None if opts.upload => vec![FieldStub {
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
    if opts.auth {
        lua.push_str("    auth = true,\n");
        lua.push_str("    -- Full auth config (uncomment and customize):\n");
        lua.push_str("    -- auth = {\n");
        lua.push_str("    --     verify_email = false,\n");
        lua.push_str("    --     strategies = {},\n");
        lua.push_str("    -- },\n");
    }
    if opts.upload {
        lua.push_str("    upload = true,\n");
        lua.push_str("    -- Full upload config (uncomment and customize):\n");
        lua.push_str("    -- upload = {\n");
        lua.push_str("    --     mime_types = { \"image/*\", \"application/pdf\" },\n");
        lua.push_str("    --     max_file_size = \"50MB\",\n");
        lua.push_str("    --     image_sizes = {\n");
        lua.push_str("    --         { name = \"thumbnail\", width = 300, height = 300, fit = \"cover\" },\n");
        lua.push_str("    --     },\n");
        lua.push_str("    -- },\n");
    }
    if opts.versions {
        lua.push_str("    versions = true,\n");
    }
    let use_as_title = if opts.auth { "email" } else { title_field };
    lua.push_str("    admin = {\n");
    lua.push_str(&format!("        use_as_title = \"{}\",\n", use_as_title));
    if !opts.no_timestamps {
        lua.push_str("        default_sort = \"-created_at\",\n");
    }
    lua.push_str(&format!(
        "        list_searchable_fields = {{ \"{}\" }},\n",
        use_as_title
    ));
    lua.push_str("    },\n");
    lua.push_str("    fields = {\n");

    for field in &fields {
        lua.push_str(&format!("        crap.fields.{}({{\n", field.field_type));
        lua.push_str(&format!("            name = \"{}\",\n", field.name));
        if field.required {
            lua.push_str("            required = true,\n");
        }
        if field.localized {
            lua.push_str("            localized = true,\n");
        }
        if let Some(stub) = type_specific_stub(&field.field_type) {
            lua.push_str(stub);
        }
        lua.push_str("        }),\n");
    }

    lua.push_str("    },\n");
    lua.push_str("    -- access = {\n");
    lua.push_str("    --     read   = \"access.anyone\",\n");
    lua.push_str("    --     create = \"access.authenticated\",\n");
    lua.push_str("    --     update = \"access.authenticated\",\n");
    lua.push_str("    --     delete = \"access.admin_only\",\n");
    lua.push_str("    -- },\n");
    lua.push_str("    -- indexes = {\n");
    lua.push_str("    --     { fields = { \"status\", \"created_at\" } },\n");
    lua.push_str("    --     { fields = { \"slug\" }, unique = true },\n");
    lua.push_str("    -- },\n");
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
    } else if lower.ends_with("ses")
        || lower.ends_with("xes")
        || lower.ends_with("zes")
        || lower.ends_with("shes")
        || lower.ends_with("ches")
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
    if lower.ends_with("z") && !lower.ends_with("zz") {
        // Single trailing z doubles before -es: Quiz → Quizzes
        format!("{}zes", s)
    } else if lower.ends_with("s")
        || lower.ends_with("x")
        || lower.ends_with("zz")
        || lower.ends_with("sh")
        || lower.ends_with("ch")
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

/// Return type-specific Lua stub lines for complex field types.
pub(crate) fn type_specific_stub(field_type: &str) -> Option<&'static str> {
    match field_type {
        "select" | "radio" => {
            Some("            options = { { label = \"Option 1\", value = \"option_1\" } },\n")
        }
        "relationship" => Some("            relationship = { collection = \"TODO\" },\n"),
        "upload" => Some("            relationship = { collection = \"media\" },\n"),
        "array" => Some("            fields = { crap.fields.text({ name = \"item\" }) },\n"),
        "blocks" => Some(
            "            blocks = { { type = \"block_type\", label = \"Block\", fields = { crap.fields.text({ name = \"content\" }) } } },\n",
        ),
        "group" | "collapsible" | "row" => {
            Some("            fields = { crap.fields.text({ name = \"item\" }) },\n")
        }
        "tabs" => Some(
            "            tabs = { { label = \"Tab 1\", fields = { crap.fields.text({ name = \"item\" }) } } },\n",
        ),
        "join" => Some("            collection = \"TODO\",\n            on = \"TODO\",\n"),
        "code" => Some("            admin = { language = \"javascript\" },\n"),
        _ => None,
    }
}

/// Parse inline field shorthand: "title:text:required,status:select,body:textarea:localized"
///
/// Modifiers after the type are order-independent flags: `required`, `localized`.
/// E.g., `"title:text:required:localized"` or `"title:text:localized:required"`.
pub(crate) fn parse_fields_shorthand(s: &str) -> Result<Vec<FieldStub>> {
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
                "index" => {} // accepted but not stored in FieldStub (handled at Lua level)
                other => anyhow::bail!(
                    "Unknown modifier '{}' in field '{}' — valid: required, localized, index",
                    other,
                    name
                ),
            }
        }
        fields.push(FieldStub {
            name,
            field_type,
            required,
            localized,
        });
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
        let fields =
            parse_fields_shorthand("title:text:required,body:textarea,published:checkbox").unwrap();
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
        let fields =
            parse_fields_shorthand("title:text:required:localized,slug:text:required").unwrap();
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
        make_collection(tmp.path(), "posts", None, &CollectionOptions::default()).unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/posts.lua")).unwrap();
        assert!(content.contains("crap.collections.define(\"posts\""));
        assert!(content.contains("singular = \"Post\""));
        assert!(content.contains("plural = \"Posts\""));
        assert!(content.contains("timestamps = true"));
        assert!(content.contains("crap.fields.text({"));
        assert!(content.contains("name = \"title\""));
        assert!(content.contains("required = true"));
    }

    #[test]
    fn test_make_collection_with_fields() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = CollectionOptions {
            no_timestamps: true,
            ..CollectionOptions::default()
        };
        make_collection(
            tmp.path(),
            "articles",
            Some("headline:text:required,body:richtext,draft:checkbox"),
            &opts,
        )
        .unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/articles.lua")).unwrap();
        assert!(content.contains("timestamps = false"));
        assert!(content.contains("crap.fields.text({"));
        assert!(content.contains("name = \"headline\""));
        assert!(content.contains("crap.fields.richtext({"));
        assert!(content.contains("name = \"body\""));
        assert!(content.contains("crap.fields.checkbox({"));
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
        assert_eq!(pluralize("Quiz"), "Quizzes");
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
        let opts = CollectionOptions {
            auth: true,
            ..CollectionOptions::default()
        };
        make_collection(tmp.path(), "users", None, &opts).unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/users.lua")).unwrap();
        assert!(content.contains("auth = true"));
        assert!(content.contains("use_as_title = \"email\""));
    }

    #[test]
    fn test_make_collection_upload() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = CollectionOptions {
            upload: true,
            ..CollectionOptions::default()
        };
        make_collection(tmp.path(), "media", None, &opts).unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/media.lua")).unwrap();
        assert!(content.contains("upload = true"));
        assert!(content.contains("name = \"alt\""));
    }

    #[test]
    fn test_make_collection_versions() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = CollectionOptions {
            versions: true,
            ..CollectionOptions::default()
        };
        make_collection(tmp.path(), "posts", None, &opts).unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/posts.lua")).unwrap();
        assert!(content.contains("versions = true"));
    }

    #[test]
    fn test_make_collection_with_localized_fields() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_collection(
            tmp.path(),
            "posts",
            Some("title:text:required:localized,body:textarea:localized"),
            &CollectionOptions::default(),
        )
        .unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/posts.lua")).unwrap();
        assert!(content.contains("localized = true"));
    }

    #[test]
    fn test_parse_fields_shorthand_new_types() {
        // Verify all newer field types are accepted
        let fields = parse_fields_shorthand(
            "buttons:radio,section:collapsible,panels:tabs,snippet:code,related:join",
        )
        .unwrap();
        assert_eq!(fields.len(), 5);
        assert_eq!(fields[0].field_type, "radio");
        assert_eq!(fields[1].field_type, "collapsible");
        assert_eq!(fields[2].field_type, "tabs");
        assert_eq!(fields[3].field_type, "code");
        assert_eq!(fields[4].field_type, "join");
    }

    #[test]
    fn test_make_collection_invalid_slug() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let result = make_collection(tmp.path(), "Bad Slug", None, &CollectionOptions::default());
        assert!(result.is_err());
    }

    #[test]
    fn test_make_collection_refuses_overwrite() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_collection(tmp.path(), "posts", None, &CollectionOptions::default()).unwrap();
        let result = make_collection(tmp.path(), "posts", None, &CollectionOptions::default());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("--force"));
    }

    #[test]
    fn test_make_collection_force_overwrite() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_collection(tmp.path(), "posts", None, &CollectionOptions::default()).unwrap();
        let opts = CollectionOptions {
            force: true,
            ..CollectionOptions::default()
        };
        assert!(make_collection(tmp.path(), "posts", None, &opts).is_ok());
    }

    #[test]
    fn test_complex_field_type_stubs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_collection(
            tmp.path(), "posts",
            Some("author:relationship,status:select,body:array,layout:blocks,meta:group,content:tabs,snippet:code,related:join,pic:upload,style:radio,section:collapsible,cols:row"),
            &CollectionOptions::default(),
        ).unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/posts.lua")).unwrap();
        assert!(
            content.contains("crap.fields.relationship({"),
            "relationship factory"
        );
        assert!(
            content.contains("relationship = { collection = \"TODO\" }"),
            "relationship stub"
        );
        assert!(content.contains("crap.fields.select({"), "select factory");
        assert!(
            content.contains("options = { { label = \"Option 1\", value = \"option_1\" } }"),
            "select stub"
        );
        assert!(
            content.contains("fields = { crap.fields.text({ name = \"item\" }) }"),
            "array sub-field stub"
        );
        assert!(content.contains("crap.fields.blocks({"), "blocks factory");
        assert!(content.contains("crap.fields.tabs({"), "tabs factory");
        assert!(
            content.contains("admin = { language = \"javascript\" }"),
            "code stub"
        );
        assert!(
            content.contains("collection = \"TODO\","),
            "join collection stub"
        );
        assert!(content.contains("on = \"TODO\","), "join on stub");
        assert!(content.contains("crap.fields.upload({"), "upload factory");
        assert!(
            content.contains("relationship = { collection = \"media\" }"),
            "upload relationship stub"
        );
    }

    #[test]
    fn test_access_block_in_output() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_collection(tmp.path(), "posts", None, &CollectionOptions::default()).unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/posts.lua")).unwrap();
        assert!(content.contains("-- access = {"));
        assert!(content.contains("--     read   = \"access.anyone\""));
        assert!(content.contains("--     create = \"access.authenticated\""));
        assert!(content.contains("--     delete = \"access.admin_only\""));
    }

    #[test]
    fn test_admin_block_expanded() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_collection(tmp.path(), "posts", None, &CollectionOptions::default()).unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/posts.lua")).unwrap();
        assert!(content.contains("default_sort = \"-created_at\""));
        assert!(content.contains("list_searchable_fields = { \"title\" }"));
    }

    #[test]
    fn test_admin_block_no_default_sort_without_timestamps() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = CollectionOptions {
            no_timestamps: true,
            ..CollectionOptions::default()
        };
        make_collection(tmp.path(), "posts", None, &opts).unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/posts.lua")).unwrap();
        assert!(
            !content.contains("default_sort"),
            "no default_sort when timestamps disabled"
        );
        assert!(content.contains("list_searchable_fields"));
    }

    #[test]
    fn test_upload_comment_block() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = CollectionOptions {
            upload: true,
            ..CollectionOptions::default()
        };
        make_collection(tmp.path(), "media", None, &opts).unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/media.lua")).unwrap();
        assert!(content.contains("upload = true"));
        assert!(content.contains("-- Full upload config (uncomment and customize):"));
        assert!(content.contains("--     mime_types"));
        assert!(content.contains("--     max_file_size"));
        assert!(content.contains("--     image_sizes"));
    }

    #[test]
    fn test_auth_comment_block() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = CollectionOptions {
            auth: true,
            ..CollectionOptions::default()
        };
        make_collection(tmp.path(), "users", None, &opts).unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/users.lua")).unwrap();
        assert!(content.contains("auth = true"));
        assert!(content.contains("-- Full auth config (uncomment and customize):"));
        assert!(content.contains("--     verify_email"));
        assert!(content.contains("--     strategies"));
    }

    #[test]
    fn test_pluralize_z_doubling() {
        assert_eq!(pluralize("Quiz"), "Quizzes");
        assert_eq!(pluralize("Fuzz"), "Fuzzes"); // double z stays
        assert_eq!(pluralize("Buzz"), "Buzzes");
    }

    #[test]
    fn test_parse_fields_shorthand_index_modifier() {
        let fields = parse_fields_shorthand("status:text:index").unwrap();
        assert_eq!(fields[0].name, "status");
        assert_eq!(fields[0].field_type, "text");
        // index is accepted as modifier but not stored in FieldStub
        assert!(!fields[0].required);
    }

    #[test]
    fn test_indexes_comment_in_output() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_collection(tmp.path(), "posts", None, &CollectionOptions::default()).unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/posts.lua")).unwrap();
        assert!(content.contains("-- indexes = {"));
        assert!(content.contains("--     { fields = { \"status\", \"created_at\" } },"));
        assert!(content.contains("--     { fields = { \"slug\" }, unique = true },"));
    }
}
