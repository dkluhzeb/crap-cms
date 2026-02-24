//! SQL filter/WHERE clause building and locale column resolution.

use anyhow::{Result, bail};

use crate::core::CollectionDefinition;
use crate::core::field::FieldType;
use super::{LocaleMode, LocaleContext, Filter, FilterClause, FilterOp, is_valid_identifier};

/// Build a single [`Filter`] into a SQL condition string and append its bind
/// parameters to `params`.
///
/// Each [`FilterOp`] variant maps to a SQL expression:
/// - `Equals` / `NotEquals` / `Like` / `GreaterThan` / `LessThan` /
///   `GreaterThanOrEqual` / `LessThanOrEqual` — single-placeholder comparisons.
/// - `Contains` — wraps the value in `%…%` with `LIKE ? ESCAPE '\'`, escaping
///   literal `%` and `_` in the value.
/// - `In` / `NotIn` — expands to `field IN (?, ?, …)`.
/// - `Exists` / `NotExists` — `IS NOT NULL` / `IS NULL` (no bind parameter).
///
/// Defense-in-depth: rejects field names that are not valid SQL identifiers
/// (alphanumeric + underscore), even though higher-level validation should
/// have caught them already.
///
/// # Errors
///
/// Returns an error if `f.field` contains characters other than ASCII
/// alphanumerics and underscores.
pub fn build_filter_condition(f: &Filter, params: &mut Vec<Box<dyn rusqlite::types::ToSql>>) -> Result<String> {
    if !is_valid_identifier(&f.field) {
        bail!("Invalid field name '{}': must be alphanumeric/underscore", f.field);
    }
    Ok(match &f.op {
        FilterOp::Equals(v) => {
            params.push(Box::new(v.clone()));
            format!("{} = ?", f.field)
        }
        FilterOp::NotEquals(v) => {
            params.push(Box::new(v.clone()));
            format!("{} != ?", f.field)
        }
        FilterOp::Like(v) => {
            params.push(Box::new(v.clone()));
            format!("{} LIKE ?", f.field)
        }
        FilterOp::Contains(v) => {
            let escaped = v.replace('%', "\\%").replace('_', "\\_");
            params.push(Box::new(format!("%{}%", escaped)));
            format!("{} LIKE ? ESCAPE '\\'", f.field)
        }
        FilterOp::GreaterThan(v) => {
            params.push(Box::new(v.clone()));
            format!("{} > ?", f.field)
        }
        FilterOp::LessThan(v) => {
            params.push(Box::new(v.clone()));
            format!("{} < ?", f.field)
        }
        FilterOp::GreaterThanOrEqual(v) => {
            params.push(Box::new(v.clone()));
            format!("{} >= ?", f.field)
        }
        FilterOp::LessThanOrEqual(v) => {
            params.push(Box::new(v.clone()));
            format!("{} <= ?", f.field)
        }
        FilterOp::In(vals) => {
            let placeholders: Vec<_> = vals.iter().map(|v| {
                params.push(Box::new(v.clone()));
                "?".to_string()
            }).collect();
            format!("{} IN ({})", f.field, placeholders.join(", "))
        }
        FilterOp::NotIn(vals) => {
            let placeholders: Vec<_> = vals.iter().map(|v| {
                params.push(Box::new(v.clone()));
                "?".to_string()
            }).collect();
            format!("{} NOT IN ({})", f.field, placeholders.join(", "))
        }
        FilterOp::Exists => {
            format!("{} IS NOT NULL", f.field)
        }
        FilterOp::NotExists => {
            format!("{} IS NULL", f.field)
        }
    })
}

