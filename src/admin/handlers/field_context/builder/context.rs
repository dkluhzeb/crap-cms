//! Top-level field context building: the field list assembly.

use std::collections::HashMap;

use crate::{
    admin::{context::field::FieldContext, handlers::shared::renders_in_admin_form},
    core::FieldDefinition,
};

use super::single::build_single_field_context;

#[cfg(test)]
mod tests;

/// Build field context objects for template rendering.
///
/// `filter_hidden`: when true, fields with `admin.hidden = true` are skipped
/// (form rendering — false during error re-renders so the user's entered
/// values are preserved across the round-trip).
///
/// Fields with top-level `hidden = true` are *always* skipped — the data is
/// stripped from API responses, so there's nothing to render and no value to
/// preserve. The `filter_hidden` toggle does not apply.
///
/// `non_default_locale`: when true, non-localized fields are rendered readonly
/// (locked) because they are shared across all locales and should only be
/// edited from the default locale.
///
/// Returns `Vec<FieldContext>` — typed end-to-end. Downstream consumers
/// (`enrich_field_contexts`, `apply_display_conditions`,
/// `split_sidebar_fields`, the `fields` field on each typed page context)
/// all consume typed values; serialization to `serde_json::Value` happens
/// once when the page context is serialized for the `before_render` hook.
pub fn build_field_contexts(
    fields: &[FieldDefinition],
    values: &HashMap<String, String>,
    errors: &HashMap<String, String>,
    filter_hidden: bool,
    non_default_locale: bool,
) -> Vec<FieldContext> {
    visible_field_defs(fields, filter_hidden)
        .map(|field| {
            build_single_field_context(field, values, errors, "", non_default_locale, false, 0)
        })
        .collect()
}

/// The top-level field defs that become form fields, in order — the **single
/// source of truth** for "which top-level fields are visible." Shared by
/// [`build_field_contexts`], `apply_display_conditions`, and
/// `enrich_field_contexts` so their per-field `zip`s stay aligned: `field.hidden`
/// is always dropped, `admin.hidden` only when `filter_hidden`. Duplicating this
/// filter (as these three sites used to) silently desyncs the zips when a
/// top-level hidden field shifts the pairing.
///
/// With `filter_hidden` the question is [`renders_in_admin_form`] — the same
/// answer the submit-side normalizers use to decide what an absent key means.
///
/// Sub-fields ask
/// [`admin_form_fields`](crate::admin::handlers::shared::admin_form_fields)
/// instead, which is that same answer without the toggle: a composite's
/// children are only ever built for the form, so a hidden one is rendered at no
/// depth.
pub(in crate::admin::handlers::field_context) fn visible_field_defs(
    field_defs: &[FieldDefinition],
    filter_hidden: bool,
) -> impl Iterator<Item = &FieldDefinition> {
    field_defs.iter().filter(move |f| {
        if filter_hidden {
            renders_in_admin_form(f)
        } else {
            !f.hidden
        }
    })
}
