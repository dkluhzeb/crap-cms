//! Locale types and functions for locale-aware queries.

use crate::config::LocaleConfig;
use crate::core::Document;
use crate::core::field::{FieldDefinition, FieldType};

use super::validation::sanitize_locale;

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
        Some(Self {
            mode,
            config: config.clone(),
        })
    }
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

    collect_locale_columns(
        fields,
        &mut select_exprs,
        &mut result_names,
        locale_ctx,
        "",
        false,
    );

    if timestamps {
        select_exprs.push("created_at".to_string());
        result_names.push("created_at".to_string());
        select_exprs.push("updated_at".to_string());
        result_names.push("updated_at".to_string());
    }

    (select_exprs, result_names)
}

/// Recursively collect locale-aware SELECT columns from a field tree.
fn collect_locale_columns(
    fields: &[FieldDefinition],
    select_exprs: &mut Vec<String>,
    result_names: &mut Vec<String>,
    locale_ctx: &LocaleContext,
    prefix: &str,
    inherited_localized: bool,
) {
    for field in fields {
        match field.field_type {
            FieldType::Group => {
                let new_prefix = if prefix.is_empty() {
                    field.name.clone()
                } else {
                    format!("{}__{}", prefix, field.name)
                };
                collect_locale_columns(
                    &field.fields,
                    select_exprs,
                    result_names,
                    locale_ctx,
                    &new_prefix,
                    inherited_localized || field.localized,
                );
            }
            FieldType::Row | FieldType::Collapsible => {
                collect_locale_columns(
                    &field.fields,
                    select_exprs,
                    result_names,
                    locale_ctx,
                    prefix,
                    inherited_localized,
                );
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    collect_locale_columns(
                        &tab.fields,
                        select_exprs,
                        result_names,
                        locale_ctx,
                        prefix,
                        inherited_localized,
                    );
                }
            }
            _ => {
                if !field.has_parent_column() {
                    continue;
                }
                let base = if prefix.is_empty() {
                    field.name.clone()
                } else {
                    format!("{}__{}", prefix, field.name)
                };
                let is_localized =
                    (inherited_localized || field.localized) && locale_ctx.config.is_enabled();
                if is_localized {
                    add_locale_columns(select_exprs, result_names, &base, locale_ctx);
                } else {
                    select_exprs.push(base.clone());
                    result_names.push(base);
                }
            }
        }
    }
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
            let locale = sanitize_locale(&locale_ctx.config.default_locale);
            select_exprs.push(format!("{}__{} AS {}", field_name, locale, field_name));
            result_names.push(field_name.to_string());
        }
        LocaleMode::Single(req_locale) => {
            let locale = if locale_ctx.config.locales.contains(req_locale) {
                req_locale
            } else {
                &locale_ctx.config.default_locale
            };
            let locale = sanitize_locale(locale);

            if locale_ctx.config.fallback
                && locale != sanitize_locale(&locale_ctx.config.default_locale)
            {
                select_exprs.push(format!(
                    "COALESCE({}__{}, {}__{}) AS {}",
                    field_name,
                    locale,
                    field_name,
                    sanitize_locale(&locale_ctx.config.default_locale),
                    field_name
                ));
            } else {
                select_exprs.push(format!("{}__{} AS {}", field_name, locale, field_name));
            }
            result_names.push(field_name.to_string());
        }
        LocaleMode::All => {
            for locale in &locale_ctx.config.locales {
                let locale = sanitize_locale(locale);
                let col = format!("{}__{}", field_name, locale);
                select_exprs.push(col.clone());
                result_names.push(col);
            }
        }
    }
}

/// Group locale-suffixed fields into nested objects for `LocaleMode::All`.
/// Converts `title__en: "Hello", title__de: "Hallo"` into `title: { en: "Hello", de: "Hallo" }`.
pub(crate) fn group_locale_fields(
    doc: &mut Document,
    fields: &[FieldDefinition],
    locale_config: &LocaleConfig,
) {
    group_locale_fields_inner(doc, fields, locale_config, "", false);
}