/// Build a complete ` WHERE …` clause from a slice of [`FilterClause`]s.
///
/// Top-level clauses are joined with `AND`. An [`FilterClause::Or`] group
/// produces `(a OR b OR (c AND d))` sub-expressions, while
/// [`FilterClause::Single`] produces a plain condition.
///
/// Returns an **empty string** when `filters` is empty (no WHERE at all),
/// so callers can unconditionally append the result to their query.
///
/// # Errors
///
/// Propagates any error from [`build_filter_condition`] (invalid field names).
pub fn build_where_clause(filters: &[FilterClause], params: &mut Vec<Box<dyn rusqlite::types::ToSql>>) -> Result<String> {
    if filters.is_empty() {
        return Ok(String::new());
    }

    let mut conditions = Vec::new();
    for clause in filters {
        match clause {
            FilterClause::Single(f) => {
                conditions.push(build_filter_condition(f, params)?);
            }
            FilterClause::Or(groups) => {
                if groups.len() == 1 && groups[0].len() == 1 {
                    conditions.push(build_filter_condition(&groups[0][0], params)?);
                } else {
                    let mut or_parts = Vec::new();
                    for group in groups {
                        if group.len() == 1 {
                            or_parts.push(build_filter_condition(&group[0], params)?);
                        } else {
                            let and_parts: Vec<String> = group.iter()
                                .map(|f| build_filter_condition(f, params))
                                .collect::<Result<_, _>>()?;
                            or_parts.push(format!("({})", and_parts.join(" AND ")));
                        }
                    }
                    conditions.push(format!("({})", or_parts.join(" OR ")));
                }
            }
        }
    }

    Ok(format!(" WHERE {}", conditions.join(" AND ")))
}

/// Resolve filter clauses to use locale-specific column names.
///
/// Walks every [`FilterClause`] (including nested OR groups) and replaces each
/// filter's field name with the locale-suffixed column name returned by
/// [`resolve_filter_column`]. Non-localized fields pass through unchanged.
///
/// This is a pure transformation — no database access required.
pub fn resolve_filters(filters: &[FilterClause], def: &CollectionDefinition, locale_ctx: Option<&LocaleContext>) -> Vec<FilterClause> {
    filters.iter().map(|clause| {
        match clause {
            FilterClause::Single(f) => {
                let resolved = resolve_filter_column(&f.field, def, locale_ctx);
                FilterClause::Single(Filter { field: resolved, op: f.op.clone() })
            }
            FilterClause::Or(groups) => {
                FilterClause::Or(groups.iter().map(|group| {
                    group.iter().map(|f| {
                        let resolved = resolve_filter_column(&f.field, def, locale_ctx);
                        Filter { field: resolved, op: f.op.clone() }
                    }).collect()
                }).collect())
            }
        }
    }).collect()
}

