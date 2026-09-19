//! Shared utilities for field context building: error counting, timezone handling,
//! display conditions, and template-safe naming.

use std::collections::HashMap;

use serde_json::{Map, Value, from_str, json};

use crate::{
    admin::{
        context::field::{DateField, FieldContext, NonRepeatingChildren},
        handlers::{field_context::builder::visible_field_defs, shared::admin_form_fields},
    },
    core::{FieldDefinition, HookRef},
    db::query::helpers::{lang_column, tz_column, utc_to_local},
    hooks::{ConditionContext, HookRunner, lifecycle::DisplayConditionResult},
};

/// One element of a multi-value list as the editor sees it. A null element
/// shows nothing; a number or bool shows as its text.
fn element_tag(element: &Value) -> Option<String> {
    match element {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

/// The tags a multi-value field shows: the elements of its stored list — text
/// or numbers, as a read returns them — as strings. Nulls and a malformed list
/// show no tags.
pub fn tag_values(value: &str) -> Vec<String> {
    from_str::<Vec<Value>>(value)
        .unwrap_or_default()
        .iter()
        .filter_map(element_tag)
        .collect()
}

/// The same tags for a list a read already returned as JSON, so a caller
/// holding the parsed value doesn't have to re-serialize it to ask. A list
/// still in its stored JSON text is parsed first; anything else shows no tags.
pub fn tag_values_of(value: &Value) -> Vec<String> {
    match value {
        Value::Array(elements) => elements.iter().filter_map(element_tag).collect(),
        Value::String(text) => tag_values(text),
        _ => Vec::new(),
    }
}

/// The hidden-input value a tag widget round-trips: the list as a JSON array.
/// Comma-joining could not represent an element that contains a comma — the
/// widget split it back into two tags, and the next save stored them that way.
pub fn tags_input_value(tags: &[String]) -> Value {
    Value::String(json!(tags).to_string())
}

/// Max nesting depth for recursive field context building (guard against infinite nesting).
pub const MAX_FIELD_DEPTH: usize = 5;

/// Whether a field renders read-only ("locale locked") in the admin editor: it's
/// a non-default locale and the field itself is not localized, so it carries the
/// canonical default-locale value that must be edited from the default locale.
/// One definition for the admin-UI companion to the server-side
/// `is_locale_locked_write`, shared by every field-context builder so the call
/// sites can't drift.
///
/// This takes only the field's own `localized` flag on purpose: **inherited**
/// localization is already folded into `non_default_locale` by the caller — a
/// localized Group builds its children with `non_default_locale = false` (see
/// `construct_group`), so a non-localized field inside a localized group is
/// correctly editable, matching the server's `is_locale_locked_write` returning
/// `false` for inherited localization.
#[must_use]
pub fn locale_locked_display(non_default_locale: bool, field: &FieldDefinition) -> bool {
    non_default_locale && !field.localized
}

/// Make a template-ID-safe string from a field name (replaces `[`, `]` with `-`).
pub fn safe_template_id(name: &str) -> String {
    name.replace('[', "-").replace(']', "")
}

/// Count errors recursively across typed [`FieldContext`] sub-fields.
///
/// Walks every leaf's `base.error` and recurses through layout wrappers
/// (Group/Collapsible/Row/Tabs) and composites (Array/Blocks rows). Used
/// by the Tabs constructor to populate `tabs[*].error_count` so the UI can
/// surface validation errors hidden behind tab switches.
pub fn count_errors_in_field_contexts(fields: &[FieldContext]) -> usize {
    fields
        .iter()
        .map(|fc| {
            // Own error + all descendant errors, walking children through the
            // SHARED FieldContext classifier (`child_field_slices`) rather
            // than a hand-rolled `match` — one compile-forced dispatch, so a
            // new composite can't be silently skipped here.
            usize::from(fc.base().error.is_some())
                + fc.child_field_slices()
                    .iter()
                    .map(|slice| count_errors_in_field_contexts(slice))
                    .sum::<usize>()
        })
        .sum()
}

/// Collect richtext node attribute errors for a given field name.
/// Matches error keys like `{field_name}[cta#0].text` and joins messages.
pub fn collect_node_attr_errors(
    errors: &HashMap<String, String>,
    field_name: &str,
) -> Option<String> {
    let prefix = format!("{field_name}[");

    let msgs: Vec<&str> = errors
        .iter()
        .filter(|(k, _)| k.starts_with(&prefix))
        .map(|(_, v)| v.as_str())
        .collect();

    if msgs.is_empty() {
        None
    } else {
        Some(msgs.join("; "))
    }
}

/// Split field contexts into main and sidebar based on the `position` property.
/// Returns `(main_fields, sidebar_fields)`.
pub fn split_sidebar_fields(fields: Vec<FieldContext>) -> (Vec<FieldContext>, Vec<FieldContext>) {
    fields
        .into_iter()
        .partition(|fc| fc.base().position.as_deref() != Some("sidebar"))
}

// ── Timezone helpers ────────────────────────────────────────────────

/// Inject stored timezone values into date sub-field contexts from a parent row object.
///
/// For each sub-field definition that is a date field with `timezone: true`, looks up
/// `{field_name}_tz` in the parent row and sets `timezone_value` on the corresponding context.
///
/// The defs run through [`admin_form_fields`], the same filter the row's
/// sub-field contexts were built with, so the pairing stays aligned.
pub fn inject_timezone_values_from_row(
    sub_ctxs: &mut [FieldContext],
    field_defs: &[FieldDefinition],
    parent_row: Option<&Map<String, Value>>,
) {
    let Some(row_obj) = parent_row else {
        return;
    };

    for (fc, fd) in sub_ctxs.iter_mut().zip(admin_form_fields(field_defs)) {
        if fd.has_tz_companion()
            && let FieldContext::Date(df) = fc
        {
            let tz_key = tz_column(&fd.name);
            if let Some(tz_val) = row_obj.get(&tz_key).and_then(|v| v.as_str()) {
                df.timezone_value = Some(tz_val.to_string());

                if let Some(stored) = row_obj.get(&fd.name).and_then(Value::as_str) {
                    localize_date_display(df, stored, tz_val);
                }
            }
        }
    }
}

/// Show a row date in its own timezone: replace the input's value with the
/// stored UTC value converted to local time in `tz`, as the top-level builder
/// does with the document's `_tz` column. Without this the form shows the UTC
/// digits, and saving them back as local time shifts the date by its offset.
pub fn localize_date_display(df: &mut DateField, stored: &str, tz: &str) {
    if stored.is_empty() || tz.is_empty() {
        return;
    }

    // Only a stored UTC instant converts; anything else (a local value
    // re-rendered from a submitted form) stays as typed.
    let Some(local) = utc_to_local(stored, tz) else {
        return;
    };

    let values = cut_to_appearance(&local, &df.picker_appearance);
    apply_picker_values(df, values);
}

/// The values a date picker shows for a stored date: the date in its zone when
/// one is stored, cut to what the picker shows — the date for `dayOnly`, the date
/// and time for `dayAndTime`. The one display conversion every date context uses.
pub fn date_picker_values(
    stored: &str,
    tz: &str,
    appearance: &str,
) -> (Option<String>, Option<String>) {
    let display = if tz.is_empty() || stored.is_empty() {
        stored.to_string()
    } else {
        utc_to_local(stored, tz).unwrap_or_else(|| stored.to_string())
    };

    cut_to_appearance(&display, appearance)
}

/// A display date cut to what a picker of `appearance` shows: the date for
/// `dayOnly`, the date and time for `dayAndTime` — with the seconds when the
/// value carries any, so a save re-submits what is stored — neither otherwise.
///
/// `timeOnly` and `monthOnly` deliberately cut to neither: their inputs render
/// the stored value as it is, and only `dayAndTime` may carry a zone at all
/// (`parse_date_config` drops `timezone` for every other appearance), so there
/// is no zone conversion left to cut down for them.
fn cut_to_appearance(display: &str, appearance: &str) -> (Option<String>, Option<String>) {
    match appearance {
        "dayOnly" => (Some(display.get(..10).unwrap_or(display).to_string()), None),
        "dayAndTime" => (None, Some(datetime_local_value(display))),
        _ => (None, None),
    }
}

/// A display datetime as `<input type="datetime-local">` takes it:
/// `YYYY-MM-DDTHH:MM`, or `YYYY-MM-DDTHH:MM:SS` when the seconds are not zero.
fn datetime_local_value(display: &str) -> String {
    let keeps_seconds = display
        .get(16..19)
        .is_some_and(|s| s.starts_with(':') && s != ":00");
    let end = if keeps_seconds { 19 } else { 16 };

    display.get(..end).unwrap_or(display).to_string()
}

/// The `step` a time-carrying input needs to show and re-submit the seconds
/// its value carries — `"1"` when it carries any, `None` otherwise. Without it
/// the browser drops the seconds, and a save with nothing changed stores a
/// different value.
fn seconds_step(value: &str) -> Option<String> {
    (value.split(':').count() >= 3).then(|| "1".to_string())
}

/// The `step` a picker of `appearance` needs for a stored date: `"1"` for a
/// `dayAndTime` value shown with its seconds or a `timeOnly` value that has
/// seconds at all, `None` otherwise.
pub fn picker_step(stored: &str, tz: &str, appearance: &str) -> Option<String> {
    match appearance {
        "dayAndTime" => date_picker_values(stored, tz, appearance)
            .1
            .as_deref()
            .and_then(seconds_step),
        "timeOnly" => seconds_step(stored),
        _ => None,
    }
}

/// Set the picker values of `df` for a stored date, leaving a value the
/// picker's appearance doesn't show as it is.
pub fn set_date_picker_values(df: &mut DateField, stored: &str, tz: &str) {
    let values = date_picker_values(stored, tz, &df.picker_appearance);
    apply_picker_values(df, values);

    if df.picker_appearance == "timeOnly" {
        df.step = seconds_step(stored);
    }
}

/// Write picker values into `df`, leaving a value the appearance doesn't show
/// (`None`) as it is. A datetime shown with its seconds sets the `step` that
/// keeps them.
fn apply_picker_values(df: &mut DateField, values: (Option<String>, Option<String>)) {
    let (date_only, datetime_local) = values;

    if date_only.is_some() {
        df.date_only_value = date_only;
    }
    if let Some(datetime_local) = datetime_local {
        df.step = seconds_step(&datetime_local);
        df.datetime_local_value = Some(datetime_local);
    }
}

/// The text a JSON field's textarea shows for a stored value: JSON text
/// pretty-printed, so an object or list reads as one; any other text as it is.
/// The write parses the text back, so a save with nothing changed stores the
/// same value.
pub fn json_textarea_value(text: &str) -> String {
    let Ok(value) = from_str::<Value>(text) else {
        return text.to_string();
    };

    if !matches!(value, Value::Object(_) | Value::Array(_)) {
        return text.to_string();
    }

    serde_json::to_string_pretty(&value).unwrap_or_else(|_| text.to_string())
}

/// Inject stored language picks and the picker allow-list into code sub-field
/// contexts from a parent row object.
///
/// For each sub-field definition that is a code field with a non-empty
/// `admin.languages` allow-list, looks up `{field_name}_lang` in the parent
/// row and sets `language` on the context (when present and non-empty), plus
/// emits the `languages` allow-list so the template can render the picker.
/// Mirrors the timezone pattern; both companions are stored as adjacent JSON
/// keys when the field is nested inside an array/blocks row, and both pair
/// their defs through [`admin_form_fields`] so the alignment holds.
pub fn inject_lang_values_from_row(
    sub_ctxs: &mut [FieldContext],
    field_defs: &[FieldDefinition],
    parent_row: Option<&Map<String, Value>>,
) {
    let Some(row_obj) = parent_row else {
        return;
    };

    for (fc, fd) in sub_ctxs.iter_mut().zip(admin_form_fields(field_defs)) {
        if !fd.has_lang_companion() {
            continue;
        }

        let FieldContext::Code(cf) = fc else { continue };

        cf.languages = Some(fd.admin.languages.clone());

        let lang_key = lang_column(&fd.name);
        if let Some(lang_val) = row_obj
            .get(&lang_key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            cf.language = lang_val.to_string();
        }
    }
}

// ── Display conditions ──────────────────────────────────────────────

/// What one display-condition pass needs beyond the fields it walks.
struct ConditionPass<'a, 'c> {
    form_data: &'a Value,
    hook_runner: &'a HookRunner,
    cond_ctx: &'a ConditionContext<'c>,
}

/// Evaluate display conditions for field contexts and inject condition data.
/// For fields with `admin.condition`, calls the Lua function and sets:
/// - `condition_visible`: initial visibility (bool)
/// - `condition_json`: condition table for client-side evaluation (if table returned)
/// - `condition_ref`: Lua function ref for server-side evaluation (if bool returned)
pub fn apply_display_conditions(
    fields: &mut [FieldContext],
    field_defs: &[FieldDefinition],
    form_data: &Value,
    hook_runner: &HookRunner,
    filter_hidden: bool,
    cond_ctx: &ConditionContext<'_>,
) {
    // Same visibility filter `build_field_contexts` used to produce `fields`,
    // so the per-field `zip` below stays aligned (see `visible_field_defs`).
    let defs: Vec<&FieldDefinition> = visible_field_defs(field_defs, filter_hidden).collect();

    let pass = ConditionPass {
        form_data,
        hook_runner,
        cond_ctx,
    };

    apply_conditions_to_level(fields, &defs, &pass);
}

/// The same pass one level down. Sub-field contexts are built from the fields
/// the form renders, so the defs filter through [`admin_form_fields`] — the
/// builder's filter — and the pairing holds at every depth.
fn apply_nested_display_conditions(
    fields: &mut [FieldContext],
    field_defs: &[FieldDefinition],
    pass: &ConditionPass<'_, '_>,
) {
    let defs: Vec<&FieldDefinition> = admin_form_fields(field_defs).collect();

    apply_conditions_to_level(fields, &defs, pass);
}

/// The refs to evaluate at one level, each paired with the `defs` slot it came
/// from so results map back per-field — NOT by ref string, which is no longer
/// unique once two fields can share a ref with different `options`.
fn conditioned_slots<'a>(defs: &[&'a FieldDefinition]) -> Vec<(usize, &'a HookRef)> {
    defs.iter()
        .enumerate()
        .filter_map(|(i, fd)| fd.admin.condition.as_ref().map(|c| (i, c)))
        .collect()
}

