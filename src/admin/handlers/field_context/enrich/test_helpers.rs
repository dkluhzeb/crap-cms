//! Shared `#[cfg(test)]` fixtures for the `enrich/*` test modules.
//!
//! The wrappers convert the typed enrichment-API output to `serde_json::Value`
//! so existing JSON-style assertions (which test the wire shape consumed by
//! templates) keep working. [`make_test_state`] builds a minimal in-memory
//! [`AdminState`] for tests that need DB access.

use std::collections::HashMap;

use serde_json::Value;

use crate::{
    admin::{
        AdminState,
        context::field::{FieldContext, RichtextField},
        handlers::field_context::{builder::build_field_contexts, enrich},
    },
    core::{DocumentFields, FieldDefinition, FieldType, Registry},
    db::{DbConnection, query::LocaleContext},
};

use super::{EnrichOptions, SubFieldOpts, types};

/// Build a minimal [`FieldDefinition`] with default admin/validation settings.
pub(super) fn make_field(name: &str, ft: FieldType) -> FieldDefinition {
    FieldDefinition::builder(name, ft).build()
}

/// Run [`build_field_contexts`] and serialize each typed result to its wire
/// JSON. Mirrors the parent module's `test_helpers::build_value_contexts` —
/// duplicated here because that helper is `pub(super)`-scoped to its parent
/// module.
pub(super) fn build_value_contexts(
    fields: &[FieldDefinition],
    values: &HashMap<String, String>,
    errors: &HashMap<String, String>,
    filter_hidden: bool,
    non_default_locale: bool,
) -> Vec<Value> {
    build_field_contexts(fields, values, errors, filter_hidden, non_default_locale)
        .into_iter()
        .map(|fc| fc.to_value())
        .collect()
}

/// Test wrapper for [`super::build_enriched_sub_field_context`] returning
/// `Value` so existing JSON-style assertions keep working.
pub(super) fn build_enriched_sub_field_value(
    sf: &FieldDefinition,
    raw_value: Option<&Value>,
    parent_name: &str,
    idx: usize,
    opts: &SubFieldOpts,
) -> Value {
    super::build_enriched_sub_field_context(sf, raw_value, parent_name, idx, opts).to_value()
}

/// Test wrapper for [`super::enrich_field_contexts`] taking `&mut Vec<Value>`.
/// Deserializes at entry, calls the production fn, reserializes at exit.
pub(super) fn enrich_field_contexts_values(
    fields: &mut Vec<Value>,
    field_defs: &[FieldDefinition],
    doc_fields: &DocumentFields,
    state: &AdminState,
    opts: &EnrichOptions,
) {
    let mut typed: Vec<FieldContext> = fields
        .iter()
        .map(|v| serde_json::from_value(v.clone()).expect("test field-context must deserialize"))
        .collect();
    enrich::enrich_field_contexts(&mut typed, field_defs, doc_fields, state, opts);
    *fields = typed.into_iter().map(|fc| fc.to_value()).collect();
}

/// Test wrapper for [`super::enrich_nested_fields`] taking `&mut Vec<Value>`.
/// Builds a minimal [`EnrichCtx`] (no viewer → the `test_default` state's
/// `default_deny = false` means a collection with no access rule stays visible,
/// preserving these label-presence assertions).
pub(super) fn enrich_nested_fields_values(
    sub_fields: &mut Vec<Value>,
    field_defs: &[FieldDefinition],
    conn: &dyn DbConnection,
    reg: &Registry,
    rel_locale_ctx: Option<&LocaleContext>,
) {
    let mut typed: Vec<FieldContext> = sub_fields
        .iter()
        .map(|v| serde_json::from_value(v.clone()).expect("test sub-field must deserialize"))
        .collect();

    let state = make_test_state();
    let errors = HashMap::new();
    let ctx = enrich::EnrichCtx {
        state: &state,
        non_default_locale: false,
        errors: &errors,
        conn,
        reg,
        rel_locale_ctx,
        user: None,
    };

    super::enrich_nested_fields(&mut typed, field_defs, &ctx);
    *sub_fields = typed.into_iter().map(|fc| fc.to_value()).collect();
}

/// Test wrapper for `types::enrich_richtext` taking `&mut Value`.
pub(super) fn enrich_richtext_value(ctx: &mut Value, reg: &Registry) {
    let mut typed: RichtextField =
        serde_json::from_value(ctx.clone()).expect("test richtext ctx must deserialize");
    types::enrich_richtext(&mut typed, reg);
    *ctx = serde_json::to_value(typed).expect("RichtextField serializes infallibly");
}

/// Build a minimal [`AdminState`] backed by an in-memory `SQLite` pool. Used by
/// tests that exercise DB-touching enrichment paths. `default_deny = false`: a
/// collection with no access rule stays readable, so enrichment label-presence
/// tests (no access configured) behave as before.
pub(super) use crate::admin::test_state::{
    test_admin_state as make_test_state, test_admin_state_with_deny as make_test_state_with_deny,
    test_admin_state_with_registry as make_test_state_with_registry,
};
