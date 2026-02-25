//! CRUD query functions operating on `&rusqlite::Connection` (works with both plain
//! connections and transactions via `Deref`).

pub mod read;
pub mod write;
pub mod auth;
pub mod join;
pub mod populate;
pub mod filter;
pub mod global;
pub mod versions;
pub mod jobs;

use anyhow::{Result, bail};
use std::collections::HashSet;

use crate::config::LocaleConfig;
use crate::core::{CollectionDefinition, Document};
use crate::core::field::{FieldDefinition, FieldType};

pub use read::*;
pub use write::*;
pub use auth::*;
pub use join::*;
pub use populate::*;
pub use global::*;
pub use versions::*;

/// How to handle localized fields in a query.
#[derive(Debug, Clone)]
pub enum LocaleMode {
    /// Return only the default locale (or no locales if disabled). Flat field names.
    Default,
    /// Return a specific locale. Flat field names.
    Single(String),
    /// Return all locales. Nested objects: { en: "val", de: "val" }.
    All,
}

/// Locale context for query functions: combines config + mode.
#[derive(Debug, Clone)]
pub struct LocaleContext {
    pub mode: LocaleMode,
    pub config: LocaleConfig,
}

impl LocaleContext {
    /// Build a `LocaleContext` from an optional locale string and config.
    /// Returns `None` if localization is disabled (empty `locales` vec).
    /// `"all"` → `All`, a specific code → `Single`, `None` → `Default`.
    pub fn from_locale_string(locale: Option<&str>, config: &LocaleConfig) -> Option<Self> {
        if !config.is_enabled() {
            return None;
        }
        let mode = match locale {
            Some("all") => LocaleMode::All,
            Some(l) => LocaleMode::Single(l.to_string()),
            None => LocaleMode::Default,
        };
        Some(Self { mode, config: config.clone() })
    }
}

/// Result of an access control check.
#[derive(Debug, Clone)]
pub enum AccessResult {
    /// Access allowed, no restrictions.
    Allowed,
    /// Access denied.
    Denied,
    /// Access allowed with constraints (read only). Additional query filters to merge.
    Constrained(Vec<FilterClause>),
}

/// A filter comparison operator with its operand value(s).
#[derive(Debug, Clone)]
pub enum FilterOp {
    Equals(String),
    NotEquals(String),
    Like(String),
    Contains(String),
    GreaterThan(String),
    LessThan(String),
    GreaterThanOrEqual(String),
    LessThanOrEqual(String),
    In(Vec<String>),
    NotIn(Vec<String>),
    Exists,
    NotExists,
}

/// A single field + operator filter condition.
#[derive(Debug, Clone)]
pub struct Filter {
    pub field: String,
    pub op: FilterOp,
}

/// A filter clause: either a single condition or an OR group.
/// Each OR element is a group of AND-ed filters: `(a AND b) OR (c AND d)`.
#[derive(Debug, Clone)]
pub enum FilterClause {
    Single(Filter),
    Or(Vec<Vec<Filter>>),
}

/// Parameters for a find query: filters, ordering, pagination, and field selection.
#[derive(Debug, Default, Clone)]
pub struct FindQuery {
    pub filters: Vec<FilterClause>,
    pub order_by: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    /// Optional list of fields to return. `None` = all fields.
    /// Always includes `id`, `created_at`, `updated_at`.
    pub select: Option<Vec<String>>,
}