/// One condition result per `defs` slot — `None` where the field declares no
/// condition, and an all-`None` row of the right length when no field at this
/// level declares one at all.
///
/// Always one slot per def, never an empty vec: the caller's walk is driven by
/// this, and a level whose own fields carry no condition still has to be walked
/// — a condition can sit on a field nested inside one of them.
fn level_results(
    defs: &[&FieldDefinition],
    pass: &ConditionPass<'_, '_>,
) -> Vec<Option<DisplayConditionResult>> {
    let mut by_def: Vec<Option<DisplayConditionResult>> = (0..defs.len()).map(|_| None).collect();

    let conditioned = conditioned_slots(defs);
    if conditioned.is_empty() {
        return by_def;
    }

    let conditions: Vec<(&HookRef, &Value)> = conditioned
        .iter()
        .map(|&(_, c)| (c, pass.form_data))
        .collect();

    let results = pass
        .hook_runner
        .call_display_conditions_batch(&conditions, pass.cond_ctx);

    // Scatter positional results back to their def slots.
    for (&(def_idx, _), result) in conditioned.iter().zip(results) {
        by_def[def_idx] = result;
    }

    by_def
}

/// Evaluate and apply the conditions of one already-resolved level, then
/// recurse into its non-repeating containers.
fn apply_conditions_to_level(
    fields: &mut [FieldContext],
    defs: &[&FieldDefinition],
    pass: &ConditionPass<'_, '_>,
) {
    for ((fc, field_def), result) in fields
        .iter_mut()
        .zip(defs.iter())
        .zip(level_results(defs, pass))
    {
        if let Some(result) = result {
            apply_single_condition(fc, field_def, &result);
        }

        recurse_into_children(fc, field_def, pass);
    }
}

