//! Shorthand field parser, inflection helpers, and Lua string escaping.

use anyhow::{Result, anyhow, bail};

use super::types::{BlockStub, CONTAINER_TYPES, FieldStub, TabStub, VALID_FIELD_TYPES};

/// Escape a string for safe embedding in a Lua double-quoted string literal.
pub(super) fn escape_lua_string(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\0', "\\0")
}

/// Naive English singularization: strip trailing "s", "es", or "ies" → "y".
pub(super) fn singularize(s: &str) -> String {
    let lower = s.to_lowercase();

    if lower.ends_with("ies") && lower.len() > 3 {
        return format!("{}y", &s[..s.len() - 3]);
    }

    if lower.ends_with("ses")
        || lower.ends_with("xes")
        || lower.ends_with("zes")
        || lower.ends_with("shes")
        || lower.ends_with("ches")
    {
        return s[..s.len() - 2].to_string();
    }

    if lower.ends_with('s') && !lower.ends_with("ss") && lower.len() > 1 {
        return s[..s.len() - 1].to_string();
    }

    s.to_string()
}

/// Naive English pluralization: add "s" (or "es" for sibilants, "ies" for consonant+y).
pub(super) fn pluralize(s: &str) -> String {
    if s.is_empty() {
        return s.to_string();
    }

    let lower = s.to_lowercase();

    if lower.ends_with("z") && !lower.ends_with("zz") {
        return format!("{}zes", s);
    }

    if lower.ends_with("s")
        || lower.ends_with("x")
        || lower.ends_with("zz")
        || lower.ends_with("sh")
        || lower.ends_with("ch")
    {
        return format!("{}es", s);
    }

    if lower.ends_with("y")
        && !lower.ends_with("ay")
        && !lower.ends_with("ey")
        && !lower.ends_with("oy")
        && !lower.ends_with("uy")
    {
        return format!("{}ies", &s[..s.len() - 1]);
    }

    format!("{}s", s)
}

/// Split `s` on `sep` only when parenthesis depth is zero.
pub(super) fn split_at_depth_zero(s: &str, sep: char) -> Vec<&str> {
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

/// Parse a single field token like `name:type(subfields):required:localized`.
fn parse_field_token(token: &str) -> Result<FieldStub> {
    let token = token.trim();
    if token.is_empty() {
        bail!("Empty field token");
    }

    let segments = split_at_depth_zero(token, ':');
    if segments.len() < 2 {
        bail!(
            "Invalid field shorthand '{}' — expected 'name:type[:required][:localized]'",
            token
        );
    }

    let name = segments[0].to_string();
    validate_field_name(&name)?;

    let (field_type, subfield_content) = parse_type_segment(segments[1], &name)?;
    validate_field_type(&field_type)?;

    let (required, localized) = parse_modifiers(&segments[2..], &name)?;
    let (fields, blocks, tabs) = parse_subfield_content(&field_type, subfield_content)?;

    Ok(FieldStub::builder(name, field_type)
        .required(required)
        .localized(localized)
        .fields(fields)
        .blocks(blocks)
        .tabs(tabs)
        .build())
}

/// Validate that a field name is a safe identifier.
fn validate_field_name(name: &str) -> Result<()> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        bail!(
            "Invalid field name '{}' — must contain only letters, digits, and underscores",
            name
        );
    }
    Ok(())
}

/// Validate that a field type is recognized.
fn validate_field_type(field_type: &str) -> Result<()> {
    if !VALID_FIELD_TYPES.contains(&field_type) {
        bail!(
            "Unknown field type '{}' — valid types: {}",
            field_type,
            VALID_FIELD_TYPES.join(", ")
        );
    }
    Ok(())
}

/// Parse the type segment, extracting optional subfield content from parens.
fn parse_type_segment(segment: &str, name: &str) -> Result<(String, Option<String>)> {
    let Some(paren_pos) = segment.find('(') else {
        return Ok((segment.to_lowercase(), None));
    };

    let ft = segment[..paren_pos].to_lowercase();
    let rest = &segment[paren_pos..];
    let close = find_matching_paren(rest)?;
    let content = &rest[1..close];
    let after = &rest[close + 1..];

    if !after.is_empty() {
        bail!(
            "Unexpected characters '{}' after closing ')' in field '{}'",
            after,
            name
        );
    }

    Ok((ft, Some(content.to_string())))
}