/// Check that a string is a safe SQL identifier (alphanumeric + underscore).
pub fn is_valid_identifier(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
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
pub fn validate_query_fields(def: &CollectionDefinition, query: &FindQuery, locale_ctx: Option<&LocaleContext>) -> Result<()> {
    let valid = get_valid_filter_columns(def, locale_ctx);

    for clause in &query.filters {
        match clause {
            FilterClause::Single(f) => validate_field_name(&f.field, &valid)?,
            FilterClause::Or(groups) => {
                for group in groups {
                    for f in group {
                        validate_field_name(&f.field, &valid)?;
                    }
                }
            }
        }
    }

    if let Some(ref order) = query.order_by {
        let col = order.strip_prefix('-').unwrap_or(order);
        validate_field_name(col, &valid)?;
    }

    Ok(())
}

/// Get column names for a collection (id + field columns + timestamps).
pub fn get_column_names(def: &CollectionDefinition) -> Vec<String> {
    let mut names = vec!["id".to_string()];
    for field in &def.fields {
        if field.field_type == FieldType::Group {
            for sub in &field.fields {
                names.push(format!("{}__{}", field.name, sub.name));
            }
        } else if field.has_parent_column() {
            names.push(field.name.clone());
        }
    }
    if def.has_drafts() {
        names.push("_status".to_string());
    }
    if def.timestamps {
        names.push("created_at".to_string());
        names.push("updated_at".to_string());
    }
    names
}

/// Get locale-aware SELECT expressions and result column names for a collection.
/// Returns (select_exprs, result_names) where:
/// - select_exprs: SQL expressions for the SELECT clause (may include aliases/COALESCE)
/// - result_names: column names in the result set (used by row_to_document)
pub fn get_locale_select_columns(
    fields: &[FieldDefinition],
    timestamps: bool,
    locale_ctx: &LocaleContext,
) -> (Vec<String>, Vec<String>) {
    let mut select_exprs = vec!["id".to_string()];
    let mut result_names = vec!["id".to_string()];

    for field in fields {
        if field.field_type == FieldType::Group {
            for sub in &field.fields {
                let base = format!("{}__{}", field.name, sub.name);
                let is_localized = (field.localized || sub.localized) && locale_ctx.config.is_enabled();
                if is_localized {
                    add_locale_columns(&mut select_exprs, &mut result_names, &base, locale_ctx);
                } else {
                    select_exprs.push(base.clone());
                    result_names.push(base);
                }
            }
        } else if field.has_parent_column() {
            if field.localized && locale_ctx.config.is_enabled() {
                add_locale_columns(&mut select_exprs, &mut result_names, &field.name, locale_ctx);
            } else {
                select_exprs.push(field.name.clone());
                result_names.push(field.name.clone());
            }
        }
    }

    if timestamps {
        select_exprs.push("created_at".to_string());
        result_names.push("created_at".to_string());
        select_exprs.push("updated_at".to_string());
        result_names.push("updated_at".to_string());
    }

    (select_exprs, result_names)
}

/// Add SELECT expressions for a localized field based on the locale mode.
fn add_locale_columns(
    select_exprs: &mut Vec<String>,
    result_names: &mut Vec<String>,
    field_name: &str,
    locale_ctx: &LocaleContext,
) {
    match &locale_ctx.mode {
        LocaleMode::Default => {
            let locale = &locale_ctx.config.default_locale;
            select_exprs.push(format!("{}__{} AS {}", field_name, locale, field_name));
            result_names.push(field_name.to_string());
        }
        LocaleMode::Single(locale) => {
            if locale_ctx.config.fallback && *locale != locale_ctx.config.default_locale {
                select_exprs.push(format!(
                    "COALESCE({}__{}, {}__{}) AS {}",
                    field_name, locale,
                    field_name, locale_ctx.config.default_locale,
                    field_name
                ));
            } else {
                select_exprs.push(format!("{}__{} AS {}", field_name, locale, field_name));
            }
            result_names.push(field_name.to_string());
        }
        LocaleMode::All => {
            for locale in &locale_ctx.config.locales {
                let col = format!("{}__{}", field_name, locale);
                select_exprs.push(col.clone());
                result_names.push(col);
            }
        }
    }
}

/// Group locale-suffixed fields into nested objects for `LocaleMode::All`.
/// Converts `title__en: "Hello", title__de: "Hallo"` into `title: { en: "Hello", de: "Hallo" }`.
pub(crate) fn group_locale_fields(doc: &mut Document, fields: &[FieldDefinition], locale_config: &LocaleConfig) {
    for field in fields {
        if field.field_type == FieldType::Group {
            for sub in &field.fields {
                if (field.localized || sub.localized) && locale_config.is_enabled() {
                    let base = format!("{}__{}", field.name, sub.name);
                    let mut locale_map = serde_json::Map::new();
                    for locale in &locale_config.locales {
                        let col = format!("{}__{}", base, locale);
                        if let Some(val) = doc.fields.remove(&col) {
                            locale_map.insert(locale.clone(), val);
                        }
                    }
                    if !locale_map.is_empty() {
                        doc.fields.insert(base, serde_json::Value::Object(locale_map));
                    }
                }
            }
        } else if field.has_parent_column() && field.localized && locale_config.is_enabled() {
            let mut locale_map = serde_json::Map::new();
            for locale in &locale_config.locales {
                let col = format!("{}__{}", field.name, locale);
                if let Some(val) = doc.fields.remove(&col) {
                    locale_map.insert(locale.clone(), val);
                }
            }
            if !locale_map.is_empty() {
                doc.fields.insert(field.name.clone(), serde_json::Value::Object(locale_map));
            }
        }
    }
}

/// Get the set of valid filter column names, accounting for locale.
pub(crate) fn get_valid_filter_columns(def: &CollectionDefinition, locale_ctx: Option<&LocaleContext>) -> HashSet<String> {
    let mut valid = HashSet::new();
    valid.insert("id".to_string());
    for field in &def.fields {
        if field.field_type == FieldType::Group {
            for sub in &field.fields {
                valid.insert(format!("{}__{}", field.name, sub.name));
            }
        } else if field.has_parent_column() {
            valid.insert(field.name.clone());
        }
    }
    if def.has_drafts() {
        valid.insert("_status".to_string());
    }
    if def.timestamps {
        valid.insert("created_at".to_string());
        valid.insert("updated_at".to_string());
    }
    let _ = locale_ctx; // filter validation uses undecorated field names
    valid
}

/// Map a flat field name to the actual locale-suffixed column name for writes.
pub(crate) fn locale_write_column(field_name: &str, field: &FieldDefinition, locale_ctx: &Option<&LocaleContext>) -> String {
    if let Some(ctx) = locale_ctx {
        if field.localized && ctx.config.is_enabled() {
            let locale = match &ctx.mode {
                LocaleMode::Single(l) => l.as_str(),
                _ => ctx.config.default_locale.as_str(),
            };
            return format!("{}__{}", field_name, locale);
        }
    }
    field_name.to_string()
}

/// Normalize a date value for storage.
///
/// - Full ISO 8601 with timezone (`2026-01-15T09:00:00Z`, `2026-01-15T09:00:00+05:00`)
///   → re-format as `YYYY-MM-DDTHH:MM:SS.000Z` (UTC)
/// - Date only (`2026-01-15`) → `2026-01-15T12:00:00.000Z` (UTC noon, prevents timezone drift)
/// - datetime-local format (`2026-01-15T09:00`) → treat as UTC → `2026-01-15T09:00:00.000Z`
/// - Time only (`14:30`) → passthrough
/// - Month only (`2026-01`) → passthrough
/// - Anything else → passthrough (validation catches garbage)
pub fn normalize_date_value(value: &str) -> String {
    use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, Utc};

    // Time only: HH:MM or HH:MM:SS
    if value.len() <= 8 && value.contains(':') && !value.contains('T') {
        return value.to_string();
    }

    // Month only: YYYY-MM (exactly 7 chars, dash at position 4)
    if value.len() == 7 && value.as_bytes().get(4) == Some(&b'-') && !value.contains('T') {
        return value.to_string();
    }

    // Try full RFC 3339 / ISO 8601 with timezone (e.g., 2026-01-15T09:00:00Z, 2026-01-15T09:00:00+05:00)
    if let Ok(dt) = DateTime::<FixedOffset>::parse_from_rfc3339(value) {
        let utc = dt.with_timezone(&Utc);
        return utc.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    }

    // Try date only: YYYY-MM-DD (10 chars)
    if value.len() == 10 {
        if let Ok(d) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
            let noon = d.and_hms_opt(12, 0, 0).unwrap();
            return noon.and_utc().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
        }
    }

    // Try datetime-local format: YYYY-MM-DDTHH:MM (16 chars, no timezone)
    if value.len() == 16 && value.contains('T') {
        if let Ok(ndt) = NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M") {
            return ndt.and_utc().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
        }
    }

    // Try datetime without timezone: YYYY-MM-DDTHH:MM:SS (19 chars)
    if value.len() == 19 && value.contains('T') {
        if let Ok(ndt) = NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S") {
            return ndt.and_utc().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
        }
    }

    // Anything else: passthrough
    value.to_string()
}

