//! `make collection` command — generate collection Lua files.

use anyhow::{Context as _, Result, bail};
use std::{fs, path::Path};

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

pub struct FieldStub {
    pub name: String,
    pub field_type: String,
    pub required: bool,
    pub localized: bool,
    pub fields: Vec<FieldStub>,
    pub blocks: Vec<BlockStub>,
    pub tabs: Vec<TabStub>,
}

pub struct BlockStub {
    pub block_type: String,
    pub label: String,
    pub fields: Vec<FieldStub>,
}

pub struct TabStub {
    pub label: String,
    pub fields: Vec<FieldStub>,
}

/// Generate a collection Lua file at `<config_dir>/collections/<slug>.lua`.
///
/// Accepts pre-parsed field stubs or `None` for defaults.
pub fn make_collection(
    config_dir: &Path,
    slug: &str,
    fields: Option<&[FieldStub]>,
    opts: &CollectionOptions,
) -> Result<()> {
    super::validate_slug(slug)?;

    let collections_dir = config_dir.join("collections");
    fs::create_dir_all(&collections_dir).context("Failed to create collections/ directory")?;

    let file_path = collections_dir.join(format!("{}.lua", slug));

    if file_path.exists() && !opts.force {
        bail!(
            "File '{}' already exists — use --force to overwrite",
            file_path.display()
        );
    }

    let singular_slug = singularize(slug);
    let label_singular = super::to_title_case(&singular_slug);
    let label_plural = pluralize(&label_singular);
    let timestamps = if opts.no_timestamps { "false" } else { "true" };

    let default_fields;
    let fields = match fields {
        Some(f) => f,
        None if opts.upload => &[] as &[FieldStub],
        None if opts.auth => &[] as &[FieldStub],
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

    // Pick the first scalar (non-container) field for use_as_title / list_searchable_fields.
    let title_field = fields
        .iter()
        .find(|f| {
            !CONTAINER_TYPES.contains(&f.field_type.as_str())
                && f.field_type != "blocks"
                && f.field_type != "tabs"
        })
        .map(|f| f.name.as_str());

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
    let use_as_title = if opts.auth {
        Some("email")
    } else if opts.upload {
        Some("filename")
    } else {
        title_field
    };
    lua.push_str("    admin = {\n");
    if let Some(title) = use_as_title {
        lua.push_str(&format!("        use_as_title = \"{}\",\n", title));
    }

    if !opts.no_timestamps {
        lua.push_str("        default_sort = \"-created_at\",\n");
    }
    if let Some(title) = use_as_title {
        lua.push_str(&format!(
            "        list_searchable_fields = {{ \"{}\" }},\n",
            title
        ));
    }
    lua.push_str("    },\n");
    lua.push_str("    fields = {\n");

    for field in fields {
        write_field_lua(&mut lua, field, 8);
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

/// Return type-specific Lua stub lines for non-container field types.
pub fn type_specific_stub(field_type: &str) -> Option<&'static str> {
    match field_type {
        "select" | "radio" => {
            Some("options = { { label = \"Option 1\", value = \"option_1\" } },\n")
        }
        "relationship" => Some(
            "relationship = { collection = \"other_collection\" }, -- change to target collection slug\n",
        ),
        "upload" => Some("relationship = { collection = \"media\" },\n"),
        "join" => Some(
            "collection = \"other_collection\", -- target collection slug\n            on = \"field_name\",               -- relationship field on target that points back\n",
        ),
        "code" => Some("admin = { language = \"javascript\" },\n"),
        _ => None,
    }
}

/// Write a single field's Lua representation with proper indentation and recursion.
pub fn write_field_lua(lua: &mut String, field: &FieldStub, indent: usize) {
    let pad = " ".repeat(indent);
    let inner = " ".repeat(indent + 4);

    lua.push_str(&format!("{}crap.fields.{}({{\n", pad, field.field_type));
    lua.push_str(&format!("{}name = \"{}\",\n", inner, field.name));

    if field.required {
        lua.push_str(&format!("{}required = true,\n", inner));
    }
    if field.localized {
        lua.push_str(&format!("{}localized = true,\n", inner));
    }

    // Nested fields (group, array, row, collapsible)
    if !field.fields.is_empty() {
        lua.push_str(&format!("{}fields = {{\n", inner));
        for sub in &field.fields {
            write_field_lua(lua, sub, indent + 8);
        }
        lua.push_str(&format!("{}}},\n", inner));
    } else if !field.blocks.is_empty() {
        // Blocks
        lua.push_str(&format!("{}blocks = {{\n", inner));
        for block in &field.blocks {
            lua.push_str(&format!("{}{{\n", " ".repeat(indent + 8)));
            lua.push_str(&format!(
                "{}type = \"{}\",\n",
                " ".repeat(indent + 12),
                block.block_type
            ));
            lua.push_str(&format!(
                "{}label = \"{}\",\n",
                " ".repeat(indent + 12),
                block.label
            ));
            lua.push_str(&format!("{}fields = {{\n", " ".repeat(indent + 12)));
            for sub in &block.fields {
                write_field_lua(lua, sub, indent + 16);
            }
            lua.push_str(&format!("{}}},\n", " ".repeat(indent + 12)));
            lua.push_str(&format!("{}}},\n", " ".repeat(indent + 8)));
        }
        lua.push_str(&format!("{}}},\n", inner));
    } else if !field.tabs.is_empty() {
        // Tabs
        lua.push_str(&format!("{}tabs = {{\n", inner));
        for tab in &field.tabs {
            lua.push_str(&format!("{}{{\n", " ".repeat(indent + 8)));
            lua.push_str(&format!(
                "{}label = \"{}\",\n",
                " ".repeat(indent + 12),
                tab.label
            ));
            lua.push_str(&format!("{}fields = {{\n", " ".repeat(indent + 12)));
            for sub in &tab.fields {
                write_field_lua(lua, sub, indent + 16);
            }
            lua.push_str(&format!("{}}},\n", " ".repeat(indent + 12)));
            lua.push_str(&format!("{}}},\n", " ".repeat(indent + 8)));
        }
        lua.push_str(&format!("{}}},\n", inner));
    } else if CONTAINER_TYPES.contains(&field.field_type.as_str()) {
        // Container with no user-defined children — emit default stub
        lua.push_str(&format!(
            "{}fields = {{ crap.fields.text({{ name = \"item\" }}) }},\n",
            inner
        ));
    } else if field.field_type == "blocks" {
        lua.push_str(&format!(
            "{}blocks = {{ {{ type = \"block_type\", label = \"Block\", fields = {{ crap.fields.text({{ name = \"content\" }}) }} }} }},\n",
            inner
        ));
    } else if field.field_type == "tabs" {
        lua.push_str(&format!(
            "{}tabs = {{ {{ label = \"Tab 1\", fields = {{ crap.fields.text({{ name = \"item\" }}) }} }} }},\n",
            inner
        ));
    } else if let Some(stub) = type_specific_stub(&field.field_type) {
        lua.push_str(&format!("{}{}", inner, stub));
    }

    lua.push_str(&format!("{}}}),\n", pad));
}

/// Container field types that support nested subfields.
const CONTAINER_TYPES: &[&str] = &["group", "array", "row", "collapsible"];

/// Split `s` on `sep` only when parenthesis depth is zero.
fn split_at_depth_zero(s: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            c if c == sep && depth == 0 => {
                parts.push(&s[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
}

/// Find the index of the closing `)` that matches the opening `(` at position 0.
fn find_matching_paren(s: &str) -> Result<usize> {
    let mut depth = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(i);
                }
            }
            _ => {}
        }
    }
    bail!("Unbalanced parentheses — missing closing ')'");
}

/// Parse a single field token like `name:type(subfields):required:localized`.
fn parse_field_token(token: &str) -> Result<FieldStub> {
    let token = token.trim();
    if token.is_empty() {
        bail!("Empty field token");
    }

    // Split on ':' at depth zero to handle colons inside parens
    let segments = split_at_depth_zero(token, ':');
    if segments.len() < 2 {
        bail!(
            "Invalid field shorthand '{}' — expected 'name:type[:required][:localized]'",
            token
        );
    }

    let name = segments[0].to_string();
    let type_segment = segments[1];

    // Check if there's a '(' in the type segment indicating subfields
    let (field_type, subfield_content) = if let Some(paren_pos) = type_segment.find('(') {
        let ft = type_segment[..paren_pos].to_lowercase();
        let rest = &type_segment[paren_pos..];
        let close = find_matching_paren(rest)?;
        let content = &rest[1..close];
        // Anything after the closing paren in this segment should be empty
        let after = &rest[close + 1..];
        if !after.is_empty() {
            bail!(
                "Unexpected characters '{}' after closing ')' in field '{}'",
                after,
                name
            );
        }
        (ft, Some(content.to_string()))
    } else {
        (type_segment.to_lowercase(), None)
    };

    if !VALID_FIELD_TYPES.contains(&field_type.as_str()) {
        bail!(
            "Unknown field type '{}' — valid types: {}",
            field_type,
            VALID_FIELD_TYPES.join(", ")
        );
    }

    // Parse modifiers from remaining segments
    let mut required = false;
    let mut localized = false;
    for seg in &segments[2..] {
        match *seg {
            "required" => required = true,
            "localized" => localized = true,
            "index" => {} // accepted but not stored
            other => bail!(
                "Unknown modifier '{}' in field '{}' — valid: required, localized, index",
                other,
                name
            ),
        }
    }

    // Parse subfield content based on type
    let mut fields = Vec::new();
    let mut blocks = Vec::new();
    let mut tabs = Vec::new();

    if let Some(content) = subfield_content {
        if CONTAINER_TYPES.contains(&field_type.as_str()) {
            fields = parse_fields_shorthand(&content)?;
        } else if field_type == "blocks" {
            blocks = parse_block_entries(&content)?;
        } else if field_type == "tabs" {
            tabs = parse_tab_entries(&content)?;
        } else {
            bail!(
                "Field type '{}' does not support subfields — only group, array, row, collapsible, blocks, and tabs do",
                field_type
            );
        }
    }

    Ok(FieldStub {
        name,
        field_type,
        required,
        localized,
        fields,
        blocks,
        tabs,
    })
}

/// Parse block entries: `type|label(fields),type|label(fields),...`
fn parse_block_entries(s: &str) -> Result<Vec<BlockStub>> {
    let parts = split_at_depth_zero(s, ',');
    let mut blocks = Vec::new();
    for part in parts {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        // Format: type|label(fields)
        let pipe_pos = part.find('|').ok_or_else(|| {
            anyhow::anyhow!(
                "Block entry '{}' missing '|' separator — expected 'type|label(fields)'",
                part
            )
        })?;
        let block_type = part[..pipe_pos].to_string();
        let rest = &part[pipe_pos + 1..];

        let (label, fields) = if let Some(paren_pos) = rest.find('(') {
            let label = rest[..paren_pos].to_string();
            let paren_rest = &rest[paren_pos..];
            let close = find_matching_paren(paren_rest)?;
            let content = &paren_rest[1..close];
            (label, parse_fields_shorthand(content)?)
        } else {
            (rest.to_string(), Vec::new())
        };

        if block_type.is_empty() {
            bail!("Block type cannot be empty");
        }
        if label.is_empty() {
            bail!(
                "Block label cannot be empty for block type '{}'",
                block_type
            );
        }

        blocks.push(BlockStub {
            block_type,
            label,
            fields,
        });
    }
    if blocks.is_empty() {
        bail!("No blocks parsed from entries");
    }
    Ok(blocks)
}

/// Parse tab entries: `label(fields),label(fields),...`
fn parse_tab_entries(s: &str) -> Result<Vec<TabStub>> {
    let parts = split_at_depth_zero(s, ',');
    let mut tabs = Vec::new();
    for part in parts {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (label, fields) = if let Some(paren_pos) = part.find('(') {
            let label = part[..paren_pos].to_string();
            let paren_rest = &part[paren_pos..];
            let close = find_matching_paren(paren_rest)?;
            let content = &paren_rest[1..close];
            (label, parse_fields_shorthand(content)?)
        } else {
            bail!(
                "Tab entry '{}' missing '(fields)' — expected 'label(fields)'",
                part
            );
        };

        if label.is_empty() {
            bail!("Tab label cannot be empty");
        }

        tabs.push(TabStub { label, fields });
    }
    if tabs.is_empty() {
        bail!("No tabs parsed from entries");
    }
    Ok(tabs)
}

/// Parse inline field shorthand: "title:text:required,status:select,body:textarea:localized"
///
/// Supports nested syntax for container types:
/// - `group|array|row|collapsible`: `name:type(subfields):modifiers`
/// - `blocks`: `name:blocks(type|label(fields),...)`
/// - `tabs`: `name:tabs(label(fields),...)`
///
/// Modifiers after the type (or closing `)`) are order-independent flags: `required`, `localized`.
pub fn parse_fields_shorthand(s: &str) -> Result<Vec<FieldStub>> {
    let parts = split_at_depth_zero(s, ',');
    let mut fields = Vec::new();
    for part in parts {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        fields.push(parse_field_token(part)?);
    }

    if fields.is_empty() {
        bail!("No fields parsed from shorthand");
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

    /// Helper: parse shorthand and call make_collection with the result.
    fn make_collection_from_shorthand(
        config_dir: &Path,
        slug: &str,
        shorthand: Option<&str>,
        opts: &CollectionOptions,
    ) -> Result<()> {
        let parsed = shorthand.map(parse_fields_shorthand).transpose()?;
        make_collection(config_dir, slug, parsed.as_deref(), opts)
    }

    #[test]
    fn test_make_collection_with_fields() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = CollectionOptions {
            no_timestamps: true,
            ..CollectionOptions::default()
        };
        make_collection_from_shorthand(
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
        assert!(content.contains("list_searchable_fields = { \"email\" }"));
        // No default fields — email/password are injected at runtime
        assert!(
            !content.contains("crap.fields."),
            "auth collection without custom fields should have empty fields block"
        );
    }

    #[test]
    fn test_make_collection_auth_with_custom_fields() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = CollectionOptions {
            auth: true,
            ..CollectionOptions::default()
        };
        make_collection_from_shorthand(tmp.path(), "users", Some("name:text,role:select"), &opts)
            .unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/users.lua")).unwrap();
        assert!(content.contains("auth = true"));
        assert!(content.contains("use_as_title = \"email\""));
        assert!(content.contains("name = \"name\""));
        assert!(content.contains("name = \"role\""));
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
        assert!(content.contains("use_as_title = \"filename\""));
        assert!(content.contains("list_searchable_fields = { \"filename\" }"));
        // No default fields — filename/mime_type/size are injected at runtime
        assert!(
            !content.contains("crap.fields."),
            "upload collection without custom fields should have empty fields block"
        );
    }

    #[test]
    fn test_make_collection_upload_with_custom_fields() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = CollectionOptions {
            upload: true,
            ..CollectionOptions::default()
        };
        make_collection_from_shorthand(
            tmp.path(),
            "media",
            Some("alt:text,caption:textarea"),
            &opts,
        )
        .unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/media.lua")).unwrap();
        assert!(content.contains("upload = true"));
        assert!(content.contains("use_as_title = \"filename\""));
        assert!(content.contains("name = \"alt\""));
        assert!(content.contains("name = \"caption\""));
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
        make_collection_from_shorthand(
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
        make_collection_from_shorthand(
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
            content.contains("relationship = { collection = \"other_collection\" }"),
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
            content.contains("collection = \"other_collection\","),
            "join collection stub"
        );
        assert!(content.contains("on = \"field_name\","), "join on stub");
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

    // ── Nested parsing tests ──────────────────────────────────────────

    #[test]
    fn test_split_at_depth_zero() {
        let parts = super::split_at_depth_zero("a,b(c,d),e", ',');
        assert_eq!(parts, vec!["a", "b(c,d)", "e"]);

        let parts = super::split_at_depth_zero("a:b(c:d):req", ':');
        assert_eq!(parts, vec!["a", "b(c:d)", "req"]);

        // nested parens
        let parts = super::split_at_depth_zero("a(b(c,d),e),f", ',');
        assert_eq!(parts, vec!["a(b(c,d),e)", "f"]);
    }

    #[test]
    fn test_find_matching_paren() {
        assert_eq!(super::find_matching_paren("(abc)").unwrap(), 4);
        assert_eq!(super::find_matching_paren("(a(b)c)").unwrap(), 6);
        assert!(super::find_matching_paren("(abc").is_err());
    }

    #[test]
    fn test_parse_nested_group() {
        let fields =
            parse_fields_shorthand("seo:group(meta_title:text,meta_desc:textarea):required")
                .unwrap();
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].name, "seo");
        assert_eq!(fields[0].field_type, "group");
        assert!(fields[0].required);
        assert_eq!(fields[0].fields.len(), 2);
        assert_eq!(fields[0].fields[0].name, "meta_title");
        assert_eq!(fields[0].fields[0].field_type, "text");
        assert_eq!(fields[0].fields[1].name, "meta_desc");
        assert_eq!(fields[0].fields[1].field_type, "textarea");
    }

    #[test]
    fn test_parse_nested_array() {
        let fields =
            parse_fields_shorthand("variants:array(color:text:required,size:number)").unwrap();
        assert_eq!(fields[0].field_type, "array");
        assert_eq!(fields[0].fields.len(), 2);
        assert!(fields[0].fields[0].required);
        assert_eq!(fields[0].fields[1].field_type, "number");
    }

    #[test]
    fn test_parse_nested_blocks() {
        let fields = parse_fields_shorthand(
            "content:blocks(paragraph|Paragraph(body:textarea),hero|Hero(title:text,image:upload))",
        )
        .unwrap();
        assert_eq!(fields[0].field_type, "blocks");
        assert_eq!(fields[0].blocks.len(), 2);
        assert_eq!(fields[0].blocks[0].block_type, "paragraph");
        assert_eq!(fields[0].blocks[0].label, "Paragraph");
        assert_eq!(fields[0].blocks[0].fields.len(), 1);
        assert_eq!(fields[0].blocks[0].fields[0].name, "body");
        assert_eq!(fields[0].blocks[1].block_type, "hero");
        assert_eq!(fields[0].blocks[1].label, "Hero");
        assert_eq!(fields[0].blocks[1].fields.len(), 2);
    }

    #[test]
    fn test_parse_nested_tabs() {
        let fields = parse_fields_shorthand(
            "settings:tabs(General(name:text,email:email),Advanced(api_key:text))",
        )
        .unwrap();
        assert_eq!(fields[0].field_type, "tabs");
        assert_eq!(fields[0].tabs.len(), 2);
        assert_eq!(fields[0].tabs[0].label, "General");
        assert_eq!(fields[0].tabs[0].fields.len(), 2);
        assert_eq!(fields[0].tabs[1].label, "Advanced");
        assert_eq!(fields[0].tabs[1].fields.len(), 1);
        assert_eq!(fields[0].tabs[1].fields[0].name, "api_key");
    }

    #[test]
    fn test_parse_deeply_nested() {
        // array containing a group
        let fields = parse_fields_shorthand(
            "variants:array(color:text,dimensions:group(width:number,height:number))",
        )
        .unwrap();
        assert_eq!(fields[0].fields.len(), 2);
        assert_eq!(fields[0].fields[1].field_type, "group");
        assert_eq!(fields[0].fields[1].fields.len(), 2);
        assert_eq!(fields[0].fields[1].fields[0].name, "width");
    }

    #[test]
    fn test_parse_mixed_flat_and_nested() {
        let fields = parse_fields_shorthand(
            "title:text:required,seo:group(meta_title:text,meta_desc:textarea),body:richtext",
        )
        .unwrap();
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0].name, "title");
        assert!(fields[0].fields.is_empty());
        assert_eq!(fields[1].name, "seo");
        assert_eq!(fields[1].fields.len(), 2);
        assert_eq!(fields[2].name, "body");
    }

    #[test]
    fn test_parse_nested_error_unbalanced_parens() {
        assert!(parse_fields_shorthand("seo:group(title:text").is_err());
    }

    #[test]
    fn test_parse_nested_error_subfields_on_non_container() {
        assert!(parse_fields_shorthand("title:text(sub:number)").is_err());
    }

    #[test]
    fn test_parse_nested_error_missing_block_label() {
        assert!(parse_fields_shorthand("content:blocks(paragraph(body:textarea))").is_err());
    }

    #[test]
    fn test_make_collection_with_nested_fields() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fields = parse_fields_shorthand(
            "title:text:required,seo:group(meta_title:text,meta_desc:textarea),items:array(name:text:required,qty:number)"
        ).unwrap();
        make_collection(
            tmp.path(),
            "posts",
            Some(&fields),
            &CollectionOptions::default(),
        )
        .unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/posts.lua")).unwrap();
        // Top-level fields
        assert!(content.contains("crap.fields.text({"));
        assert!(content.contains("name = \"title\""));
        // Group with nested fields
        assert!(content.contains("crap.fields.group({"));
        assert!(content.contains("name = \"seo\""));
        assert!(content.contains("name = \"meta_title\""));
        assert!(content.contains("name = \"meta_desc\""));
        // Array with nested fields
        assert!(content.contains("crap.fields.array({"));
        assert!(content.contains("name = \"items\""));
        assert!(content.contains("name = \"name\""));
        assert!(content.contains("name = \"qty\""));
        // Nested fields block should have `fields = {` (not the placeholder stub)
        // Count occurrences of "fields = {" — should be at least 3 (top-level, group, array)
        let fields_count = content.matches("fields = {").count();
        assert!(
            fields_count >= 3,
            "expected at least 3 'fields = {{' blocks, got {}",
            fields_count
        );
    }

    #[test]
    fn test_make_collection_with_nested_blocks() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fields = parse_fields_shorthand(
            "content:blocks(paragraph|Paragraph(body:textarea),hero|Hero(title:text,image:upload))",
        )
        .unwrap();
        make_collection(
            tmp.path(),
            "pages",
            Some(&fields),
            &CollectionOptions::default(),
        )
        .unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/pages.lua")).unwrap();
        assert!(content.contains("crap.fields.blocks({"));
        assert!(content.contains("type = \"paragraph\""));
        assert!(content.contains("label = \"Paragraph\""));
        assert!(content.contains("name = \"body\""));
        assert!(content.contains("type = \"hero\""));
        assert!(content.contains("label = \"Hero\""));
        assert!(content.contains("name = \"image\""));
    }

    #[test]
    fn test_make_collection_with_nested_tabs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fields = parse_fields_shorthand(
            "settings:tabs(General(name:text,email:email),Advanced(api_key:text))",
        )
        .unwrap();
        make_collection(
            tmp.path(),
            "config",
            Some(&fields),
            &CollectionOptions::default(),
        )
        .unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/config.lua")).unwrap();
        assert!(content.contains("crap.fields.tabs({"));
        assert!(content.contains("label = \"General\""));
        assert!(content.contains("name = \"name\""));
        assert!(content.contains("name = \"email\""));
        assert!(content.contains("label = \"Advanced\""));
        assert!(content.contains("name = \"api_key\""));
    }

    #[test]
    fn test_all_container_fields_omit_use_as_title() {
        // When all fields are containers, there's no scalar field to use as title.
        // use_as_title and list_searchable_fields should be omitted.
        let fields = parse_fields_shorthand("items:array(label:text)").unwrap();
        let tmp = tempfile::tempdir().expect("tempdir");
        make_collection(
            tmp.path(),
            "things",
            Some(&fields),
            &CollectionOptions::default(),
        )
        .unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/things.lua")).unwrap();
        assert!(
            !content.contains("use_as_title"),
            "no scalar field exists — use_as_title should be omitted"
        );
        assert!(
            !content.contains("list_searchable_fields"),
            "no scalar field exists — list_searchable_fields should be omitted"
        );
    }

    // ── Shorthand parsing edge cases ─────────────────────────────────

    #[test]
    fn test_parse_empty_parens_container() {
        // `items:array()` → should error (no subfields parsed)
        assert!(parse_fields_shorthand("items:array()").is_err());
    }

    #[test]
    fn test_parse_trailing_comma() {
        // Trailing comma should be ignored, yielding 2 fields
        let fields = parse_fields_shorthand("title:text,body:textarea,").unwrap();
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].name, "title");
        assert_eq!(fields[1].name, "body");
    }

    #[test]
    fn test_parse_whitespace_around_segments() {
        // Leading/trailing whitespace on the token is trimmed,
        // but internal spaces around ':' are NOT trimmed (they become part of the segment).
        // So `title : text` has name "title " and type " text" which is invalid.
        // Only fully trimmed tokens work: "  title:text:required  "
        let fields = parse_fields_shorthand("  title:text:required  ").unwrap();
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].name, "title");
        assert_eq!(fields[0].field_type, "text");
        assert!(fields[0].required);

        // Spaces around ':' should fail (type " text " is unknown)
        assert!(parse_fields_shorthand("title : text").is_err());
    }

    #[test]
    fn test_parse_deeply_nested_4_levels() {
        // array > group > collapsible > row > text
        let fields =
            parse_fields_shorthand("a:array(b:group(c:collapsible(d:row(x:text))))").unwrap();
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].field_type, "array");
        assert_eq!(fields[0].fields[0].field_type, "group");
        assert_eq!(fields[0].fields[0].fields[0].field_type, "collapsible");
        assert_eq!(fields[0].fields[0].fields[0].fields[0].field_type, "row");
        assert_eq!(
            fields[0].fields[0].fields[0].fields[0].fields[0].field_type,
            "text"
        );
        assert_eq!(fields[0].fields[0].fields[0].fields[0].fields[0].name, "x");
    }

    #[test]
    fn test_parse_blocks_empty_fields() {
        // `para|Paragraph()` → empty parens → parse_fields_shorthand("") → error
        assert!(parse_fields_shorthand("content:blocks(para|Paragraph())").is_err());
    }

    #[test]
    fn test_parse_tabs_single_tab() {
        let fields = parse_fields_shorthand("settings:tabs(General(name:text))").unwrap();
        assert_eq!(fields[0].tabs.len(), 1);
        assert_eq!(fields[0].tabs[0].label, "General");
        assert_eq!(fields[0].tabs[0].fields.len(), 1);
        assert_eq!(fields[0].tabs[0].fields[0].name, "name");
    }

    #[test]
    fn test_parse_blocks_empty_type() {
        // `|Paragraph(body:textarea)` → empty block type → error
        assert!(parse_fields_shorthand("content:blocks(|Paragraph(body:textarea))").is_err());
    }

    #[test]
    fn test_parse_tabs_empty_label() {
        // `(name:text)` with empty label → error
        assert!(parse_fields_shorthand("settings:tabs((name:text))").is_err());
    }

    #[test]
    fn test_parse_container_modifiers_with_subfields() {
        // Modifiers after closing paren: `seo:group(t:text):required:localized`
        let fields = parse_fields_shorthand("seo:group(t:text):required:localized").unwrap();
        assert_eq!(fields[0].name, "seo");
        assert_eq!(fields[0].field_type, "group");
        assert!(fields[0].required);
        assert!(fields[0].localized);
        assert_eq!(fields[0].fields.len(), 1);
        assert_eq!(fields[0].fields[0].name, "t");
    }

    // ── Lua generation edge cases ────────────────────────────────────

    #[test]
    fn test_make_collection_blocks_only_omits_title() {
        // When blocks is the sole field → no scalar → omit use_as_title
        let fields =
            parse_fields_shorthand("content:blocks(para|Paragraph(body:textarea))").unwrap();
        let tmp = tempfile::tempdir().expect("tempdir");
        make_collection(
            tmp.path(),
            "pages",
            Some(&fields),
            &CollectionOptions::default(),
        )
        .unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/pages.lua")).unwrap();
        assert!(
            !content.contains("use_as_title"),
            "blocks-only collection should omit use_as_title"
        );
        assert!(
            !content.contains("list_searchable_fields"),
            "blocks-only collection should omit list_searchable_fields"
        );
    }

    #[test]
    fn test_make_collection_tabs_only_omits_title() {
        // When tabs is the sole field → no scalar → omit use_as_title
        let fields = parse_fields_shorthand("settings:tabs(General(name:text))").unwrap();
        let tmp = tempfile::tempdir().expect("tempdir");
        make_collection(
            tmp.path(),
            "config",
            Some(&fields),
            &CollectionOptions::default(),
        )
        .unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/config.lua")).unwrap();
        assert!(
            !content.contains("use_as_title"),
            "tabs-only collection should omit use_as_title"
        );
    }

    #[test]
    fn test_make_collection_auth_versions() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = CollectionOptions {
            auth: true,
            versions: true,
            ..CollectionOptions::default()
        };
        make_collection(tmp.path(), "users", None, &opts).unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/users.lua")).unwrap();
        assert!(content.contains("auth = true"));
        assert!(content.contains("versions = true"));
        assert!(content.contains("use_as_title = \"email\""));
    }

    #[test]
    fn test_make_collection_upload_versions() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = CollectionOptions {
            upload: true,
            versions: true,
            ..CollectionOptions::default()
        };
        make_collection(tmp.path(), "media", None, &opts).unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/media.lua")).unwrap();
        assert!(content.contains("upload = true"));
        assert!(content.contains("versions = true"));
        assert!(content.contains("use_as_title = \"filename\""));
    }

    #[test]
    fn test_make_collection_all_flags() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = CollectionOptions {
            auth: true,
            versions: true,
            no_timestamps: true,
            ..CollectionOptions::default()
        };
        make_collection(tmp.path(), "users", None, &opts).unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/users.lua")).unwrap();
        assert!(content.contains("auth = true"));
        assert!(content.contains("versions = true"));
        assert!(content.contains("timestamps = false"));
        assert!(
            !content.contains("default_sort"),
            "no default_sort when timestamps disabled"
        );
    }

    #[test]
    fn test_make_collection_nested_localized() {
        // Nested array with localized subfields
        let tmp = tempfile::tempdir().expect("tempdir");
        make_collection_from_shorthand(
            tmp.path(),
            "pages",
            Some("items:array(label:text:required:localized,desc:textarea:localized)"),
            &CollectionOptions::default(),
        )
        .unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/pages.lua")).unwrap();
        assert!(content.contains("crap.fields.array({"));
        assert!(content.contains("name = \"items\""));
        // Subfields should have localized = true
        assert!(content.contains("localized = true"));
        assert!(content.contains("name = \"label\""));
        assert!(content.contains("name = \"desc\""));
        // Count localized occurrences — should be 2 (label + desc)
        let localized_count = content.matches("localized = true").count();
        assert_eq!(
            localized_count, 2,
            "expected 2 localized subfields, got {}",
            localized_count
        );
    }

    #[test]
    fn test_container_without_subfields_gets_default_stub() {
        // Containers without (...) should still get default placeholder stubs
        let fields =
            parse_fields_shorthand("items:array,meta:group,layout:blocks,panels:tabs").unwrap();
        assert!(fields[0].fields.is_empty()); // no subfields parsed
        assert!(fields[1].fields.is_empty());
        assert!(fields[2].blocks.is_empty());
        assert!(fields[3].tabs.is_empty());

        let tmp = tempfile::tempdir().expect("tempdir");
        make_collection(
            tmp.path(),
            "test",
            Some(&fields),
            &CollectionOptions::default(),
        )
        .unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/test.lua")).unwrap();
        // Should have default stubs
        assert!(content.contains("fields = { crap.fields.text({ name = \"item\" }) }"));
        assert!(content.contains("blocks = { { type = \"block_type\""));
        assert!(content.contains("tabs = { { label = \"Tab 1\""));
    }
}
