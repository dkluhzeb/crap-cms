//! Locale-aware SELECT column lists for a collection's rows.

use anyhow::Result;

use crate::{
    core::FieldDefinition,
    db::{LocaleContext, query::locale::leaf_select::collect_locale_columns},
};

/// Get locale-aware SELECT expressions and result column names for a collection.
/// Returns (`select_exprs`, `result_names`) where:
/// - `select_exprs`: SQL expressions for the SELECT clause (may include aliases/COALESCE)
/// - `result_names`: column names in the result set (used by `row_to_document`)
///
/// # Errors
///
/// Returns an error if any field name conflicts with locale-suffixed naming.
pub fn get_locale_select_columns(
    fields: &[FieldDefinition],
    timestamps: bool,
    locale_ctx: &LocaleContext,
) -> Result<(Vec<String>, Vec<String>)> {
    get_locale_select_columns_full(fields, timestamps, false, false, locale_ctx)
}

/// Full version with all options including soft-delete and draft status columns.
///
/// # Errors
///
/// Returns an error if any field name conflicts with locale-suffixed naming.
pub fn get_locale_select_columns_full(
    fields: &[FieldDefinition],
    timestamps: bool,
    soft_delete: bool,
    has_drafts: bool,
    locale_ctx: &LocaleContext,
) -> Result<(Vec<String>, Vec<String>)> {
    let mut select_exprs = vec!["id".to_string()];
    let mut result_names = vec!["id".to_string()];

    collect_locale_columns(fields, &mut select_exprs, &mut result_names, locale_ctx)?;

    if has_drafts {
        select_exprs.push("_status".to_string());
        result_names.push("_status".to_string());
    }

    if soft_delete {
        select_exprs.push("_deleted_at".to_string());
        result_names.push("_deleted_at".to_string());
    }

    if timestamps {
        select_exprs.push("created_at".to_string());
        result_names.push("created_at".to_string());
        select_exprs.push("updated_at".to_string());
        result_names.push("updated_at".to_string());
    }

    Ok((select_exprs, result_names))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        core::FieldType,
        db::{
            LocaleMode,
            query::test_helpers::{make_field, make_locale_config, make_localized_field},
        },
    };

    #[test]
    fn get_locale_select_columns_default_mode() {
        let fields = vec![make_localized_field("title", FieldType::Text)];
        let locale_cfg = make_locale_config();
        let ctx = LocaleContext {
            mode: LocaleMode::Default,
            config: locale_cfg,
        };
        let (exprs, names) = get_locale_select_columns(&fields, false, &ctx).unwrap();
        assert_eq!(exprs, vec!["id", "\"title__en\" AS \"title\""]);
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
        let (exprs, names) = get_locale_select_columns(&fields, false, &ctx).unwrap();
        assert_eq!(
            exprs,
            vec!["id", "COALESCE(\"title__de\", \"title__en\") AS \"title\""]
        );
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
        let (exprs, names) = get_locale_select_columns(&fields, false, &ctx).unwrap();
        assert_eq!(exprs, vec!["id", "\"title__en\"", "\"title__de\""]);
        assert_eq!(names, vec!["id", "title__en", "title__de"]);
    }

    // ── has_drafts column tests ─────────────────────────────────────────

    /// Regression: `get_locale_select_columns_full` must include `_status`
    /// when `has_drafts` is true.
    #[test]
    fn get_locale_select_columns_full_includes_status_when_has_drafts() {
        let fields = vec![make_field("title", FieldType::Text)];
        let locale_cfg = make_locale_config();
        let ctx = LocaleContext {
            mode: LocaleMode::Default,
            config: locale_cfg,
        };

        let (exprs, names) =
            get_locale_select_columns_full(&fields, false, false, true, &ctx).unwrap();

        assert!(
            exprs.contains(&"_status".to_string()),
            "_status should be in SELECT exprs when has_drafts=true, got: {exprs:?}"
        );
        assert!(
            names.contains(&"_status".to_string()),
            "_status should be in result names when has_drafts=true, got: {names:?}"
        );
    }

    /// Regression: `get_locale_select_columns_full` must NOT include `_status`
    /// when `has_drafts` is false.
    #[test]
    fn get_locale_select_columns_full_excludes_status_when_no_drafts() {
        let fields = vec![make_field("title", FieldType::Text)];
        let locale_cfg = make_locale_config();
        let ctx = LocaleContext {
            mode: LocaleMode::Default,
            config: locale_cfg,
        };

        let (exprs, names) =
            get_locale_select_columns_full(&fields, false, false, false, &ctx).unwrap();

        assert!(
            !exprs.contains(&"_status".to_string()),
            "_status should NOT be in SELECT exprs when has_drafts=false, got: {exprs:?}"
        );
        assert!(
            !names.contains(&"_status".to_string()),
            "_status should NOT be in result names when has_drafts=false, got: {names:?}"
        );
    }
}