fn group_locale_fields_inner(
    doc: &mut Document,
    fields: &[FieldDefinition],
    locale_config: &LocaleConfig,
    prefix: &str,
    inherited_localized: bool,
) {
    for field in fields {
        match field.field_type {
            FieldType::Group => {
                let new_prefix = if prefix.is_empty() {
                    field.name.clone()
                } else {
                    format!("{}__{}", prefix, field.name)
                };
                group_locale_fields_inner(
                    doc,
                    &field.fields,
                    locale_config,
                    &new_prefix,
                    inherited_localized || field.localized,
                );
            }
            FieldType::Row | FieldType::Collapsible => {
                group_locale_fields_inner(
                    doc,
                    &field.fields,
                    locale_config,
                    prefix,
                    inherited_localized,
                );
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    group_locale_fields_inner(
                        doc,
                        &tab.fields,
                        locale_config,
                        prefix,
                        inherited_localized,
                    );
                }
            }
            _ => {
                if !field.has_parent_column() {
                    continue;
                }
                let is_localized =
                    (inherited_localized || field.localized) && locale_config.is_enabled();
                if !is_localized {
                    continue;
                }
                let base = if prefix.is_empty() {
                    field.name.clone()
                } else {
                    format!("{}__{}", prefix, field.name)
                };
                let mut locale_map = serde_json::Map::new();
                for locale in &locale_config.locales {
                    let col = format!("{}__{}", base, sanitize_locale(locale));
                    if let Some(val) = doc.fields.remove(&col) {
                        locale_map.insert(locale.clone(), val);
                    }
                }
                if !locale_map.is_empty() {
                    doc.fields
                        .insert(base, serde_json::Value::Object(locale_map));
                }
            }
        }
    }
}

