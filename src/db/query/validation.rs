//! Query field validation: identifier checks, filter field validation.

use std::collections::HashSet;

use anyhow::{Result, bail};

use crate::{
    core::{CollectionDefinition, FieldDefinition, FieldType},
    db::{FilterClause, FindQuery, LocaleContext},
};

use super::columns::get_valid_filter_columns;

/// Check that a string is a safe SQL identifier (alphanumeric + underscore).
#[must_use]
pub fn is_valid_identifier(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Sanitize a locale string for safe use in SQL identifiers (column names, defaults).
/// Converts dashes to underscores (e.g. "de-DE" → "`de_DE`") and strips anything
/// except alphanumeric + underscore.
///
/// # Errors
///
/// Returns an error if the input contains no alphanumeric or underscore characters.
pub fn sanitize_locale(locale: &str) -> Result<String> {
    let result: String = locale
        .chars()
        .map(|c| if c == '-' { '_' } else { c })
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();

    if result.is_empty() {
        bail!("sanitize_locale produced empty string from input: {locale:?}");
    }

    Ok(result)
}

/// Validate a slug: lowercase alphanumeric + underscores, not empty, no leading underscore.
///
/// Used for slugs that map to SQL identifiers, Lua identifiers, or
/// dotted-access keys (collection names, global names, field/node/job
/// names). For URL- and filename-style slugs that allow hyphens
/// (custom pages, slots, themes), use [`validate_template_slug`].
///
/// # Errors
///
/// Returns an error if the slug is empty, starts with an underscore, or
/// contains characters other than lowercase ASCII letters, digits, and `_`.
pub fn validate_slug(slug: &str) -> Result<()> {
    if slug.is_empty() {
        bail!("Slug cannot be empty");
    }

    if !slug
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        bail!("Invalid slug '{slug}' — use lowercase letters, digits, and underscores only");
    }

    if slug.starts_with('_') {
        bail!("Slug cannot start with underscore");
    }

    Ok(())
}

/// Slug prefixes reserved because they would make an MCP tool name ambiguous.
/// MCP collection tools are named `{op}_{slug}`, and `{op}` includes the
/// compound forms `create_many_`, `update_many_`, `delete_many_`, and
/// `find_by_id_`. A slug beginning `many_` or `by_id_` collides with another
/// collection's bulk / find-by-id tool (e.g. `create_many_posts` is both
/// `CreateMany`/`posts` and `Create`/`many_posts`), making one unreachable.
const RESERVED_TOOL_SLUG_PREFIXES: &[&str] = &["many_", "by_id_"];

/// Reject a collection/global slug that begins with a reserved MCP-tool prefix.
///
/// # Errors
///
/// Returns an error if `slug` starts with `many_` or `by_id_`.
pub fn reject_reserved_tool_prefix(slug: &str) -> Result<()> {
    for prefix in RESERVED_TOOL_SLUG_PREFIXES {
        if slug.starts_with(prefix) {
            bail!(
                "Invalid slug '{slug}' — cannot begin with the reserved prefix '{prefix}' \
                 (it would make MCP tool names like 'create_many_...' ambiguous)"
            );
        }
    }

    Ok(())
}

/// Validate a URL/filename-style slug: lowercase alphanumeric, hyphens,
/// underscores. Used by scaffold targets whose slug maps to a file path
/// or URL segment (custom pages, slots, themes) — not to a Lua or SQL
/// identifier. Strictly tighter than the runtime `is_valid_slug` in
/// `admin::custom_pages` so scaffold output is always a subset of what
/// the runtime accepts.
///
/// Rules:
/// - non-empty
/// - only `[a-z0-9_-]`
/// - cannot start with `-` or `_`
/// - cannot end with `-`
/// - no `--` or `__` runs
///
/// # Errors
///
/// Returns an error if any of the rules above are violated.
pub fn validate_template_slug(slug: &str) -> Result<()> {
    if slug.is_empty() {
        bail!("Slug cannot be empty");
    }

    if !slug
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        bail!("Invalid slug '{slug}' — use lowercase letters, digits, '-', and '_' only");
    }

    if slug.starts_with('-') || slug.starts_with('_') {
        bail!("Slug '{slug}' cannot start with '-' or '_'");
    }

    if slug.ends_with('-') {
        bail!("Slug '{slug}' cannot end with '-'");
    }

    if slug.contains("--") || slug.contains("__") {
        bail!("Slug '{slug}' cannot contain '--' or '__'");
    }

    Ok(())
}

