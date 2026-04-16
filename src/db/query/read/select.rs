//! `apply_select_filter` and `apply_select_to_document` — column/field selection filtering.

use std::collections::HashSet;

use crate::core::Document;

/// Filter SELECT columns based on a `select` list. If `select` is None or empty,
/// returns all columns (backward compat). Always includes `id`, `created_at`, `updated_at`.
/// For group fields: selecting `"seo"` includes all `seo__*` sub-columns.
pub fn apply_select_filter(
    select_exprs: Vec<String>,
    result_names: Vec<String>,
    select: Option<&Vec<String>>,
) -> (Vec<String>, Vec<String>) {
    let select = match select {
        Some(s) if !s.is_empty() => s,
        _ => return (select_exprs, result_names),
    };

    let selected: HashSet<&str> = select.iter().map(|s| s.as_str()).collect();
    let mut out_exprs = Vec::new();
    let mut out_names = Vec::new();

    for (expr, name) in select_exprs.into_iter().zip(result_names) {
        let dominated_by_select = matches!(name.as_str(), "id" | "created_at" | "updated_at")
            || selected.contains(name.as_str())
            || name.split_once("__").is_some_and(|(prefix, _)| {
                // Group prefix: "seo" selected → include "seo__title"
                // Locale suffix: "title" selected → include "title__de"
                selected.contains(prefix)
            });

        if dominated_by_select {
            out_exprs.push(expr);
            out_names.push(name);
        }
    }

    (out_exprs, out_names)
}

/// Strip fields not in `select` from a document. Always keeps `id`.
/// Used for post-query field stripping (e.g., after `find_by_id`).
pub fn apply_select_to_document(doc: &mut Document, select: &[String]) {
    let selected: HashSet<&str> = select.iter().map(|s| s.as_str()).collect();

    doc.fields.retain(|key, _| {
        selected.contains(key.as_str())
            || key
                .split_once("__")
                .is_some_and(|(prefix, _)| selected.contains(prefix))
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