/// Parse modifier flags from remaining segments.
fn parse_modifiers(segments: &[&str], name: &str) -> Result<(bool, bool)> {
    let mut required = false;
    let mut localized = false;

    for seg in segments {
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

    Ok((required, localized))
}

/// Parse subfield content based on the field type.
fn parse_subfield_content(
    field_type: &str,
    content: Option<String>,
) -> Result<(Vec<FieldStub>, Vec<BlockStub>, Vec<TabStub>)> {
    let Some(content) = content else {
        return Ok((Vec::new(), Vec::new(), Vec::new()));
    };

    if CONTAINER_TYPES.contains(&field_type) {
        return Ok((parse_fields_shorthand(&content)?, Vec::new(), Vec::new()));
    }
    if field_type == "blocks" {
        return Ok((Vec::new(), parse_block_entries(&content)?, Vec::new()));
    }
    if field_type == "tabs" {
        return Ok((Vec::new(), Vec::new(), parse_tab_entries(&content)?));
    }

    bail!(
        "Field type '{}' does not support subfields — only group, array, row, collapsible, blocks, and tabs do",
        field_type
    );
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

        let pipe_pos = part.find('|').ok_or_else(|| {
            anyhow!(
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

        blocks.push(BlockStub::new(block_type, label, fields));
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

        let Some(paren_pos) = part.find('(') else {
            bail!(
                "Tab entry '{}' missing '(fields)' — expected 'label(fields)'",
                part
            );
        };

        let label = part[..paren_pos].to_string();
        let paren_rest = &part[paren_pos..];
        let close = find_matching_paren(paren_rest)?;
        let content = &paren_rest[1..close];
        let fields = parse_fields_shorthand(content)?;

        if label.is_empty() {
            bail!("Tab label cannot be empty");
        }

        tabs.push(TabStub::new(label, fields));
    }

    if tabs.is_empty() {
        bail!("No tabs parsed from entries");
    }

    Ok(tabs)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Inflection ──────────────────────────────────────────────────────

    #[test]
    fn singularize_basic() {
        assert_eq!(singularize("categories"), "category");
        assert_eq!(singularize("stories"), "story");
        assert_eq!(singularize("addresses"), "address");
        assert_eq!(singularize("boxes"), "box");
        assert_eq!(singularize("buzzes"), "buzz");
        assert_eq!(singularize("dishes"), "dish");
        assert_eq!(singularize("watches"), "watch");
        assert_eq!(singularize("posts"), "post");
        assert_eq!(singularize("tags"), "tag");
        assert_eq!(singularize("address"), "address");
        assert_eq!(singularize("glass"), "glass");
        assert_eq!(singularize("s"), "s");
    }

    #[test]
    fn pluralize_basic() {
        assert_eq!(pluralize("Post"), "Posts");
        assert_eq!(pluralize("Category"), "Categories");
        assert_eq!(pluralize("Tag"), "Tags");
        assert_eq!(pluralize("Address"), "Addresses");
        assert_eq!(pluralize("Box"), "Boxes");
        assert_eq!(pluralize("Key"), "Keys");
        assert_eq!(pluralize(""), "");
        assert_eq!(pluralize("Quiz"), "Quizzes");
        assert_eq!(pluralize("Brush"), "Brushes");
        assert_eq!(pluralize("Church"), "Churches");
        assert_eq!(pluralize("Boy"), "Boys");
        assert_eq!(pluralize("Day"), "Days");
        assert_eq!(pluralize("Toy"), "Toys");
        assert_eq!(pluralize("Guy"), "Guys");
        assert_eq!(pluralize("Fuzz"), "Fuzzes");
        assert_eq!(pluralize("Buzz"), "Buzzes");
    }

    // ── Escape ──────────────────────────────────────────────────────────

    #[test]
    fn escape_lua_string_basic() {
        assert_eq!(escape_lua_string("hello"), "hello");
        assert_eq!(escape_lua_string(r#"say "hi""#), r#"say \"hi\""#);
        assert_eq!(escape_lua_string("line\nnew"), "line\\nnew");
        assert_eq!(escape_lua_string("back\\slash"), "back\\\\slash");
        assert_eq!(escape_lua_string("null\0byte"), "null\\0byte");
    }

    // ── Utilities ───────────────────────────────────────────────────────

    #[test]
    fn split_at_depth_zero_basic() {
        let parts = split_at_depth_zero("a,b(c,d),e", ',');
        assert_eq!(parts, vec!["a", "b(c,d)", "e"]);

        let parts = split_at_depth_zero("a:b(c:d):req", ':');
        assert_eq!(parts, vec!["a", "b(c:d)", "req"]);

        let parts = split_at_depth_zero("a(b(c,d),e),f", ',');
        assert_eq!(parts, vec!["a(b(c,d),e)", "f"]);
    }

    #[test]
    fn find_matching_paren_basic() {
        assert_eq!(find_matching_paren("(abc)").unwrap(), 4);
        assert_eq!(find_matching_paren("(a(b)c)").unwrap(), 6);
        assert!(find_matching_paren("(abc").is_err());
    }

    // ── Field shorthand parsing ─────────────────────────────────────────

    #[test]
    fn parse_basic_fields() {
        let fields =
            parse_fields_shorthand("title:text:required,body:textarea,published:checkbox").unwrap();
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0].name, "title");
        assert_eq!(fields[0].field_type, "text");
        assert!(fields[0].required);
        assert!(!fields[0].localized);
        assert_eq!(fields[1].name, "body");
        assert_eq!(fields[1].field_type, "textarea");
        assert_eq!(fields[2].name, "published");
    }

    #[test]
    fn parse_localized_fields() {
        let fields = parse_fields_shorthand("title:text:localized").unwrap();
        assert!(!fields[0].required);
        assert!(fields[0].localized);

        let fields = parse_fields_shorthand("title:text:required:localized").unwrap();
        assert!(fields[0].required);
        assert!(fields[0].localized);

        let fields = parse_fields_shorthand("title:text:localized:required").unwrap();
        assert!(fields[0].required);
        assert!(fields[0].localized);

        let fields =
            parse_fields_shorthand("title:text:required:localized,slug:text:required").unwrap();
        assert!(fields[0].localized);
        assert!(!fields[1].localized);
    }

    #[test]
    fn parse_invalid_fields() {
        assert!(parse_fields_shorthand("title").is_err());
        assert!(parse_fields_shorthand("title:unknown").is_err());
        assert!(parse_fields_shorthand("").is_err());
        assert!(parse_fields_shorthand("title:text:bogus").is_err());
    }

    #[test]
    fn parse_index_modifier() {
        let fields = parse_fields_shorthand("status:text:index").unwrap();
        assert_eq!(fields[0].name, "status");
        assert!(!fields[0].required);
    }

    #[test]
    fn parse_new_field_types() {
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
    fn parse_trailing_comma() {
        let fields = parse_fields_shorthand("title:text,body:textarea,").unwrap();
        assert_eq!(fields.len(), 2);
    }

    #[test]
    fn parse_whitespace_around_segments() {
        let fields = parse_fields_shorthand("  title:text:required  ").unwrap();
        assert_eq!(fields[0].name, "title");
        assert!(fields[0].required);

        assert!(parse_fields_shorthand("title : text").is_err());
    }

    #[test]
    fn parse_rejects_invalid_field_names() {
        assert!(parse_fields_shorthand(r#"test",evil=true,name="x:text"#).is_err());
        assert!(parse_fields_shorthand("bad name:text").is_err());
        assert!(parse_fields_shorthand("field-name:text").is_err());
        assert!(parse_fields_shorthand("field.name:text").is_err());
        assert!(parse_fields_shorthand("field;name:text").is_err());
    }

    // ── Nested parsing ──────────────────────────────────────────────────

    #[test]
    fn parse_nested_group() {
        let fields =
            parse_fields_shorthand("seo:group(meta_title:text,meta_desc:textarea):required")
                .unwrap();
        assert_eq!(fields[0].name, "seo");
        assert_eq!(fields[0].field_type, "group");
        assert!(fields[0].required);
        assert_eq!(fields[0].fields.len(), 2);
        assert_eq!(fields[0].fields[0].name, "meta_title");
        assert_eq!(fields[0].fields[1].name, "meta_desc");
    }

    #[test]
    fn parse_nested_array() {
        let fields =
            parse_fields_shorthand("variants:array(color:text:required,size:number)").unwrap();
        assert_eq!(fields[0].field_type, "array");
        assert_eq!(fields[0].fields.len(), 2);
        assert!(fields[0].fields[0].required);
    }

    #[test]
    fn parse_nested_blocks() {
        let fields = parse_fields_shorthand(
            "content:blocks(paragraph|Paragraph(body:textarea),hero|Hero(title:text,image:upload))",
        )
        .unwrap();
        assert_eq!(fields[0].blocks.len(), 2);
        assert_eq!(fields[0].blocks[0].block_type, "paragraph");
        assert_eq!(fields[0].blocks[0].label, "Paragraph");
        assert_eq!(fields[0].blocks[0].fields.len(), 1);
        assert_eq!(fields[0].blocks[1].fields.len(), 2);
    }

    #[test]
    fn parse_nested_tabs() {
        let fields = parse_fields_shorthand(
            "settings:tabs(General(name:text,email:email),Advanced(api_key:text))",
        )
        .unwrap();
        assert_eq!(fields[0].tabs.len(), 2);
        assert_eq!(fields[0].tabs[0].label, "General");
        assert_eq!(fields[0].tabs[0].fields.len(), 2);
        assert_eq!(fields[0].tabs[1].fields.len(), 1);
    }

    #[test]
    fn parse_deeply_nested() {
        let fields = parse_fields_shorthand(
            "variants:array(color:text,dimensions:group(width:number,height:number))",
        )
        .unwrap();
        assert_eq!(fields[0].fields[1].field_type, "group");
        assert_eq!(fields[0].fields[1].fields.len(), 2);
    }

    #[test]
    fn parse_deeply_nested_4_levels() {
        let fields =
            parse_fields_shorthand("a:array(b:group(c:collapsible(d:row(x:text))))").unwrap();
        assert_eq!(fields[0].fields[0].fields[0].fields[0].fields[0].name, "x");
    }

    #[test]
    fn parse_mixed_flat_and_nested() {
        let fields = parse_fields_shorthand(
            "title:text:required,seo:group(meta_title:text,meta_desc:textarea),body:richtext",
        )
        .unwrap();
        assert_eq!(fields.len(), 3);
        assert!(fields[0].fields.is_empty());
        assert_eq!(fields[1].fields.len(), 2);
    }

    #[test]
    fn parse_container_modifiers_with_subfields() {
        let fields = parse_fields_shorthand("seo:group(t:text):required:localized").unwrap();
        assert!(fields[0].required);
        assert!(fields[0].localized);
        assert_eq!(fields[0].fields.len(), 1);
    }

    #[test]
    fn parse_single_tab() {
        let fields = parse_fields_shorthand("settings:tabs(General(name:text))").unwrap();
        assert_eq!(fields[0].tabs.len(), 1);
        assert_eq!(fields[0].tabs[0].fields[0].name, "name");
    }

    // ── Parse errors ────────────────────────────────────────────────────

    #[test]
    fn parse_error_unbalanced_parens() {
        assert!(parse_fields_shorthand("seo:group(title:text").is_err());
    }

    #[test]
    fn parse_error_subfields_on_non_container() {
        assert!(parse_fields_shorthand("title:text(sub:number)").is_err());
    }

    #[test]
    fn parse_error_missing_block_label() {
        assert!(parse_fields_shorthand("content:blocks(paragraph(body:textarea))").is_err());
    }

    #[test]
    fn parse_error_empty_parens_container() {
        assert!(parse_fields_shorthand("items:array()").is_err());
    }

    #[test]
    fn parse_error_blocks_empty_fields() {
        assert!(parse_fields_shorthand("content:blocks(para|Paragraph())").is_err());
    }

    #[test]
    fn parse_error_blocks_empty_type() {
        assert!(parse_fields_shorthand("content:blocks(|Paragraph(body:textarea))").is_err());
    }

    #[test]
    fn parse_error_tabs_empty_label() {
        assert!(parse_fields_shorthand("settings:tabs((name:text))").is_err());
    }
}