/// Coerce a form string value to the appropriate SQLite type.
pub(crate) fn coerce_value(field_type: &FieldType, value: &str) -> Box<dyn rusqlite::types::ToSql> {
    match field_type {
        FieldType::Checkbox => {
            let b = matches!(value, "on" | "true" | "1" | "yes");
            Box::new(b as i32)
        }
        FieldType::Number => {
            if value.is_empty() {
                Box::new(rusqlite::types::Null)
            } else if let Ok(f) = value.parse::<f64>() {
                Box::new(f)
            } else {
                Box::new(rusqlite::types::Null)
            }
        }
        FieldType::Date => {
            if value.is_empty() {
                Box::new(rusqlite::types::Null)
            } else {
                Box::new(normalize_date_value(value))
            }
        }
        _ => {
            if value.is_empty() {
                Box::new(rusqlite::types::Null)
            } else {
                Box::new(value.to_string())
            }
        }
    }
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

    // ── LocaleContext tests ──────────────────────────────────────────────────

    #[test]
    fn locale_context_disabled() {
        let config = crate::config::LocaleConfig::default();
        let ctx = LocaleContext::from_locale_string(None, &config);
        assert!(ctx.is_none(), "Should be None when localization is disabled");
    }

    #[test]
    fn locale_context_all() {
        let config = crate::config::LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        };
        let ctx = LocaleContext::from_locale_string(Some("all"), &config);
        assert!(ctx.is_some());
        assert!(matches!(ctx.unwrap().mode, LocaleMode::All));
    }

    #[test]
    fn locale_context_specific() {
        let config = crate::config::LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        };
        let ctx = LocaleContext::from_locale_string(Some("de"), &config);
        assert!(ctx.is_some());
        match ctx.unwrap().mode {
            LocaleMode::Single(locale) => assert_eq!(locale, "de"),
            other => panic!("Expected Single, got {:?}", other),
        }
    }

    #[test]
    fn locale_context_default() {
        let config = crate::config::LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        };
        let ctx = LocaleContext::from_locale_string(None, &config);
        assert!(ctx.is_some());
        assert!(matches!(ctx.unwrap().mode, LocaleMode::Default));
    }

    // ── Helper functions for new tests ───────────────────────────────────────

    use crate::core::collection::{
        CollectionDefinition, CollectionLabels, CollectionAdmin, CollectionHooks, CollectionAccess,
    };
    use crate::core::field::{FieldDefinition, FieldType, FieldAdmin, FieldHooks, FieldAccess};

    fn make_field(name: &str, field_type: FieldType) -> FieldDefinition {
        FieldDefinition {
            name: name.to_string(),
            field_type,
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
            localized: false,
            picker_appearance: None,
        }
    }

    fn make_localized_field(name: &str, field_type: FieldType) -> FieldDefinition {
        let mut f = make_field(name, field_type);
        f.localized = true;
        f
    }

    fn make_group_field(name: &str, sub_fields: Vec<FieldDefinition>) -> FieldDefinition {
        let mut f = make_field(name, FieldType::Group);
        f.fields = sub_fields;
        f
    }

    fn make_collection_def(slug: &str, fields: Vec<FieldDefinition>, timestamps: bool) -> CollectionDefinition {
        CollectionDefinition {
            slug: slug.to_string(),
            labels: CollectionLabels::default(),
            timestamps,
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

    fn make_locale_config() -> crate::config::LocaleConfig {
        crate::config::LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        }
    }

    // ── coerce_value tests ───────────────────────────────────────────────────

    #[test]
    fn coerce_value_checkbox_truthy() {
        use rusqlite::types::ToSql;
        for input in &["on", "true", "1", "yes"] {
            let val = coerce_value(&FieldType::Checkbox, input);
            let output = val.to_sql().unwrap();
            match output {
                rusqlite::types::ToSqlOutput::Owned(rusqlite::types::Value::Integer(i)) => {
                    assert_eq!(i, 1, "Expected 1 for checkbox input '{}'", input);
                }
                other => panic!("Expected Integer(1) for '{}', got {:?}", input, other),
            }
        }
    }

    #[test]
    fn coerce_value_checkbox_falsy() {
        use rusqlite::types::ToSql;
        for input in &["off", "false", "0", "no"] {
            let val = coerce_value(&FieldType::Checkbox, input);
            let output = val.to_sql().unwrap();
            match output {
                rusqlite::types::ToSqlOutput::Owned(rusqlite::types::Value::Integer(i)) => {
                    assert_eq!(i, 0, "Expected 0 for checkbox input '{}'", input);
                }
                other => panic!("Expected Integer(0) for '{}', got {:?}", input, other),
            }
        }
    }

    #[test]
    fn coerce_value_number_valid() {
        use rusqlite::types::ToSql;
        let val = coerce_value(&FieldType::Number, "42.5");
        let output = val.to_sql().unwrap();
        match output {
            rusqlite::types::ToSqlOutput::Owned(rusqlite::types::Value::Real(f)) => {
                assert!((f - 42.5).abs() < f64::EPSILON, "Expected 42.5, got {}", f);
            }
            other => panic!("Expected Real(42.5), got {:?}", other),
        }
    }

    #[test]
    fn coerce_value_number_empty_is_null() {
        use rusqlite::types::ToSql;
        let val = coerce_value(&FieldType::Number, "");
        let output = val.to_sql().unwrap();
        match output {
            rusqlite::types::ToSqlOutput::Owned(rusqlite::types::Value::Null) => {}
            other => panic!("Expected Null for empty number, got {:?}", other),
        }
    }

    #[test]
    fn coerce_value_number_invalid_is_null() {
        use rusqlite::types::ToSql;
        let val = coerce_value(&FieldType::Number, "abc");
        let output = val.to_sql().unwrap();
        match output {
            rusqlite::types::ToSqlOutput::Owned(rusqlite::types::Value::Null) => {}
            other => panic!("Expected Null for invalid number 'abc', got {:?}", other),
        }
    }

    #[test]
    fn coerce_value_text_nonempty() {
        use rusqlite::types::ToSql;
        let val = coerce_value(&FieldType::Text, "hello");
        let output = val.to_sql().unwrap();
        match &output {
            rusqlite::types::ToSqlOutput::Owned(rusqlite::types::Value::Text(s)) => {
                assert_eq!(s, "hello");
            }
            rusqlite::types::ToSqlOutput::Borrowed(rusqlite::types::ValueRef::Text(b)) => {
                assert_eq!(std::str::from_utf8(b).unwrap(), "hello");
            }
            other => panic!("Expected Text('hello'), got {:?}", other),
        }
    }

    #[test]
    fn coerce_value_text_empty_is_null() {
        use rusqlite::types::ToSql;
        let val = coerce_value(&FieldType::Text, "");
        let output = val.to_sql().unwrap();
        match output {
            rusqlite::types::ToSqlOutput::Owned(rusqlite::types::Value::Null) => {}
            other => panic!("Expected Null for empty text, got {:?}", other),
        }
    }

    // ── get_column_names tests ───────────────────────────────────────────────

    #[test]
    fn get_column_names_simple_fields() {
        let def = make_collection_def("posts", vec![
            make_field("title", FieldType::Text),
            make_field("count", FieldType::Number),
        ], true);
        let names = get_column_names(&def);
        assert_eq!(names, vec!["id", "title", "count", "created_at", "updated_at"]);
    }

    #[test]
    fn get_column_names_with_group() {
        let def = make_collection_def("posts", vec![
            make_field("title", FieldType::Text),
            make_group_field("seo", vec![
                make_field("title", FieldType::Text),
                make_field("description", FieldType::Textarea),
            ]),
        ], true);
        let names = get_column_names(&def);
        assert_eq!(names, vec!["id", "title", "seo__title", "seo__description", "created_at", "updated_at"]);
    }

    #[test]
    fn get_column_names_no_timestamps() {
        let def = make_collection_def("posts", vec![
            make_field("title", FieldType::Text),
        ], false);
        let names = get_column_names(&def);
        assert_eq!(names, vec!["id", "title"]);
    }

    // ── locale_write_column tests ────────────────────────────────────────────

    #[test]
    fn locale_write_column_non_localized_passthrough() {
        let field = make_field("title", FieldType::Text);
        let locale_cfg = make_locale_config();
        let ctx = LocaleContext { mode: LocaleMode::Single("de".to_string()), config: locale_cfg };
        let ctx_ref: Option<&LocaleContext> = Some(&ctx);
        let col = locale_write_column("title", &field, &ctx_ref);
        assert_eq!(col, "title", "Non-localized field should pass through unchanged");
    }

    #[test]
    fn locale_write_column_localized_single() {
        let field = make_localized_field("title", FieldType::Text);
        let locale_cfg = make_locale_config();
        let ctx = LocaleContext { mode: LocaleMode::Single("de".to_string()), config: locale_cfg };
        let ctx_ref: Option<&LocaleContext> = Some(&ctx);
        let col = locale_write_column("title", &field, &ctx_ref);
        assert_eq!(col, "title__de");
    }

    #[test]
    fn locale_write_column_localized_default_mode() {
        let field = make_localized_field("title", FieldType::Text);
        let locale_cfg = make_locale_config();
        let ctx = LocaleContext { mode: LocaleMode::Default, config: locale_cfg };
        let ctx_ref: Option<&LocaleContext> = Some(&ctx);
        let col = locale_write_column("title", &field, &ctx_ref);
        assert_eq!(col, "title__en", "Default mode should use default locale");
    }

    // ── get_locale_select_columns tests ──────────────────────────────────────

    #[test]
    fn get_locale_select_columns_default_mode() {
        let fields = vec![make_localized_field("title", FieldType::Text)];
        let locale_cfg = make_locale_config();
        let ctx = LocaleContext { mode: LocaleMode::Default, config: locale_cfg };
        let (exprs, names) = get_locale_select_columns(&fields, false, &ctx);
        assert_eq!(exprs, vec!["id", "title__en AS title"]);
        assert_eq!(names, vec!["id", "title"]);
    }

    #[test]
    fn get_locale_select_columns_single_with_fallback() {
        let fields = vec![make_localized_field("title", FieldType::Text)];
        let locale_cfg = make_locale_config(); // fallback = true, default = "en"
        let ctx = LocaleContext { mode: LocaleMode::Single("de".to_string()), config: locale_cfg };
        let (exprs, names) = get_locale_select_columns(&fields, false, &ctx);
        assert_eq!(exprs, vec!["id", "COALESCE(title__de, title__en) AS title"]);
        assert_eq!(names, vec!["id", "title"]);
    }

    #[test]
    fn get_locale_select_columns_all_mode() {
        let fields = vec![make_localized_field("title", FieldType::Text)];
        let locale_cfg = make_locale_config();
        let ctx = LocaleContext { mode: LocaleMode::All, config: locale_cfg };
        let (exprs, names) = get_locale_select_columns(&fields, false, &ctx);
        assert_eq!(exprs, vec!["id", "title__en", "title__de"]);
        assert_eq!(names, vec!["id", "title__en", "title__de"]);
    }

    // ── group_locale_fields tests ────────────────────────────────────────────

    #[test]
    fn group_locale_fields_basic() {
        let fields = vec![make_localized_field("title", FieldType::Text)];
        let locale_cfg = make_locale_config();
        let mut doc = crate::core::Document::new("id1".to_string());
        doc.fields.insert("title__en".to_string(), serde_json::json!("Hello"));
        doc.fields.insert("title__de".to_string(), serde_json::json!("Hallo"));

        group_locale_fields(&mut doc, &fields, &locale_cfg);

        let title = doc.fields.get("title").expect("title should exist");
        assert_eq!(title.get("en").and_then(|v| v.as_str()), Some("Hello"));
        assert_eq!(title.get("de").and_then(|v| v.as_str()), Some("Hallo"));
        // Original suffixed keys should be removed
        assert!(!doc.fields.contains_key("title__en"));
        assert!(!doc.fields.contains_key("title__de"));
    }

    #[test]
    fn group_locale_fields_with_group_prefix() {
        let fields = vec![
            make_group_field("seo", vec![
                make_localized_field("title", FieldType::Text),
            ]),
        ];
        let locale_cfg = make_locale_config();
        let mut doc = crate::core::Document::new("id1".to_string());
        doc.fields.insert("seo__title__en".to_string(), serde_json::json!("SEO EN"));
        doc.fields.insert("seo__title__de".to_string(), serde_json::json!("SEO DE"));

        group_locale_fields(&mut doc, &fields, &locale_cfg);

        let seo_title = doc.fields.get("seo__title").expect("seo__title should exist");
        assert_eq!(seo_title.get("en").and_then(|v| v.as_str()), Some("SEO EN"));
        assert_eq!(seo_title.get("de").and_then(|v| v.as_str()), Some("SEO DE"));
        assert!(!doc.fields.contains_key("seo__title__en"));
        assert!(!doc.fields.contains_key("seo__title__de"));
    }

    // ── get_valid_filter_columns tests ───────────────────────────────────────

    #[test]
    fn get_valid_filter_columns_includes_expected() {
        let def = make_collection_def("posts", vec![
            make_field("title", FieldType::Text),
            make_field("status", FieldType::Select),
            make_group_field("seo", vec![
                make_field("title", FieldType::Text),
            ]),
        ], true);
        let valid = get_valid_filter_columns(&def, None);
        assert!(valid.contains("id"));
        assert!(valid.contains("title"));
        assert!(valid.contains("status"));
        assert!(valid.contains("seo__title"));
        assert!(valid.contains("created_at"));
        assert!(valid.contains("updated_at"));
    }

    #[test]
    fn get_valid_filter_columns_excludes_array_and_blocks() {
        let def = make_collection_def("posts", vec![
            make_field("title", FieldType::Text),
            make_field("tags", FieldType::Array),
            make_field("content", FieldType::Blocks),
        ], true);
        let valid = get_valid_filter_columns(&def, None);
        assert!(valid.contains("title"), "Text fields should be included");
        assert!(!valid.contains("tags"), "Array fields should be excluded");
        assert!(!valid.contains("content"), "Blocks fields should be excluded");
    }

    // ── normalize_date_value tests ──────────────────────────────────────────

    #[test]
    fn normalize_date_only_to_utc_noon() {
        assert_eq!(normalize_date_value("2026-01-15"), "2026-01-15T12:00:00.000Z");
    }

    #[test]
    fn normalize_full_iso_utc() {
        assert_eq!(normalize_date_value("2026-01-15T09:00:00Z"), "2026-01-15T09:00:00.000Z");
    }

    #[test]
    fn normalize_iso_with_millis() {
        assert_eq!(normalize_date_value("2026-01-15T09:00:00.000Z"), "2026-01-15T09:00:00.000Z");
    }

    #[test]
    fn normalize_iso_with_offset() {
        assert_eq!(normalize_date_value("2026-01-15T09:00:00+05:00"), "2026-01-15T04:00:00.000Z");
    }

    #[test]
    fn normalize_datetime_local() {
        assert_eq!(normalize_date_value("2026-01-15T09:00"), "2026-01-15T09:00:00.000Z");
    }

    #[test]
    fn normalize_datetime_no_tz() {
        assert_eq!(normalize_date_value("2026-01-15T09:00:00"), "2026-01-15T09:00:00.000Z");
    }

    #[test]
    fn normalize_time_only_passthrough() {
        assert_eq!(normalize_date_value("14:30"), "14:30");
    }

    #[test]
    fn normalize_month_only_passthrough() {
        assert_eq!(normalize_date_value("2026-01"), "2026-01");
    }

    #[test]
    fn normalize_garbage_passthrough() {
        assert_eq!(normalize_date_value("garbage"), "garbage");
    }

    // ── coerce_value Date tests ─────────────────────────────────────────────

    #[test]
    fn coerce_value_date_empty_is_null() {
        use rusqlite::types::ToSql;
        let val = coerce_value(&FieldType::Date, "");
        let output = val.to_sql().unwrap();
        match output {
            rusqlite::types::ToSqlOutput::Owned(rusqlite::types::Value::Null) => {}
            other => panic!("Expected Null for empty date, got {:?}", other),
        }
    }

    #[test]
    fn coerce_value_date_normalizes() {
        use rusqlite::types::ToSql;
        let val = coerce_value(&FieldType::Date, "2026-03-15");
        let output = val.to_sql().unwrap();
        let text = match &output {
            rusqlite::types::ToSqlOutput::Owned(rusqlite::types::Value::Text(s)) => s.clone(),
            rusqlite::types::ToSqlOutput::Borrowed(rusqlite::types::ValueRef::Text(b)) => {
                std::str::from_utf8(b).unwrap().to_string()
            }
            other => panic!("Expected normalized date string, got {:?}", other),
        };
        assert_eq!(text, "2026-03-15T12:00:00.000Z");
    }
}