/// Recurse into non-repeating containers via the SHARED `FieldContext`
/// classifier (`non_repeating_children_mut`), so conditions on fields nested in
/// a group/collapsible/row/tabs evaluate against the same form data. The
/// classifier is the single, compile-forced source for "which children share
/// this scope" — array/blocks ROWS (per-row scope) classify as `None` and are a
/// separate feature. Pairing the child `FieldContext`s with their
/// `FieldDefinition`s is the one place the def side is selected.
fn recurse_into_children(
    fc: &mut FieldContext,
    field_def: &FieldDefinition,
    pass: &ConditionPass<'_, '_>,
) {
    match fc.non_repeating_children_mut() {
        NonRepeatingChildren::Flat(sub_fields) => {
            apply_nested_display_conditions(sub_fields, &field_def.fields, pass);
        }
        NonRepeatingChildren::Tabs(panes) => {
            for (pane, tab_def) in panes.iter_mut().zip(field_def.tabs.iter()) {
                apply_nested_display_conditions(&mut pane.sub_fields, &tab_def.fields, pass);
            }
        }
        NonRepeatingChildren::None => {}
    }
}

/// Apply a single display condition result to a field context.
fn apply_single_condition(
    fc: &mut FieldContext,
    field_def: &FieldDefinition,
    result: &DisplayConditionResult,
) {
    let Some(ref cond_ref) = field_def.admin.condition else {
        return;
    };

    let condition = &mut fc.base_mut().condition;

    match result {
        DisplayConditionResult::Bool(visible) => {
            condition.visible = Some(*visible);
            condition.func_ref = Some(cond_ref.reference().to_string());
        }
        DisplayConditionResult::Table {
            condition: cond,
            visible,
        } => {
            condition.visible = Some(*visible);
            condition.expr = Some(cond.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::{
        admin::context::field::{BaseFieldData, CodeField, DateField, FieldContext as FC},
        core::field::{FieldAdmin, FieldDefinition, FieldType},
    };

    use super::*;

    // ── display conditions ────────────────────────────────────────────

    fn conditioned(name: &str, reference: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text)
            .admin(
                FieldAdmin::builder()
                    .condition(HookRef::new(reference))
                    .build(),
            )
            .build()
    }

    /// Regression: the walk returned before its loop when no field at a level
    /// declared a condition, so it never descended — a condition on a field
    /// inside a group never fired unless one of the group's *siblings* also had
    /// one. The result row is now always one slot per def, whether or not this
    /// level has anything to evaluate, so the walk always continues.
    #[test]
    fn a_level_with_no_conditions_still_yields_one_slot_per_field() {
        let plain = [
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("body", FieldType::Text).build(),
        ];
        let defs: Vec<&FieldDefinition> = plain.iter().collect();

        assert!(
            conditioned_slots(&defs).is_empty(),
            "nothing at this level is conditioned"
        );
    }

    /// A conditioned field is paired with its own slot, so a preceding
    /// unconditioned field cannot shift the result onto the wrong field.
    #[test]
    fn a_conditioned_field_keeps_its_own_slot() {
        let mixed = [
            FieldDefinition::builder("title", FieldType::Text).build(),
            conditioned("url", "hooks.conditions.show_when_online"),
        ];
        let defs: Vec<&FieldDefinition> = mixed.iter().collect();

        let slots = conditioned_slots(&defs);
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0].0, 1, "the second field, not the first");
        assert_eq!(slots[0].1.reference(), "hooks.conditions.show_when_online");
    }

    // ── tag values ────────────────────────────────────────────────────

    /// Regression: an element containing a comma must survive the widget
    /// round-trip. The hidden input carries the list as JSON, so the comma
    /// stays inside its element instead of splitting it into two tags.
    #[test]
    fn a_tag_containing_a_comma_round_trips() {
        let tags = tag_values(r#"["Hello, world","plain"]"#);
        assert_eq!(tags, vec!["Hello, world".to_string(), "plain".to_string()]);

        assert_eq!(
            tags_input_value(&tags),
            json!(r#"["Hello, world","plain"]"#),
            "the widget reads back exactly the elements it was given"
        );
        assert_eq!(
            tag_values(tags_input_value(&tags).as_str().unwrap()),
            tags,
            "round trip is lossless"
        );
    }

    /// The parsed-value flavor answers the same as the text one, for a caller
    /// that already holds the read's JSON.
    #[test]
    fn tag_values_of_matches_the_text_form() {
        assert_eq!(
            tag_values_of(&json!(["a", 2, null, true])),
            vec!["a".to_string(), "2".to_string(), "true".to_string()]
        );
        assert_eq!(
            tag_values_of(&json!(r#"["a","b"]"#)),
            vec!["a".to_string(), "b".to_string()],
            "a list still in its stored JSON text is parsed"
        );
        assert!(tag_values_of(&json!(null)).is_empty());
        assert!(tag_values_of(&json!(7)).is_empty());
    }

    fn date_field_with_tz(name: &str) -> FieldDefinition {
        FieldDefinition {
            name: name.to_string(),
            field_type: FieldType::Date,
            timezone: true,
            default_timezone: Some("America/New_York".to_string()),
            ..Default::default()
        }
    }

    fn date_field_no_tz(name: &str) -> FieldDefinition {
        FieldDefinition {
            name: name.to_string(),
            field_type: FieldType::Date,
            ..Default::default()
        }
    }

    fn date_ctx(name: &str, tz_value: Option<&str>) -> FC {
        FC::Date(DateField {
            base: BaseFieldData {
                name: name.to_string(),
                ..Default::default()
            },
            timezone_value: tz_value.map(str::to_string),
            ..Default::default()
        })
    }

    fn code_ctx(name: &str, language: &str) -> FC {
        FC::Code(CodeField {
            base: BaseFieldData {
                name: name.to_string(),
                ..Default::default()
            },
            language: language.to_string(),
            languages: None,
        })
    }

    #[test]
    fn inject_timezone_values_from_row_sets_tz_for_date_fields() {
        let field_defs = vec![date_field_with_tz("starts_at"), date_field_no_tz("ends_at")];

        let mut ctxs = vec![
            date_ctx("items[0][starts_at]", Some("")),
            date_ctx("items[0][ends_at]", None),
        ];

        let row: Map<String, Value> = serde_json::from_value(json!({
            "starts_at": "2026-01-15",
            "starts_at_tz": "Asia/Tokyo",
            "ends_at": "2026-02-15",
        }))
        .unwrap();

        inject_timezone_values_from_row(&mut ctxs, &field_defs, Some(&row));

        let FC::Date(d0) = &ctxs[0] else {
            panic!("expected date")
        };
        assert_eq!(d0.timezone_value.as_deref(), Some("Asia/Tokyo"));

        let FC::Date(d1) = &ctxs[1] else {
            panic!("expected date")
        };
        assert_eq!(d1.timezone_value, None);
    }

    #[test]
    fn inject_timezone_values_from_row_noop_when_no_row() {
        let field_defs = vec![date_field_with_tz("starts_at")];
        let mut ctxs = vec![date_ctx("items[0][starts_at]", Some(""))];

        inject_timezone_values_from_row(&mut ctxs, &field_defs, None);

        let FC::Date(d0) = &ctxs[0] else {
            panic!("expected date")
        };
        assert_eq!(d0.timezone_value.as_deref(), Some(""));
    }

    fn code_field_with_languages(name: &str, langs: Vec<&str>) -> FieldDefinition {
        let admin = FieldAdmin {
            languages: langs.into_iter().map(str::to_string).collect(),
            ..Default::default()
        };
        FieldDefinition {
            name: name.to_string(),
            field_type: FieldType::Code,
            admin,
            ..Default::default()
        }
    }

    #[test]
    fn inject_lang_values_from_row_sets_language_and_languages() {
        let field_defs = vec![
            code_field_with_languages("snippet", vec!["javascript", "python"]),
            code_field_with_languages("notes", vec![]), // no languages → no picker, untouched
        ];

        let mut ctxs = vec![
            code_ctx("items[0][snippet]", "javascript"),
            code_ctx("items[0][notes]", "json"),
        ];

        let row: Map<String, Value> = serde_json::from_value(json!({
            "snippet": "print(1)",
            "snippet_lang": "python",
            "notes": "{}",
        }))
        .unwrap();

        inject_lang_values_from_row(&mut ctxs, &field_defs, Some(&row));

        // First field: picker enabled → languages emitted, per-row pick wins.
        let FC::Code(c0) = &ctxs[0] else {
            panic!("expected code")
        };
        assert_eq!(c0.language, "python");
        assert_eq!(
            c0.languages.as_deref(),
            Some(&["javascript".to_string(), "python".to_string()][..])
        );

        // Second field: no allow-list → context unchanged.
        let FC::Code(c1) = &ctxs[1] else {
            panic!("expected code")
        };
        assert_eq!(c1.language, "json");
        assert!(c1.languages.is_none());
    }

    #[test]
    fn inject_lang_values_from_row_keeps_default_when_lang_value_missing() {
        let field_defs = vec![code_field_with_languages(
            "snippet",
            vec!["javascript", "python"],
        )];
        let mut ctxs = vec![code_ctx("items[0][snippet]", "javascript")];

        // Row exists but `_lang` key is absent — keep the operator default but
        // still emit `languages` so the picker renders.
        let row: Map<String, Value> =
            serde_json::from_value(json!({"snippet": "console.log(1)"})).unwrap();

        inject_lang_values_from_row(&mut ctxs, &field_defs, Some(&row));

        let FC::Code(c0) = &ctxs[0] else {
            panic!("expected code")
        };
        assert_eq!(c0.language, "javascript");
        assert_eq!(
            c0.languages.as_deref(),
            Some(&["javascript".to_string(), "python".to_string()][..])
        );
    }

    // ── safe_template_id ──────────────────────────────────────────────

    #[test]
    fn safe_template_id_simple_name() {
        assert_eq!(safe_template_id("items"), "items");
    }

    #[test]
    fn safe_template_id_with_brackets() {
        assert_eq!(safe_template_id("content[0][items]"), "content-0-items");
    }

    #[test]
    fn safe_template_id_nested_index_placeholder() {
        assert_eq!(
            safe_template_id("content[__INDEX__][items]"),
            "content-__INDEX__-items"
        );
    }

    // ── split_sidebar_fields ──────────────────────────────────────────

    use crate::admin::handlers::field_context::test_helpers::fields_from_json;

    #[test]
    fn split_sidebar_fields_separates_by_position() {
        let fields = fields_from_json(vec![
            json!({"name": "title", "field_type": "text"}),
            json!({"name": "slug", "field_type": "text", "position": "sidebar"}),
            json!({"name": "body", "field_type": "richtext"}),
            json!({"name": "status", "field_type": "select", "position": "sidebar"}),
        ]);
        let (main, sidebar) = split_sidebar_fields(fields);
        assert_eq!(main.len(), 2);
        assert_eq!(sidebar.len(), 2);
        assert_eq!(main[0].base().name, "title");
        assert_eq!(main[1].base().name, "body");
        assert_eq!(sidebar[0].base().name, "slug");
        assert_eq!(sidebar[1].base().name, "status");
    }

    #[test]
    fn split_sidebar_fields_no_sidebar() {
        let fields = fields_from_json(vec![
            json!({"name": "title", "field_type": "text"}),
            json!({"name": "body", "field_type": "richtext"}),
        ]);
        let (main, sidebar) = split_sidebar_fields(fields);
        assert_eq!(main.len(), 2);
        assert!(sidebar.is_empty());
    }

    #[test]
    fn split_sidebar_fields_all_sidebar() {
        let fields = fields_from_json(vec![
            json!({"name": "a", "field_type": "text", "position": "sidebar"}),
            json!({"name": "b", "field_type": "text", "position": "sidebar"}),
        ]);
        let (main, sidebar) = split_sidebar_fields(fields);
        assert!(main.is_empty());
        assert_eq!(sidebar.len(), 2);
    }

    #[test]
    fn split_sidebar_fields_empty() {
        let (main, sidebar) = split_sidebar_fields(vec![]);
        assert!(main.is_empty());
        assert!(sidebar.is_empty());
    }

    // ── count_errors_in_field_contexts ────────────────────────────────

    #[test]
    fn count_errors_empty_fields() {
        assert_eq!(count_errors_in_field_contexts(&[]), 0);
    }

    #[test]
    fn count_errors_no_errors() {
        let fields = fields_from_json(vec![
            json!({"field_type": "text", "name": "title", "value": "hello"}),
            json!({"field_type": "text", "name": "body", "value": "world"}),
        ]);
        assert_eq!(count_errors_in_field_contexts(&fields), 0);
    }

    #[test]
    fn count_errors_direct_errors() {
        let fields = fields_from_json(vec![
            json!({"field_type": "text", "name": "title", "error": "Required"}),
            json!({"field_type": "text", "name": "body", "value": "ok"}),
            json!({"field_type": "text", "name": "email", "error": "Invalid email"}),
        ]);
        assert_eq!(count_errors_in_field_contexts(&fields), 2);
    }

    #[test]
    fn count_errors_nested_in_sub_fields() {
        let fields = fields_from_json(vec![json!({
            "field_type": "group",
            "name": "group1",
            "sub_fields": [
                {"field_type": "text", "name": "nested1", "error": "Too short"},
                {"field_type": "text", "name": "nested2", "value": "ok"},
            ]
        })]);
        assert_eq!(count_errors_in_field_contexts(&fields), 1);
    }

    #[test]
    fn count_errors_nested_in_tabs() {
        let fields = fields_from_json(vec![json!({
            "field_type": "tabs",
            "name": "settings",
            "tabs": [
                {
                    "label": "General",
                    "sub_fields": [
                        {"field_type": "text", "name": "f1", "error": "Required"},
                        {"field_type": "text", "name": "f2", "error": "Too long"},
                    ]
                },
                {
                    "label": "Advanced",
                    "sub_fields": [
                        {"field_type": "text", "name": "f3", "value": "ok"},
                    ]
                }
            ]
        })]);
        assert_eq!(count_errors_in_field_contexts(&fields), 2);
    }

    #[test]
    fn count_errors_nested_in_array_rows() {
        let fields = fields_from_json(vec![json!({
            "field_type": "array",
            "name": "items",
            "rows": [
                {
                    "index": 0,
                    "sub_fields": [
                        {"field_type": "text", "name": "items[0][title]", "error": "Required"},
                    ]
                },
                {
                    "index": 1,
                    "sub_fields": [
                        {"field_type": "text", "name": "items[1][title]", "value": "ok"},
                    ]
                }
            ]
        })]);
        assert_eq!(count_errors_in_field_contexts(&fields), 1);
    }

    #[test]
    fn count_errors_null_error_not_counted() {
        let fields = fields_from_json(vec![
            json!({"field_type": "text", "name": "title", "error": null}),
        ]);
        assert_eq!(count_errors_in_field_contexts(&fields), 0);
    }

    // ── collect_node_attr_errors ──────────────────────────────────────

    #[test]
    fn collect_node_attr_errors_finds_matching() {
        let mut errors = HashMap::new();
        errors.insert(
            "content[cta#0].text".to_string(),
            "Text is required".to_string(),
        );
        errors.insert(
            "content[cta#0].url".to_string(),
            "URL is required".to_string(),
        );

        let result = collect_node_attr_errors(&errors, "content");
        assert!(result.is_some());
        let msg = result.unwrap();
        assert!(msg.contains("Text is required"));
        assert!(msg.contains("URL is required"));
    }

    #[test]
    fn collect_node_attr_errors_ignores_unrelated() {
        let mut errors = HashMap::new();
        errors.insert(
            "other_field[cta#0].text".to_string(),
            "Text is required".to_string(),
        );
        errors.insert("content".to_string(), "Field error".to_string());

        let result = collect_node_attr_errors(&errors, "content");
        assert!(result.is_none());
    }

    #[test]
    fn collect_node_attr_errors_empty() {
        let errors = HashMap::new();
        let result = collect_node_attr_errors(&errors, "content");
        assert!(result.is_none());
    }

    /// A row date stored as UTC is shown as local time in its row's zone, so
    /// saving the form unchanged keeps the same instant.
    #[test]
    fn inject_timezone_values_from_row_shows_local_time() {
        let field_defs = vec![date_field_with_tz("starts_at")];
        let mut ctxs = vec![FC::Date(DateField {
            base: BaseFieldData {
                name: "items[0][starts_at]".to_string(),
                ..Default::default()
            },
            picker_appearance: "dayAndTime".to_string(),
            datetime_local_value: Some("2026-01-15T14:00".to_string()),
            ..Default::default()
        })];

        let row: Map<String, Value> = serde_json::from_value(json!({
            "starts_at": "2026-01-15T14:00:00.000Z",
            "starts_at_tz": "Asia/Tokyo",
        }))
        .unwrap();

        inject_timezone_values_from_row(&mut ctxs, &field_defs, Some(&row));

        let FC::Date(d) = &ctxs[0] else {
            panic!("expected date")
        };
        assert_eq!(d.datetime_local_value.as_deref(), Some("2026-01-15T23:00"));
    }

    fn picker(appearance: &str, date_only: Option<&str>, datetime: Option<&str>) -> DateField {
        DateField {
            picker_appearance: appearance.to_string(),
            date_only_value: date_only.map(str::to_string),
            datetime_local_value: datetime.map(str::to_string),
            ..Default::default()
        }
    }

    /// A value that is not a stored UTC instant — a local date re-rendered from
    /// a submitted form — is left as typed, in both picker appearances.
    #[test]
    fn localize_date_display_leaves_an_unparseable_value_as_typed() {
        let mut day = picker("dayOnly", Some("2026-01-15"), None);
        localize_date_display(&mut day, "2026-01-15", "Asia/Tokyo");
        assert_eq!(day.date_only_value.as_deref(), Some("2026-01-15"));
        assert_eq!(day.datetime_local_value, None);

        let mut day_time = picker("dayAndTime", None, Some("2026-01-15T14:00"));
        localize_date_display(&mut day_time, "2026-01-15T14:00", "Asia/Tokyo");
        assert_eq!(
            day_time.datetime_local_value.as_deref(),
            Some("2026-01-15T14:00")
        );
        assert_eq!(day_time.date_only_value, None);
    }

    /// Regression: a `dayAndTime` value stored with seconds was shown cut to
    /// the minute, so a save with nothing changed dropped the seconds. The
    /// picker shows the seconds — with the `step` that makes the browser keep
    /// them — when they are not zero, and a `timeOnly` value keeps whatever
    /// seconds it has; a value without seconds shows as before.
    #[test]
    fn a_stored_value_with_seconds_is_shown_with_them() {
        assert_eq!(
            date_picker_values("2026-01-15T14:30:45.000Z", "", "dayAndTime"),
            (None, Some("2026-01-15T14:30:45".to_string()))
        );
        assert_eq!(
            picker_step("2026-01-15T14:30:45.000Z", "", "dayAndTime").as_deref(),
            Some("1")
        );
        assert_eq!(
            date_picker_values("2026-01-15T14:30:00.000Z", "", "dayAndTime"),
            (None, Some("2026-01-15T14:30".to_string()))
        );
        assert_eq!(
            picker_step("2026-01-15T14:30:00.000Z", "", "dayAndTime"),
            None
        );

        // In a zone: Tokyo is UTC+9, the seconds survive the conversion.
        assert_eq!(
            date_picker_values("2026-01-15T14:30:45.000Z", "Asia/Tokyo", "dayAndTime"),
            (None, Some("2026-01-15T23:30:45".to_string()))
        );
        assert_eq!(
            picker_step("2026-01-15T14:30:45.000Z", "Asia/Tokyo", "dayAndTime").as_deref(),
            Some("1")
        );

        assert_eq!(
            picker_step("14:30:15", "", "timeOnly").as_deref(),
            Some("1")
        );
        assert_eq!(
            picker_step("14:30:00", "", "timeOnly").as_deref(),
            Some("1")
        );
        assert_eq!(picker_step("14:30", "", "timeOnly"), None);
        assert_eq!(picker_step("2026-01", "", "monthOnly"), None);
        assert_eq!(picker_step("2026-01-15T14:30:45.000Z", "", "dayOnly"), None);

        let mut df = picker("timeOnly", None, None);
        set_date_picker_values(&mut df, "14:30:15", "");
        assert_eq!(df.step.as_deref(), Some("1"));

        let mut df = picker("dayAndTime", None, None);
        set_date_picker_values(&mut df, "2026-01-15T14:30:45.000Z", "");
        assert_eq!(
            df.datetime_local_value.as_deref(),
            Some("2026-01-15T14:30:45")
        );
        assert_eq!(df.step.as_deref(), Some("1"));
    }

    /// The JSON textarea shows a stored object or list pretty-printed and any
    /// other text as it is; the pretty text parses back to the same value.
    #[test]
    fn json_textarea_shows_json_pretty_printed() {
        let pretty = json_textarea_value(r#"{"n":[1,2]}"#);
        assert_eq!(pretty, "{\n  \"n\": [\n    1,\n    2\n  ]\n}");
        assert_eq!(
            serde_json::from_str::<Value>(&pretty).unwrap(),
            json!({ "n": [1, 2] })
        );

        assert_eq!(json_textarea_value("not json"), "not json");
        assert_eq!(json_textarea_value("42"), "42");
        assert_eq!(json_textarea_value(""), "");
    }

    /// A stored UTC instant shows in its zone, cut to the picker's appearance;
    /// an empty value or zone leaves the field untouched.
    #[test]
    fn localize_date_display_converts_a_stored_instant() {
        let mut day = picker("dayOnly", Some("2026-01-15"), None);
        localize_date_display(&mut day, "2026-01-15T20:00:00.000Z", "Asia/Tokyo");
        assert_eq!(day.date_only_value.as_deref(), Some("2026-01-16"));

        let mut no_zone = picker("dayOnly", Some("2026-01-15"), None);
        localize_date_display(&mut no_zone, "2026-01-15T20:00:00.000Z", "");
        assert_eq!(no_zone.date_only_value.as_deref(), Some("2026-01-15"));

        let mut empty = picker("dayAndTime", None, Some("2026-01-15T14:00"));
        localize_date_display(&mut empty, "", "Asia/Tokyo");
        assert_eq!(
            empty.datetime_local_value.as_deref(),
            Some("2026-01-15T14:00")
        );
    }
}
