//! The document SELECT list ([`document_select_named`]) and the `select`
//! projection over it (`apply_select_filter`, `apply_select_to_document`).

use std::collections::HashSet;

use anyhow::Result;

use crate::{
    core::{CollectionDefinition, Document},
    db::{
        LocaleContext,
        query::{
            REVISION_COLUMN, get_column_names, get_locale_select_columns_full,
            helpers::{column_belongs_to, quote_ident},
        },
    },
};

/// The SELECT expressions a collection document is read with, each paired with
/// the name it comes back under: the field columns (per locale when locales are
/// on), the gated system columns, and the document's revision.
///
/// The one column list every document read builds on — `find`, `find_by_id`
/// and the auth lookups — so a system column a read returns can never be
/// present on one of them and missing on another.
///
/// # Errors
///
/// Returns an error if any field name conflicts with locale-suffixed naming.
pub(crate) fn document_select_named(
    def: &CollectionDefinition,
    locale_ctx: Option<&LocaleContext>,
) -> Result<(Vec<String>, Vec<String>)> {
    let (mut exprs, mut names) = match locale_ctx {
        Some(ctx) if ctx.config.is_enabled() => get_locale_select_columns_full(
            &def.fields,
            def.timestamps,
            def.soft_delete,
            def.has_drafts(),
            ctx,
        )?,
        _ => {
            let names = get_column_names(def);
            let quoted = names.iter().map(|n| quote_ident(n)).collect();
            (quoted, names)
        }
    };

    exprs.push(quote_ident(REVISION_COLUMN));
    names.push(REVISION_COLUMN.to_string());

    Ok((exprs, names))
}

/// Filter SELECT columns based on a `select` list. If `select` is None or empty,
/// returns all columns (backward compat). Always includes `id`, `created_at`,
/// `updated_at`, `_status` (so cursor pagination can encode the composite
/// `(_status, sort_col, id)` order regardless of caller-provided `select`) and
/// `_revision` (a projected read still hands back the revision a later write
/// sends as its precondition). A selected field keeps every column it stores
/// ([`column_belongs_to`]): per-locale columns, a group's sub-columns, and its
/// `_tz` / `_lang` companions.
pub(super) fn apply_select_filter(
    select_exprs: Vec<String>,
    result_names: Vec<String>,
    select: Option<&Vec<String>>,
) -> (Vec<String>, Vec<String>) {
    let select = match select {
        Some(s) if !s.is_empty() => s,
        _ => return (select_exprs, result_names),
    };

    let selected: HashSet<&str> = select.iter().map(std::string::String::as_str).collect();
    let mut out_exprs = Vec::new();
    let mut out_names = Vec::new();

    for (expr, name) in select_exprs.into_iter().zip(result_names) {
        let dominated_by_select = matches!(
            name.as_str(),
            "id" | "created_at" | "updated_at" | "_status" | REVISION_COLUMN
        ) || selected.iter().any(|field| column_belongs_to(&name, field));

        if dominated_by_select {
            out_exprs.push(expr);
            out_names.push(name);
        }
    }

    (out_exprs, out_names)
}