/// Map a flat field name to the actual locale-suffixed column name for writes.
pub(crate) fn locale_write_column(
    field_name: &str,
    field: &FieldDefinition,
    locale_ctx: &Option<&LocaleContext>,
) -> String {
    if let Some(ctx) = locale_ctx
        && field.localized
        && ctx.config.is_enabled()
    {
        let req_locale = match &ctx.mode {
            LocaleMode::Single(l) => l.as_str(),
            _ => ctx.config.default_locale.as_str(),
        };
        let locale = if ctx.config.locales.iter().any(|l| l == req_locale) {
            req_locale
        } else {
            ctx.config.default_locale.as_str()
        };
        return format!("{}__{}", field_name, sanitize_locale(locale));
    }
    field_name.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::field::FieldType;
    use crate::db::query::test_helpers::*;

    #[test]
    fn locale_context_disabled() {
        let config = crate::config::LocaleConfig::default();
        let ctx = LocaleContext::from_locale_string(None, &config);
        assert!(
            ctx.is_none(),
            "Should be None when localization is disabled"
        );
    }

    #[test]
    fn locale_context_all() {
        let config = make_locale_config();
        let ctx = LocaleContext::from_locale_string(Some("all"), &config);
        assert!(ctx.is_some());
        assert!(matches!(ctx.unwrap().mode, LocaleMode::All));
    }

    #[test]
    fn locale_context_specific() {
        let config = make_locale_config();
        let ctx = LocaleContext::from_locale_string(Some("de"), &config);
        assert!(ctx.is_some());
        match ctx.unwrap().mode {
            LocaleMode::Single(locale) => assert_eq!(locale, "de"),
            other => panic!("Expected Single, got {:?}", other),
        }
    }

    #[test]
    fn locale_context_default() {
        let config = make_locale_config();
        let ctx = LocaleContext::from_locale_string(None, &config);
        assert!(ctx.is_some());
        assert!(matches!(ctx.unwrap().mode, LocaleMode::Default));
    }

    #[test]
    fn locale_write_column_non_localized_passthrough() {
        let field = make_field("title", FieldType::Text);
        let locale_cfg = make_locale_config();
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: locale_cfg,
        };
        let ctx_ref: Option<&LocaleContext> = Some(&ctx);
        let col = locale_write_column("title", &field, &ctx_ref);
        assert_eq!(
            col, "title",
            "Non-localized field should pass through unchanged"
        );
    }

    #[test]
    fn locale_write_column_localized_single() {
        let field = make_localized_field("title", FieldType::Text);
        let locale_cfg = make_locale_config();
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: locale_cfg,
        };
        let ctx_ref: Option<&LocaleContext> = Some(&ctx);
        let col = locale_write_column("title", &field, &ctx_ref);
        assert_eq!(col, "title__de");
    }

    #[test]
    fn locale_write_column_localized_default_mode() {
        let field = make_localized_field("title", FieldType::Text);
        let locale_cfg = make_locale_config();
        let ctx = LocaleContext {
            mode: LocaleMode::Default,
            config: locale_cfg,
        };
        let ctx_ref: Option<&LocaleContext> = Some(&ctx);
        let col = locale_write_column("title", &field, &ctx_ref);
        assert_eq!(col, "title__en", "Default mode should use default locale");
    }

    #[test]
    fn get_locale_select_columns_default_mode() {
        let fields = vec![make_localized_field("title", FieldType::Text)];
        let locale_cfg = make_locale_config();
        let ctx = LocaleContext {
            mode: LocaleMode::Default,
            config: locale_cfg,
        };
        let (exprs, names) = get_locale_select_columns(&fields, false, &ctx);
        assert_eq!(exprs, vec!["id", "title__en AS title"]);
        assert_eq!(names, vec!["id", "title"]);
    }

    #[test]
    fn get_locale_select_columns_single_with_fallback() {
        let fields = vec![make_localized_field("title", FieldType::Text)];
        let locale_cfg = make_locale_config();
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: locale_cfg,
        };
        let (exprs, names) = get_locale_select_columns(&fields, false, &ctx);
        assert_eq!(exprs, vec!["id", "COALESCE(title__de, title__en) AS title"]);
        assert_eq!(names, vec!["id", "title"]);
    }

    #[test]
    fn get_locale_select_columns_all_mode() {
        let fields = vec![make_localized_field("title", FieldType::Text)];
        let locale_cfg = make_locale_config();
        let ctx = LocaleContext {
            mode: LocaleMode::All,
            config: locale_cfg,
        };
        let (exprs, names) = get_locale_select_columns(&fields, false, &ctx);
        assert_eq!(exprs, vec!["id", "title__en", "title__de"]);
        assert_eq!(names, vec!["id", "title__en", "title__de"]);
    }

    #[test]
    fn group_locale_fields_basic() {
        let fields = vec![make_localized_field("title", FieldType::Text)];
        let locale_cfg = make_locale_config();
        let mut doc = crate::core::Document::new("id1".to_string());
        doc.fields
            .insert("title__en".to_string(), serde_json::json!("Hello"));
        doc.fields
            .insert("title__de".to_string(), serde_json::json!("Hallo"));

        group_locale_fields(&mut doc, &fields, &locale_cfg);

        let title = doc.fields.get("title").expect("title should exist");
        assert_eq!(title.get("en").and_then(|v| v.as_str()), Some("Hello"));
        assert_eq!(title.get("de").and_then(|v| v.as_str()), Some("Hallo"));
        assert!(!doc.fields.contains_key("title__en"));
        assert!(!doc.fields.contains_key("title__de"));
    }

    #[test]
    fn group_locale_fields_with_group_prefix() {
        let fields = vec![make_group_field(
            "seo",
            vec![make_localized_field("title", FieldType::Text)],
        )];
        let locale_cfg = make_locale_config();
        let mut doc = crate::core::Document::new("id1".to_string());
        doc.fields
            .insert("seo__title__en".to_string(), serde_json::json!("SEO EN"));
        doc.fields
            .insert("seo__title__de".to_string(), serde_json::json!("SEO DE"));

        group_locale_fields(&mut doc, &fields, &locale_cfg);

        let seo_title = doc
            .fields
            .get("seo__title")
            .expect("seo__title should exist");
        assert_eq!(seo_title.get("en").and_then(|v| v.as_str()), Some("SEO EN"));
        assert_eq!(seo_title.get("de").and_then(|v| v.as_str()), Some("SEO DE"));
        assert!(!doc.fields.contains_key("seo__title__en"));
        assert!(!doc.fields.contains_key("seo__title__de"));
    }

    #[test]
    fn get_locale_select_columns_tabs_with_group() {
        let fields = vec![make_tabs_field(
            "layout",
            vec![crate::core::field::FieldTab::new(
                "Social",
                vec![make_group_field(
                    "social",
                    vec![make_field("github", FieldType::Text)],
                )],
            )],
        )];
        let locale_cfg = make_locale_config();
        let ctx = LocaleContext {
            mode: LocaleMode::Default,
            config: locale_cfg,
        };
        let (exprs, names) = get_locale_select_columns(&fields, false, &ctx);
        assert!(
            exprs.contains(&"social__github".to_string()),
            "Group inside Tabs should appear in SELECT"
        );
        assert!(names.contains(&"social__github".to_string()));
    }

    #[test]
    fn get_locale_select_columns_tabs_with_localized_field() {
        let fields = vec![make_tabs_field(
            "layout",
            vec![crate::core::field::FieldTab::new(
                "Content",
                vec![make_localized_field("title", FieldType::Text)],
            )],
        )];
        let locale_cfg = make_locale_config();
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: locale_cfg,
        };
        let (exprs, names) = get_locale_select_columns(&fields, false, &ctx);
        assert!(
            exprs.iter().any(|e| e.contains("title__de")),
            "Localized field in Tabs should have locale column"
        );
        assert!(names.contains(&"title".to_string()));
    }

    #[test]
    fn get_locale_select_columns_group_containing_tabs_localized() {
        let fields = vec![{
            let mut g = make_group_field(
                "meta",
                vec![make_tabs_field(
                    "t",
                    vec![crate::core::field::FieldTab::new(
                        "Content",
                        vec![make_field("title", FieldType::Text)],
                    )],
                )],
            );
            g.localized = true;
            g
        }];
        let locale_cfg = make_locale_config();
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: locale_cfg,
        };
        let (exprs, names) = get_locale_select_columns(&fields, false, &ctx);
        assert!(
            exprs.iter().any(|e| e.contains("meta__title__de")),
            "Localized Group→Tabs: meta__title__de"
        );
        assert!(names.contains(&"meta__title".to_string()));
    }
}
