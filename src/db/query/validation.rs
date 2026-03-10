//! Query field validation: identifier checks, filter field validation.

use std::collections::HashSet;

use anyhow::{Result, bail};

use crate::core::CollectionDefinition;
use crate::core::field::FieldType;

use super::columns::get_valid_filter_columns;
use super::locale::LocaleContext;
use super::types::{FilterClause, FindQuery};

/// Check that a string is a safe SQL identifier (alphanumeric + underscore).
pub fn is_valid_identifier(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Sanitize a locale string for safe use in SQL identifiers (column names, defaults).
/// Only allows alphanumeric characters, underscores, and dashes.
pub fn sanitize_locale(locale: &str) -> String {
    locale.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect()
}

/// Validate a slug: lowercase alphanumeric + underscores, not empty, no leading underscore.
pub fn validate_slug(slug: &str) -> Result<()> {
    if slug.is_empty() {
        bail!("Slug cannot be empty");
    }
    if !slug.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
        bail!(
            "Invalid slug '{}' — use lowercase letters, digits, and underscores only",
            slug
        );
    }
    if slug.starts_with('_') {
        bail!("Slug cannot start with underscore");
    }
    Ok(())
}

/// Validate that a field name exists in the set of valid columns.
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

/// Validate all filter fields and order_by in a FindQuery against a collection definition.
///
/// Filter fields support dot notation for array/block/relationship sub-fields
/// (e.g., `items.name`, `content.body`, `tags.id`). The first segment must match
/// a known field; deeper segments are validated at SQL generation time.
///
/// `order_by` only supports flat columns (no dot notation).
pub fn validate_query_fields(def: &CollectionDefinition, query: &FindQuery, locale_ctx: Option<&LocaleContext>) -> Result<()> {
    let (exact_columns, prefix_roots) = get_valid_filter_paths(def, locale_ctx);

    for clause in &query.filters {
        match clause {
            FilterClause::Single(f) => validate_filter_field(&f.field, &exact_columns, &prefix_roots)?,
            FilterClause::Or(groups) => {
                for group in groups {
                    for f in group {
                        validate_filter_field(&f.field, &exact_columns, &prefix_roots)?;
                    }
                }
            }
        }
    }

    // order_by only supports flat columns (no sub-field sorting)
    if let Some(ref order) = query.order_by {
        let col = order.strip_prefix('-').unwrap_or(order);
        validate_field_name(col, &exact_columns)?;
    }

    Ok(())
}

/// Get valid filter paths: exact column names + prefix roots for dot notation.
///
/// Returns `(exact_columns, prefix_roots)` where:
/// - `exact_columns`: flat column names valid for filtering and order_by
/// - `prefix_roots`: field names that accept dot-path sub-filters (Array, Blocks, has-many Relationship)
pub fn get_valid_filter_paths(def: &CollectionDefinition, locale_ctx: Option<&LocaleContext>) -> (HashSet<String>, HashSet<String>) {
    let exact = get_valid_filter_columns(def, locale_ctx);
    let mut prefixes = HashSet::new();

    for field in &def.fields {
        match field.field_type {
            FieldType::Array | FieldType::Blocks => {
                prefixes.insert(field.name.clone());
            }
            FieldType::Relationship => {
                if let Some(ref rc) = field.relationship {
                    if rc.has_many {
                        prefixes.insert(field.name.clone());
                    }
                }
            }
            _ => {}
        }
    }

    (exact, prefixes)
}

/// Validate a single filter field name against exact columns or dot-path prefixes.
pub(crate) fn validate_filter_field(field: &str, exact_columns: &HashSet<String>, prefix_roots: &HashSet<String>) -> Result<()> {
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
    bail!(
        "Invalid field '{}'. Valid fields: {}",
        field,
        {
            let mut all: Vec<String> = exact_columns.iter().cloned().collect();
            for p in prefix_roots {
                all.push(format!("{}.*", p));
            }
            all.sort();
            all.join(", ")
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
            .iter().map(|s| s.to_string()).collect();
        assert!(validate_field_name("title", &valid).is_ok());
        assert!(validate_field_name("id", &valid).is_ok());
    }

    #[test]
    fn validate_field_name_rejects_unknown() {
        let valid: HashSet<String> = ["id", "title", "status"]
            .iter().map(|s| s.to_string()).collect();
        let err = validate_field_name("nonexistent", &valid).unwrap_err();
        assert!(err.to_string().contains("Invalid field 'nonexistent'"));
    }

    #[test]
    fn sanitize_locale_strips_dangerous_chars() {
        assert_eq!(sanitize_locale("en"), "en");
        assert_eq!(sanitize_locale("de-DE"), "de-DE");
        assert_eq!(sanitize_locale("en_US"), "en_US");
        assert_eq!(sanitize_locale("'; DROP TABLE --"), "DROPTABLE--");
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
}