/// Strip fields not in `select` from a document. Always keeps `id` and the
/// document's `_revision`.
/// Used for post-query field stripping (e.g., after `find_by_id`).
pub fn apply_select_to_document(doc: &mut Document, select: &[String]) {
    let selected: HashSet<&str> = select.iter().map(std::string::String::as_str).collect();

    doc.fields.retain(|key, _| {
        key == REVISION_COLUMN || selected.iter().any(|field| column_belongs_to(key, field))
    });

    if !selected.contains("created_at") {
        doc.created_at = None;
    }
    if !selected.contains("updated_at") {
        doc.updated_at = None;
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// Regression: a selected field kept only the columns named after it with
    /// `__`, so a timezone date lost its `_tz` companion and a code field its
    /// `_lang` companion.
    #[test]
    fn select_keeps_a_fields_companion_columns() {
        let names: Vec<String> = [
            "id",
            "starts",
            "starts_tz",
            "starts_tz__en",
            "snippet",
            "snippet_lang",
            "other",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
        let select = vec!["starts".to_string(), "snippet".to_string()];

        let (_, kept) = apply_select_filter(names.clone(), names, Some(&select));
        assert_eq!(
            kept,
            [
                "id",
                "starts",
                "starts_tz",
                "starts_tz__en",
                "snippet",
                "snippet_lang"
            ]
        );

        let mut doc = Document::new("d1".to_string());
        for key in ["starts", "starts_tz", "snippet", "snippet_lang", "other"] {
            doc.fields.insert(key.to_string(), json!("x"));
        }
        apply_select_to_document(&mut doc, &select);

        let mut keys: Vec<String> = doc.fields.keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["snippet", "snippet_lang", "starts", "starts_tz"]);
    }

    #[test]
    fn apply_select_filter_with_group() {
        let select_exprs = vec![
            "id".to_string(),
            "title".to_string(),
            "seo__meta_title".to_string(),
            "seo__meta_desc".to_string(),
            "created_at".to_string(),
            "updated_at".to_string(),
        ];
        let result_names = select_exprs.clone();

        // Select only "seo" — should include all seo__* sub-columns
        let select = vec!["seo".to_string()];
        let (exprs, names) = apply_select_filter(select_exprs, result_names, Some(&select));

        assert!(names.contains(&"id".to_string()));
        assert!(names.contains(&"seo__meta_title".to_string()));
        assert!(names.contains(&"seo__meta_desc".to_string()));
        assert!(names.contains(&"created_at".to_string()));
        assert!(!names.contains(&"title".to_string()));
        assert_eq!(exprs.len(), names.len());
    }

    #[test]
    fn apply_select_filter_none_returns_all() {
        let exprs = vec!["id".to_string(), "title".to_string(), "status".to_string()];
        let names = exprs.clone();
        let (out_exprs, out_names) = apply_select_filter(exprs.clone(), names.clone(), None);
        assert_eq!(out_exprs, exprs);
        assert_eq!(out_names, names);
    }

    #[test]
    fn apply_select_filter_empty_returns_all() {
        let exprs = vec!["id".to_string(), "title".to_string()];
        let names = exprs.clone();
        let empty: Vec<String> = Vec::new();
        let (out_exprs, out_names) =
            apply_select_filter(exprs.clone(), names.clone(), Some(&empty));
        assert_eq!(out_exprs, exprs);
        assert_eq!(out_names, names);
    }

    /// Regression: `_status` must survive `apply_select_filter` so cursor
    /// pagination can encode the composite `(_status, sort_col, id)` order
    /// even when the caller passes a narrow `select`. Without this,
    /// `cursor_from_doc` would fall back to `"published"` for every row
    /// (drafts included) and round-trip pagination silently skips drafts.
    #[test]
    fn apply_select_filter_keeps_status_for_cursor() {
        let exprs = vec![
            "id".to_string(),
            "title".to_string(),
            "_status".to_string(),
            "created_at".to_string(),
        ];
        let names = exprs.clone();
        let select = vec!["title".to_string()];
        let (_, out_names) = apply_select_filter(exprs, names, Some(&select));
        assert!(
            out_names.contains(&"_status".to_string()),
            "_status must be kept regardless of select; got {out_names:?}"
        );
    }

    #[test]
    fn apply_select_filter_locale_suffix_passthrough() {
        // When a column is "title__de" and select has "title", the locale variant should be included
        let exprs = vec![
            "id".to_string(),
            "title__de".to_string(),
            "title__en".to_string(),
        ];
        let names = exprs.clone();
        let select = vec!["title".to_string()];
        let (_, out_names) = apply_select_filter(exprs, names, Some(&select));
        assert!(out_names.contains(&"id".to_string()));
        assert!(out_names.contains(&"title__de".to_string()));
        assert!(out_names.contains(&"title__en".to_string()));
    }

    #[test]
    fn apply_select_to_document_keeps_selected() {
        let mut doc = Document::new("abc".to_string());
        doc.fields.insert("title".to_string(), json!("Hello"));
        doc.fields.insert("status".to_string(), json!("draft"));
        doc.fields.insert("body".to_string(), json!("Some content"));
        doc.created_at = Some("2024-01-01".to_string());
        doc.updated_at = Some("2024-01-02".to_string());

        let select = vec!["title".to_string()];
        apply_select_to_document(&mut doc, &select);

        // id is always kept (not in fields HashMap, it's a struct field)
        assert_eq!(doc.id, "abc");
        // title was selected, should be kept
        assert!(doc.fields.contains_key("title"));
        // status and body were NOT selected, should be removed
        assert!(!doc.fields.contains_key("status"));
        assert!(!doc.fields.contains_key("body"));
        // timestamps not in select, should be cleared
        assert!(doc.created_at.is_none());
        assert!(doc.updated_at.is_none());
    }

    #[test]
    fn apply_select_to_document_prefix_match() {
        let mut doc = Document::new("x".to_string());
        doc.fields
            .insert("seo__title".to_string(), json!("SEO Title"));
        doc.fields
            .insert("seo__desc".to_string(), json!("SEO Desc"));
        doc.fields.insert("title".to_string(), json!("Main Title"));
        doc.created_at = Some("2024-01-01".to_string());
        doc.updated_at = Some("2024-01-01".to_string());

        // Select only "seo" — should keep seo__* keys via prefix match
        let select = vec!["seo".to_string()];
        apply_select_to_document(&mut doc, &select);

        assert!(
            doc.fields.contains_key("seo__title"),
            "seo__title should be kept by prefix match"
        );
        assert!(
            doc.fields.contains_key("seo__desc"),
            "seo__desc should be kept by prefix match"
        );
        assert!(
            !doc.fields.contains_key("title"),
            "title not in select should be removed"
        );
    }

    #[test]
    fn apply_select_to_document_keeps_created_at_when_selected() {
        let mut doc = Document::new("x".to_string());
        doc.created_at = Some("2024-01-01".to_string());
        doc.updated_at = Some("2024-01-02".to_string());

        let select = vec!["created_at".to_string()];
        apply_select_to_document(&mut doc, &select);

        assert!(
            doc.created_at.is_some(),
            "created_at should be kept when selected"
        );
        assert!(
            doc.updated_at.is_none(),
            "updated_at should be cleared when not selected"
        );
    }

    #[test]
    fn apply_select_to_document_keeps_updated_at_when_selected() {
        let mut doc = Document::new("x".to_string());
        doc.created_at = Some("2024-01-01".to_string());
        doc.updated_at = Some("2024-01-02".to_string());

        let select = vec!["updated_at".to_string()];
        apply_select_to_document(&mut doc, &select);

        assert!(
            doc.updated_at.is_some(),
            "updated_at should be kept when selected"
        );
        assert!(
            doc.created_at.is_none(),
            "created_at should be cleared when not selected"
        );
    }
}