/// Validate that a field name exists in the set of valid columns.
///
/// # Errors
///
/// Returns an error if the field name is not present in `valid_columns`.
pub fn validate_field_name(field: &str, valid_columns: &HashSet<String>) -> Result<()> {
    if !valid_columns.contains(field) {
        bail!(
            "Invalid field '{}'. Valid fields: {}",
            field,
            valid_columns.iter().cloned().collect::<Vec<_>>().join(", ")
        );
    }

    Ok(())
}

/// Validate all filter fields and `order_by` in a `FindQuery` against a collection definition.
///
/// Filter fields support dot notation for array/block/relationship sub-fields
/// (e.g., `items.name`, `content.body`, `tags.id`). The first segment must match
/// a known field; deeper segments are validated at SQL generation time.
///
/// `order_by` only supports flat columns (no dot notation).
///
/// # Errors
///
/// Returns an error if any filter field or `order_by` references a column
/// that does not exist on the collection.
pub fn validate_query_fields(
    def: &CollectionDefinition,
    query: &FindQuery,
    locale_ctx: Option<&LocaleContext>,
) -> Result<()> {
    let (exact_columns, prefix_roots) = get_valid_filter_paths(def, locale_ctx);

    for clause in &query.filters {
        validate_clause_fields(clause, &exact_columns, &prefix_roots)?;
    }

    // order_by only supports flat columns (no sub-field sorting).
    // `_rank` is the one virtual sort: relevance order for the current
    // `search` term (best first). It is only meaningful with a search and
    // has no stable column, so cursor pagination cannot encode it.
    if let Some(ref order) = query.order_by {
        if order == "_rank" {
            if query.search.as_deref().is_none_or(str::is_empty) {
                bail!("order_by '_rank' requires a 'search' term — it sorts by search relevance");
            }
            if query.after_cursor.is_some() || query.before_cursor.is_some() {
                bail!(
                    "order_by '_rank' cannot be combined with cursor pagination — relevance is not cursor-stable; use page/offset pagination"
                );
            }
            return Ok(());
        }
        if order == "-_rank" {
            bail!("order_by '_rank' is always best-first — drop the '-' prefix");
        }

        let col = order.strip_prefix('-').unwrap_or(order);

        validate_field_name(col, &exact_columns)?;
    }

    Ok(())
}

/// Get valid filter paths: exact column names + prefix roots for dot notation.
///
/// Returns `(exact_columns, prefix_roots)` where:
/// - `exact_columns`: flat column names valid for filtering and `order_by`
/// - `prefix_roots`: field names that accept dot-path sub-filters (Array, Blocks, has-many Relationship)
#[must_use]
pub fn get_valid_filter_paths(
    def: &CollectionDefinition,
    locale_ctx: Option<&LocaleContext>,
) -> (HashSet<String>, HashSet<String>) {
    let exact = get_valid_filter_columns(def, locale_ctx);
    let mut prefixes = HashSet::new();

    collect_prefix_roots(&def.fields, &mut prefixes);

    (exact, prefixes)
}

/// Recursively collect Array/Blocks/has-many Relationship field names,
/// descending into transparent layout wrappers (Row, Collapsible, Tabs).
fn collect_prefix_roots(fields: &[FieldDefinition], prefixes: &mut HashSet<String>) {
    for field in fields {
        match field.field_type {
            FieldType::Array | FieldType::Blocks => {
                prefixes.insert(field.name.clone());
            }
            FieldType::Relationship => {
                if let Some(ref rc) = field.relationship
                    && rc.has_many
                {
                    prefixes.insert(field.name.clone());
                }
            }
            FieldType::Row | FieldType::Collapsible => {
                collect_prefix_roots(&field.fields, prefixes);
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    collect_prefix_roots(&tab.fields, prefixes);
                }
            }
            _ => {}
        }
    }
}

