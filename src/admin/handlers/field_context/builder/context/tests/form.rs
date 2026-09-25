//! Form-level concerns: hidden fields, errors, locale locking, position and labels.

use std::collections::HashMap;

use crate::{
    admin::handlers::field_context::{
        builder::visible_field_defs,
        test_helpers::{build_value_contexts, make_field},
    },
    core::{FieldType, LocalizedString},
};

// ── filter_hidden ─────────────────────────────────────────────────

#[test]
fn build_field_contexts_filter_hidden_removes_hidden_fields() {
    let mut hidden_field = make_field("secret", FieldType::Text);
    hidden_field.admin.hidden = true;
    let fields = vec![
        make_field("title", FieldType::Text),
        hidden_field,
        make_field("body", FieldType::Textarea),
    ];
    let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), true, false);
    assert_eq!(result.len(), 2);
    assert_eq!(result[0]["name"], "title");
    assert_eq!(result[1]["name"], "body");
}

#[test]
fn build_field_contexts_no_filter_includes_hidden_fields() {
    let mut hidden_field = make_field("secret", FieldType::Text);
    hidden_field.admin.hidden = true;
    let fields = vec![make_field("title", FieldType::Text), hidden_field];
    let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
    assert_eq!(result.len(), 2);
}

/// Top-level `hidden = true` is unconditional: even with `filter_hidden = false`
/// the field is skipped, because its data is stripped from API responses.
#[test]
fn build_field_contexts_top_level_hidden_always_skipped() {
    let mut api_hidden_field = make_field("internal", FieldType::Text);
    api_hidden_field.hidden = true;

    let fields = vec![make_field("title", FieldType::Text), api_hidden_field];

    let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0]["name"], "title");
}

/// Regression (zip-alignment): `visible_field_defs` must yield the SAME
/// field sequence `build_field_contexts` builds — `apply_display_conditions`
/// and `enrich_field_contexts` zip their per-field results against it, so a
/// divergence silently applies the wrong field's condition/enrichment. A
/// top-level `hidden` field before later fields used to desync them (defs
/// kept the hidden field; the built contexts dropped it).
#[test]
fn visible_field_defs_matches_built_contexts_with_hidden_fields() {
    let mut internal = make_field("internal", FieldType::Text);
    internal.hidden = true; // always dropped
    let mut admin_only = make_field("admin_only", FieldType::Text);
    admin_only.admin.hidden = true; // dropped only when filter_hidden
    let fields = vec![
        internal,
        make_field("title", FieldType::Text),
        admin_only,
        make_field("body", FieldType::Text),
    ];

    for filter_hidden in [true, false] {
        let built = build_value_contexts(
            &fields,
            &HashMap::new(),
            &HashMap::new(),
            filter_hidden,
            false,
        );
        let built_names: Vec<&str> = built.iter().map(|c| c["name"].as_str().unwrap()).collect();
        let def_names: Vec<&str> = visible_field_defs(&fields, filter_hidden)
            .map(|f| f.name.as_str())
            .collect();
        assert_eq!(built_names, def_names, "filter_hidden={filter_hidden}");
    }
}

/// `admin.hidden = true` (only) is the upload-meta case: the data must NOT
/// be stripped from API responses, only from the admin form when
/// `filter_hidden = true`.
#[test]
fn build_field_contexts_admin_hidden_does_not_imply_top_level_hidden() {
    let mut admin_hidden = make_field("url", FieldType::Text);
    admin_hidden.admin.hidden = true;
    assert!(!admin_hidden.hidden);

    let fields = vec![make_field("title", FieldType::Text), admin_hidden];

    let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
    assert_eq!(result.len(), 2);
}

// ── Error propagation ─────────────────────────────────────────────

#[test]
fn build_field_contexts_errors_attached_to_fields() {
    let fields = vec![make_field("title", FieldType::Text)];
    let mut errors = HashMap::new();
    errors.insert("title".to_string(), "Title is required".to_string());
    let result = build_value_contexts(&fields, &HashMap::new(), &errors, false, false);
    assert_eq!(result[0]["error"], "Title is required");
}

#[test]
fn build_field_contexts_no_error_when_field_valid() {
    let fields = vec![make_field("title", FieldType::Text)];
    let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
    assert!(result[0].get("error").is_none());
}

// ── Locale locking ────────────────────────────────────────────────

#[test]
fn build_field_contexts_locale_locked_non_localized_field() {
    let fields = vec![make_field("slug", FieldType::Text)];
    let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, true);
    assert_eq!(result[0]["locale_locked"], true);
    assert_eq!(result[0]["readonly"], true);
}

#[test]
fn build_field_contexts_localized_field_not_locked() {
    let mut field = make_field("title", FieldType::Text);
    field.localized = true;
    let fields = vec![field];
    let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, true);
    assert_eq!(result[0]["locale_locked"], false);
    assert_eq!(result[0]["readonly"], false);
}

/// A non-localized field inside a *localized* group is editable in a
/// non-default locale. The group carries its children per-locale (localization
/// inherits), so `construct_group` builds them with `non_default_locale =
/// false` → `locale_locked_display` returns `false`. This matches the server's
/// `is_locale_locked_write`, which returns `false` for inherited localization.
/// Regression guard for the admin-vs-server parity a chokepoint audit flagged
/// as a possible mismatch — the admin encodes inheritance via
/// `non_default_locale`, not the locked formula, so there is no mismatch.
#[test]
fn build_field_contexts_non_localized_field_in_localized_group_is_editable() {
    let mut group = make_field("seo", FieldType::Group);
    group.localized = true;
    // The child does NOT set `localized` itself — it inherits from the group.
    group.fields = vec![make_field("slug", FieldType::Text)];
    let fields = vec![group];

    let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, true);
    let sub = result[0]["sub_fields"].as_array().unwrap();
    assert_eq!(
        sub[0]["locale_locked"], false,
        "a child of a localized group must be editable in a non-default locale"
    );
    assert_eq!(sub[0]["readonly"], false);
}

// ── Position / labels / readonly ──────────────────────────────────

#[test]
fn build_field_contexts_position_set() {
    let mut field = make_field("status", FieldType::Text);
    field.admin.position = Some("sidebar".to_string());
    let fields = vec![field];
    let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
    assert_eq!(result[0]["position"], "sidebar");
}

#[test]
fn build_field_contexts_custom_label_placeholder_description() {
    let mut field = make_field("title", FieldType::Text);
    field.admin.label = Some(LocalizedString::Plain("Custom Title".to_string()));
    field.admin.placeholder = Some(LocalizedString::Plain("Enter title here...".to_string()));
    field.admin.description = Some(LocalizedString::Plain("The main title".to_string()));
    let fields = vec![field];
    let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
    assert_eq!(result[0]["label"], "Custom Title");
    assert_eq!(result[0]["placeholder"], "Enter title here...");
    assert_eq!(result[0]["description"], "The main title");
}

#[test]
fn build_field_contexts_readonly_field() {
    let mut field = make_field("slug", FieldType::Text);
    field.admin.readonly = true;
    let fields = vec![field];
    let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
    assert_eq!(result[0]["readonly"], true);
}