/// Map a filter field name to its actual SQL column name, accounting for locale.
///
/// When a [`LocaleContext`] is present and localization is enabled:
/// - If the field is a localized top-level field, returns `"field__locale"`.
/// - If the field is a group sub-field (`"group__sub"`) where either the group
///   or the sub-field is localized, returns `"group__sub__locale"`.
/// - For [`LocaleMode::Single`] the requested locale is used; otherwise the
///   default locale from config is used.
///
/// Non-localized fields (or disabled locale config) pass through unchanged.
pub fn resolve_filter_column(field_name: &str, def: &CollectionDefinition, locale_ctx: Option<&LocaleContext>) -> String {
    if let Some(ctx) = locale_ctx {
        if ctx.config.is_enabled() {
            // Check if this field is localized
            for field in &def.fields {
                if field.field_type == FieldType::Group {
                    let prefix = format!("{}__{}", field.name, "");
                    if field_name.starts_with(&prefix) {
                        let sub_name = &field_name[prefix.len()..];
                        for sub in &field.fields {
                            if sub.name == sub_name && (field.localized || sub.localized) {
                                let locale = match &ctx.mode {
                                    LocaleMode::Single(l) => l.as_str(),
                                    _ => ctx.config.default_locale.as_str(),
                                };
                                return format!("{}__{}", field_name, locale);
                            }
                        }
                    }
                } else if field.name == field_name && field.localized {
                    let locale = match &ctx.mode {
                        LocaleMode::Single(l) => l.as_str(),
                        _ => ctx.config.default_locale.as_str(),
                    };
                    return format!("{}__{}", field_name, locale);
                }
            }
        }
    }
    field_name.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LocaleConfig;
    use crate::core::collection::{
        CollectionAccess, CollectionAdmin, CollectionDefinition, CollectionHooks, CollectionLabels,
    };
    use crate::core::field::{FieldAccess, FieldAdmin, FieldDefinition, FieldHooks, FieldType};
    use crate::db::query::{FilterClause, FilterOp, Filter, LocaleContext, LocaleMode};

    // ── Helpers ────────────────────────────────────────────────────────────

    /// Build a minimal `FieldDefinition` with the given name, type, and localized flag.
    fn make_field(name: &str, ft: FieldType, localized: bool) -> FieldDefinition {
        FieldDefinition {
            name: name.to_string(),
            field_type: ft,
            required: false,
            unique: false,
            validate: None,
            default_value: None,
            options: Vec::new(),
            admin: FieldAdmin::default(),
            hooks: FieldHooks::default(),
            access: FieldAccess::default(),
            relationship: None,
            fields: Vec::new(),
            blocks: Vec::new(),
            localized,
        }
    }

    /// Build a `CollectionDefinition` with the given fields.
    fn make_collection(fields: Vec<FieldDefinition>) -> CollectionDefinition {
        CollectionDefinition {
            slug: "test".to_string(),
            labels: CollectionLabels::default(),
            timestamps: true,
            fields,
            admin: CollectionAdmin::default(),
            hooks: CollectionHooks::default(),
            auth: None,
            upload: None,
            access: CollectionAccess::default(),
            live: None,
            versions: None,
        }
    }

    /// Build a `LocaleConfig` with en + de locales enabled.
    fn locale_config_en_de() -> LocaleConfig {
        LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        }
    }

    // ── build_filter_condition ─────────────────────────────────────────────

    #[test]
    fn filter_condition_equals() {
        let f = Filter { field: "status".into(), op: FilterOp::Equals("active".into()) };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_condition(&f, &mut params).unwrap();
        assert_eq!(sql, "status = ?");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn filter_condition_not_equals() {
        let f = Filter { field: "status".into(), op: FilterOp::NotEquals("draft".into()) };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_condition(&f, &mut params).unwrap();
        assert_eq!(sql, "status != ?");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn filter_condition_like() {
        let f = Filter { field: "title".into(), op: FilterOp::Like("%hello%".into()) };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_condition(&f, &mut params).unwrap();
        assert_eq!(sql, "title LIKE ?");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn filter_condition_contains() {
        let f = Filter { field: "body".into(), op: FilterOp::Contains("search term".into()) };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_condition(&f, &mut params).unwrap();
        assert_eq!(sql, "body LIKE ? ESCAPE '\\'");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn filter_condition_greater_than() {
        let f = Filter { field: "age".into(), op: FilterOp::GreaterThan("18".into()) };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_condition(&f, &mut params).unwrap();
        assert_eq!(sql, "age > ?");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn filter_condition_less_than() {
        let f = Filter { field: "price".into(), op: FilterOp::LessThan("100".into()) };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_condition(&f, &mut params).unwrap();
        assert_eq!(sql, "price < ?");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn filter_condition_greater_than_or_equal() {
        let f = Filter { field: "score".into(), op: FilterOp::GreaterThanOrEqual("50".into()) };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_condition(&f, &mut params).unwrap();
        assert_eq!(sql, "score >= ?");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn filter_condition_less_than_or_equal() {
        let f = Filter { field: "rating".into(), op: FilterOp::LessThanOrEqual("5".into()) };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_condition(&f, &mut params).unwrap();
        assert_eq!(sql, "rating <= ?");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn filter_condition_in() {
        let f = Filter {
            field: "status".into(),
            op: FilterOp::In(vec!["a".into(), "b".into(), "c".into()]),
        };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_condition(&f, &mut params).unwrap();
        assert_eq!(sql, "status IN (?, ?, ?)");
        assert_eq!(params.len(), 3);
    }

    #[test]
    fn filter_condition_not_in() {
        let f = Filter {
            field: "role".into(),
            op: FilterOp::NotIn(vec!["banned".into(), "suspended".into()]),
        };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_condition(&f, &mut params).unwrap();
        assert_eq!(sql, "role NOT IN (?, ?)");
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn filter_condition_exists() {
        let f = Filter { field: "avatar".into(), op: FilterOp::Exists };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_condition(&f, &mut params).unwrap();
        assert_eq!(sql, "avatar IS NOT NULL");
        assert_eq!(params.len(), 0);
    }

    #[test]
    fn filter_condition_not_exists() {
        let f = Filter { field: "deleted_at".into(), op: FilterOp::NotExists };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_condition(&f, &mut params).unwrap();
        assert_eq!(sql, "deleted_at IS NULL");
        assert_eq!(params.len(), 0);
    }

    #[test]
    fn filter_condition_rejects_invalid_identifier() {
        let f = Filter { field: "field name".into(), op: FilterOp::Equals("v".into()) };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let result = build_filter_condition(&f, &mut params);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid field name"));
    }

    // ── build_where_clause ────────────────────────────────────────────────

    #[test]
    fn where_clause_empty_filters() {
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_where_clause(&[], &mut params).unwrap();
        assert_eq!(sql, "");
        assert_eq!(params.len(), 0);
    }

    #[test]
    fn where_clause_single_filter() {
        let filters = vec![
            FilterClause::Single(Filter { field: "status".into(), op: FilterOp::Equals("active".into()) }),
        ];
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_where_clause(&filters, &mut params).unwrap();
        assert_eq!(sql, " WHERE status = ?");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn where_clause_multiple_and() {
        let filters = vec![
            FilterClause::Single(Filter { field: "status".into(), op: FilterOp::Equals("active".into()) }),
            FilterClause::Single(Filter { field: "role".into(), op: FilterOp::Equals("admin".into()) }),
        ];
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_where_clause(&filters, &mut params).unwrap();
        assert_eq!(sql, " WHERE status = ? AND role = ?");
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn where_clause_or_groups() {
        // Build: WHERE (a = ? OR (b = ? AND c = ?))
        let filters = vec![
            FilterClause::Or(vec![
                // First OR branch: single filter a = ?
                vec![Filter { field: "a".into(), op: FilterOp::Equals("1".into()) }],
                // Second OR branch: b = ? AND c = ?
                vec![
                    Filter { field: "b".into(), op: FilterOp::Equals("2".into()) },
                    Filter { field: "c".into(), op: FilterOp::Equals("3".into()) },
                ],
            ]),
        ];
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_where_clause(&filters, &mut params).unwrap();
        assert_eq!(sql, " WHERE (a = ? OR (b = ? AND c = ?))");
        assert_eq!(params.len(), 3);
    }

    // ── resolve_filter_column ─────────────────────────────────────────────

    #[test]
    fn resolve_column_non_localized_passthrough() {
        let def = make_collection(vec![make_field("title", FieldType::Text, false)]);
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".into()),
            config: locale_config_en_de(),
        };
        let result = resolve_filter_column("title", &def, Some(&ctx));
        assert_eq!(result, "title");
    }

    #[test]
    fn resolve_column_localized_single_locale() {
        let def = make_collection(vec![make_field("title", FieldType::Text, true)]);
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".into()),
            config: locale_config_en_de(),
        };
        let result = resolve_filter_column("title", &def, Some(&ctx));
        assert_eq!(result, "title__de");
    }

    #[test]
    fn resolve_column_group_sub_field_localized() {
        let mut group = make_field("meta", FieldType::Group, false);
        let sub = make_field("description", FieldType::Text, true);
        group.fields = vec![sub];

        let def = make_collection(vec![group]);
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".into()),
            config: locale_config_en_de(),
        };
        let result = resolve_filter_column("meta__description", &def, Some(&ctx));
        assert_eq!(result, "meta__description__de");
    }

    // ── resolve_filters ───────────────────────────────────────────────────

    #[test]
    fn resolve_filters_non_localized_passthrough() {
        let def = make_collection(vec![make_field("status", FieldType::Text, false)]);
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".into()),
            config: locale_config_en_de(),
        };
        let filters = vec![
            FilterClause::Single(Filter { field: "status".into(), op: FilterOp::Equals("active".into()) }),
        ];
        let resolved = resolve_filters(&filters, &def, Some(&ctx));
        match &resolved[0] {
            FilterClause::Single(f) => assert_eq!(f.field, "status"),
            other => panic!("Expected Single, got {:?}", other),
        }
    }

    #[test]
    fn resolve_filters_applies_locale() {
        let def = make_collection(vec![make_field("title", FieldType::Text, true)]);
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".into()),
            config: locale_config_en_de(),
        };
        let filters = vec![
            FilterClause::Single(Filter { field: "title".into(), op: FilterOp::Equals("Hallo".into()) }),
        ];
        let resolved = resolve_filters(&filters, &def, Some(&ctx));
        match &resolved[0] {
            FilterClause::Single(f) => assert_eq!(f.field, "title__de"),
            other => panic!("Expected Single, got {:?}", other),
        }
    }
}