/// Validate a single filter field name against exact columns or dot-path prefixes.
pub(crate) fn validate_filter_field(
    field: &str,
    exact_columns: &HashSet<String>,
    prefix_roots: &HashSet<String>,
) -> Result<()> {
    // System columns (`_*`) are engine-internal. Let them pass SQL-shape
    // validation here — the service layer runs its own `validate_user_filters`
    // check first and returns a user-friendly "system column" error for
    // anything that actually hits this path.
    let first = field.split('.').next().unwrap_or(field);
    if first.starts_with('_') {
        return Ok(());
    }

    // Exact match — flat column name
    if exact_columns.contains(field) {
        return Ok(());
    }

    // Dot notation — check if the first segment is a valid prefix root
    if let Some(dot_pos) = field.find('.') {
        let root = &field[..dot_pos];

        if prefix_roots.contains(root) {
            return Ok(());
        }
    }

    bail!("Invalid field '{}'. Valid fields: {}", field, {
        let mut all: Vec<String> = exact_columns.iter().cloned().collect();

        for p in prefix_roots {
            all.push(format!("{p}.*"));
        }

        all.sort();
        all.join(", ")
    })
}

/// Recursively validate every leaf field in a [`FilterClause`] tree against the
/// collection's valid filter paths. Shared by `validate_query_fields` and the
/// count path so both walk the tree identically.
pub(crate) fn validate_clause_fields(
    clause: &FilterClause,
    exact_columns: &HashSet<String>,
    prefix_roots: &HashSet<String>,
) -> Result<()> {
    match clause {
        FilterClause::Single(f) => validate_filter_field(&f.field, exact_columns, prefix_roots),
        FilterClause::And(subs) | FilterClause::Or(subs) => {
            for c in subs {
                validate_clause_fields(c, exact_columns, prefix_roots)?;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{
        CollectionDefinition, FieldDefinition, FieldTab, FieldType, RelationshipConfig,
    };

    #[test]
    fn is_valid_identifier_accepts_valid() {
        assert!(is_valid_identifier("title"));
        assert!(is_valid_identifier("created_at"));
        assert!(is_valid_identifier("field_123"));
        assert!(is_valid_identifier("id"));
    }

    #[test]
    fn is_valid_identifier_rejects_invalid() {
        assert!(!is_valid_identifier(""));
        assert!(!is_valid_identifier("field name"));
        assert!(!is_valid_identifier("1=1; DROP TABLE posts; --"));
        assert!(!is_valid_identifier("field-name"));
        assert!(!is_valid_identifier("field.name"));
        assert!(!is_valid_identifier("field;name"));
    }

    #[test]
    fn validate_field_name_accepts_known() {
        let valid: HashSet<String> = ["id", "title", "status"]
            .iter()
            .map(std::string::ToString::to_string)
            .collect();
        assert!(validate_field_name("title", &valid).is_ok());
        assert!(validate_field_name("id", &valid).is_ok());
    }

    #[test]
    fn validate_field_name_rejects_unknown() {
        let valid: HashSet<String> = ["id", "title", "status"]
            .iter()
            .map(std::string::ToString::to_string)
            .collect();
        let err = validate_field_name("nonexistent", &valid).unwrap_err();
        assert!(err.to_string().contains("Invalid field 'nonexistent'"));
    }

    #[test]
    fn sanitize_locale_strips_dangerous_chars() {
        assert_eq!(sanitize_locale("en").unwrap(), "en");
        assert_eq!(sanitize_locale("de-DE").unwrap(), "de_DE");
        assert_eq!(sanitize_locale("en_US").unwrap(), "en_US");
        // Dashes map to underscores, everything else non-alphanumeric is stripped
        assert_eq!(sanitize_locale("'; DROP TABLE --").unwrap(), "DROPTABLE__");
    }

    #[test]
    fn sanitize_locale_pathological_input_returns_error() {
        let result = sanitize_locale("!@#$%^&*()");
        assert!(result.is_err());
    }

    #[test]
    fn field_name_double_underscore_is_valid_identifier_but_reserved() {
        // Double underscores pass is_valid_identifier but are reserved for group naming
        assert!(is_valid_identifier("seo__title"));
        // The rejection happens at a higher level (field name validation in schema parsing)
    }

    #[test]
    fn validate_slug_accepts_valid() {
        assert!(validate_slug("posts").is_ok());
        assert!(validate_slug("site_settings").is_ok());
        assert!(validate_slug("v2_users").is_ok());
    }

    #[test]
    fn validate_slug_rejects_invalid() {
        assert!(validate_slug("").is_err());
        assert!(validate_slug("Posts").is_err());
        assert!(validate_slug("my-slug").is_err());
        assert!(validate_slug("_private").is_err());
    }

    #[test]
    fn reject_reserved_tool_prefix_blocks_many_and_by_id() {
        // These would make MCP tool names like `create_many_posts` /
        // `find_by_id_x` ambiguous.
        assert!(reject_reserved_tool_prefix("many_posts").is_err());
        assert!(reject_reserved_tool_prefix("by_id_users").is_err());
        // A normal slug (and one that merely contains, not starts-with, the
        // token) is fine.
        assert!(reject_reserved_tool_prefix("posts").is_ok());
        assert!(reject_reserved_tool_prefix("company_by_id").is_ok());
        assert!(reject_reserved_tool_prefix("germany_news").is_ok());
    }

    #[test]
    fn validate_template_slug_accepts_lowercase_with_hyphens_and_underscores() {
        assert!(validate_template_slug("status").is_ok());
        assert!(validate_template_slug("system-status").is_ok());
        assert!(validate_template_slug("my_widget").is_ok());
        assert!(validate_template_slug("v2-users").is_ok());
        assert!(validate_template_slug("a").is_ok());
    }

    #[test]
    fn validate_template_slug_rejects_invalid() {
        // Empty, charset violations, and edge positions.
        assert!(validate_template_slug("").is_err());
        assert!(validate_template_slug("Status").is_err());
        assert!(validate_template_slug("with space").is_err());
        assert!(validate_template_slug("dot.dot").is_err());
        assert!(validate_template_slug("../etc").is_err());
        assert!(validate_template_slug("with/slash").is_err());
        // Leading/trailing/double separators.
        assert!(validate_template_slug("-leading").is_err());
        assert!(validate_template_slug("_leading").is_err());
        assert!(validate_template_slug("trailing-").is_err());
        assert!(validate_template_slug("double--hyphen").is_err());
        assert!(validate_template_slug("double__underscore").is_err());
    }

    /// Regression: `get_valid_filter_paths` did not recurse into layout wrappers,
    /// so Array/Blocks fields inside Row/Tabs/Collapsible were rejected as invalid.
    #[test]
    fn filter_paths_include_array_inside_layout_wrappers() {
        // Array inside a Row
        let def = CollectionDefinition::builder("test")
            .fields(vec![
                FieldDefinition::builder("layout", FieldType::Row)
                    .fields(vec![
                        FieldDefinition::builder("items", FieldType::Array)
                            .fields(vec![
                                FieldDefinition::builder("name", FieldType::Text).build(),
                            ])
                            .build(),
                    ])
                    .build(),
            ])
            .build();

        let (_, prefixes) = get_valid_filter_paths(&def, None);
        assert!(
            prefixes.contains("items"),
            "Array inside Row should be a valid filter prefix root"
        );

        // has-many Relationship inside Tabs
        let def = CollectionDefinition::builder("test")
            .fields(vec![
                FieldDefinition::builder("tabbed", FieldType::Tabs)
                    .tabs(vec![FieldTab::new(
                        "main",
                        vec![
                            FieldDefinition::builder("tags", FieldType::Relationship)
                                .relationship(RelationshipConfig {
                                    collection: "tags".into(),
                                    has_many: true,
                                    max_depth: None,
                                    polymorphic: vec![],
                                })
                                .build(),
                        ],
                    )])
                    .build(),
            ])
            .build();

        let (_, prefixes) = get_valid_filter_paths(&def, None);
        assert!(
            prefixes.contains("tags"),
            "has-many Relationship inside Tabs should be a valid filter prefix root"
        );
    }
}
