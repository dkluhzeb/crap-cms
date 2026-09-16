//! Bundle of HTML form input — raw scalar map + extracted typed join data.
//!
//! HTML forms hit the admin layer as `HashMap<String, String>` (URL-encoded
//! key/value pairs, including bracketed array-row keys like
//! `slides[0][title]`). The typed write pipeline ([`DocumentFields`]) wants a
//! single merged map where array / blocks / has-many fields appear as typed
//! JSON values. `FormData` carries both halves so:
//!
//! - Error-rendering paths can re-render with the original strings.
//! - Success paths can convert to [`DocumentFields`] via the [`From`] impl.

use std::collections::HashMap;

use crate::{
    admin::handlers::shared::for_each_admin_form_leaf,
    core::{CollectionDefinition, DocumentFields, FieldDefinition, FieldType},
    service::values_from_strings,
};

use super::{join_data::extract_join_data_from_form, select_has_many::transform_select_has_many};

/// Give every checkbox the form rendered an explicit value. An HTML checkbox
/// submits nothing when unchecked, so an absent *rendered* checkbox means
/// "unchecked" — make that explicit as `"0"` so the write path stores false
/// rather than falling back to the field's `default_value`. That default is for
/// API creates (Lua / gRPC / MCP) that genuinely omit the field; a form always
/// reflects the box's shown state (the new-item form pre-checks a `true`
/// default, so leaving it checked submits `"on"`).
///
/// A checkbox the form never rendered (`admin.hidden`, or one inside a hidden
/// group) is left absent: its value is kept, which is what hiding it promises.
/// The walk is [`for_each_admin_form_leaf`], the same answer the form builder
/// used to decide what to render, so the two cannot disagree.
fn normalize_absent_checkboxes(raw: &mut HashMap<String, String>, fields: &[FieldDefinition]) {
    for_each_admin_form_leaf(fields, |field, column| {
        if field.field_type == FieldType::Checkbox {
            raw.entry(column).or_insert_with(|| "0".to_string());
        }
    });
}

/// Form-submission state: raw `HashMap` plus extracted join (array/blocks/relationship) data.
#[derive(Debug, Clone, Default)]
pub struct FormData {
    raw: HashMap<String, String>,
    join: DocumentFields,
}

impl FormData {
    /// Construct from a raw form `HashMap`.
    ///
    /// Runs [`transform_select_has_many`] in-place (consolidating multi-select
    /// inputs against the field schema) and extracts typed join data
    /// (arrays, blocks, has-many relationships) into a separate
    /// [`DocumentFields`]. Meta keys that are not part of the document
    /// (`_action`, `_locale`, `password`, …) are left in `raw` for the
    /// caller to extract via [`take`](Self::take) / [`take_action`](Self::take_action) etc.
    pub fn from_raw(mut raw: HashMap<String, String>, fields: &[FieldDefinition]) -> Self {
        transform_select_has_many(&mut raw, fields);
        normalize_absent_checkboxes(&mut raw, fields);
        let join = extract_join_data_from_form(&raw, fields);

        Self { raw, join }
    }

    /// Construct from a raw form `HashMap` with no transform or extraction.
    ///
    /// Use when the field definitions aren't in scope or join extraction
    /// isn't relevant (e.g. upload-error re-render paths).
    pub fn raw_only(raw: HashMap<String, String>) -> Self {
        Self {
            raw,
            join: DocumentFields::new(),
        }
    }

    /// Borrow the raw form map.
    pub fn raw(&self) -> &HashMap<String, String> {
        &self.raw
    }

    /// Mutably borrow the raw form map.
    ///
    /// For callers that need to inject scalar fields after construction
    /// (e.g. upload-metadata injection). Mutations to structural fields
    /// (arrays/blocks/has-many) won't propagate into the extracted join
    /// data — those should happen before `from_raw`.
    pub fn raw_mut(&mut self) -> &mut HashMap<String, String> {
        &mut self.raw
    }

    /// Borrow the extracted join data.
    pub fn join(&self) -> &DocumentFields {
        &self.join
    }

    /// Produce merged [`DocumentFields`] by cloning both halves.
    ///
    /// Use this when the `FormData` must survive (e.g. for error rendering);
    /// otherwise prefer `into()` to consume.
    pub fn to_doc_fields(&self) -> DocumentFields {
        self.clone().into()
    }

    /// Remove and return a meta key from the raw form.
    ///
    /// Generic accessor for any non-data meta key. Admin handlers use it
    /// for `password`, `_locked`, and other situational keys; the well-known
    /// `_action` and `_locale` keys have their own helpers below.
    pub fn take(&mut self, key: &str) -> Option<String> {
        self.raw.remove(key)
    }

    /// Borrow a key from the raw form (non-removing read).
    pub fn get(&self, key: &str) -> Option<&str> {
        self.raw.get(key).map(String::as_str)
    }

    /// Remove and return the `_action` meta key, or `""` if absent.
    ///
    /// Universal across admin write handlers and `service/upload`.
    pub fn take_action(&mut self) -> String {
        self.take("_action").unwrap_or_default()
    }

    /// Remove and return the `_locale` meta key.
    pub fn take_locale(&mut self) -> Option<String> {
        self.take("_locale")
    }

    /// Remove and return the `password` meta key — auth collections only.
    ///
    /// Every write surface extracts it the same way: a collection without auth
    /// has no password to set, and `password` must never reach the write as
    /// document data.
    pub fn take_password(&mut self, def: &CollectionDefinition) -> Option<String> {
        def.is_auth_collection()
            .then(|| self.take("password"))
            .flatten()
    }
}

impl From<FormData> for DocumentFields {
    fn from(f: FormData) -> Self {
        let mut out = values_from_strings(f.raw);
        out.extend(f.join);

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{FieldAdmin, FieldType};
    use serde_json::{Value, json};

    fn make_field(name: &str, ft: FieldType) -> FieldDefinition {
        FieldDefinition::builder(name, ft).build()
    }

    #[test]
    fn from_raw_extracts_join_data() {
        let mut raw = HashMap::new();
        raw.insert("title".into(), "Hello".into());
        raw.insert("items[0][name]".into(), "first".into());

        let mut arr = make_field("items", FieldType::Array);
        arr.fields = vec![make_field("name", FieldType::Text)];

        let form = FormData::from_raw(raw, &[make_field("title", FieldType::Text), arr]);

        assert_eq!(form.raw().get("title"), Some(&"Hello".to_string()));
        assert!(form.join().get("items").is_some());
    }

    /// An absent top-level checkbox is normalized to an explicit "0" (HTML omits
    /// an unchecked box), so the write path stores false rather than the field's
    /// `default_value`; a present checkbox value is left untouched. Group-nested
    /// checkboxes get their prefixed column normalized too.
    #[test]
    fn from_raw_normalizes_absent_checkboxes() {
        let mut group = make_field("meta", FieldType::Group);
        group.fields = vec![make_field("flag", FieldType::Checkbox)];
        let fields = vec![
            make_field("featured", FieldType::Checkbox),
            make_field("archived", FieldType::Checkbox),
            group,
        ];

        // `featured` is checked (present); `archived` and `meta.flag` are absent.
        let mut raw = HashMap::new();
        raw.insert("featured".into(), "on".into());

        let form = FormData::from_raw(raw, &fields);

        assert_eq!(
            form.raw().get("featured"),
            Some(&"on".to_string()),
            "present value untouched"
        );
        assert_eq!(
            form.raw().get("archived"),
            Some(&"0".to_string()),
            "absent checkbox -> 0"
        );
        assert_eq!(
            form.raw().get("meta__flag"),
            Some(&"0".to_string()),
            "absent nested checkbox -> 0"
        );
    }

    /// Regression: a checkbox the form never rendered must stay absent, so the
    /// write keeps its stored value. Normalizing every checkbox — rendered or
    /// not — turned an `admin.hidden` `true` into `false` on every save, and
    /// forced a hidden `default_value = true` to false on create.
    #[test]
    fn from_raw_leaves_checkboxes_the_form_never_rendered_absent() {
        let hidden_box = FieldDefinition::builder("internal", FieldType::Checkbox)
            .admin(FieldAdmin::builder().hidden(true).build())
            .build();
        let hidden_group = FieldDefinition::builder("system", FieldType::Group)
            .admin(FieldAdmin::builder().hidden(true).build())
            .fields(vec![make_field("flag", FieldType::Checkbox)])
            .build();
        let fields = vec![
            make_field("featured", FieldType::Checkbox),
            hidden_box,
            hidden_group,
        ];

        let form = FormData::from_raw(HashMap::new(), &fields);

        assert_eq!(
            form.raw().get("featured"),
            Some(&"0".to_string()),
            "a rendered absent checkbox is still an explicit uncheck"
        );
        assert_eq!(
            form.raw().get("internal"),
            None,
            "a hidden checkbox keeps its stored value"
        );
        assert_eq!(
            form.raw().get("system__flag"),
            None,
            "a checkbox inside a hidden group is never rendered either"
        );
    }

    /// The two halves of the group case, in one submission: the form renders an
    /// input for a visible checkbox inside a group but none for a hidden one, so
    /// a group that submits nothing at all still means "the visible box was
    /// unchecked" (`meta__flag` -> `"0"`) while the hidden box keeps whatever the
    /// document holds (`meta__internal` absent). Rendering the hidden box would
    /// let the editor uncheck it and have the change silently dropped.
    #[test]
    fn a_hidden_checkbox_inside_a_visible_group_is_the_only_one_left_absent() {
        let mut group = make_field("meta", FieldType::Group);
        group.fields = vec![
            make_field("flag", FieldType::Checkbox),
            FieldDefinition::builder("internal", FieldType::Checkbox)
                .admin(FieldAdmin::builder().hidden(true).build())
                .build(),
        ];

        let form = FormData::from_raw(HashMap::new(), &[group]);

        assert_eq!(
            form.raw().get("meta__flag"),
            Some(&"0".to_string()),
            "a rendered checkbox in a group is an explicit uncheck when absent"
        );
        assert_eq!(
            form.raw().get("meta__internal"),
            None,
            "a hidden checkbox in a visible group keeps its stored value"
        );
    }

    /// Regression: the same rule for a `has_many` scalar — the form renders no
    /// input for a hidden one, so it must not be normalized to an empty list.
    #[test]
    fn from_raw_leaves_has_many_the_form_never_rendered_absent() {
        let mut visible = make_field("tags", FieldType::Text);
        visible.has_many = true;

        let mut hidden = FieldDefinition::builder("internal_tags", FieldType::Text)
            .admin(FieldAdmin::builder().hidden(true).build())
            .build();
        hidden.has_many = true;

        let form = FormData::from_raw(HashMap::new(), &[visible, hidden]);

        assert_eq!(
            form.raw().get("tags"),
            Some(&"[]".to_string()),
            "a rendered has-many with nothing selected is an explicit empty list"
        );
        assert_eq!(
            form.raw().get("internal_tags"),
            None,
            "a hidden has-many list survives the save"
        );
    }

    #[test]
    fn into_doc_fields_merges_both_halves() {
        let mut raw = HashMap::new();
        raw.insert("title".into(), "Hello".into());

        let mut form = FormData::raw_only(raw);
        form.join = DocumentFields::from_iter([("tags".to_string(), json!(["a", "b"]))]);

        let data: DocumentFields = form.into();

        assert_eq!(data.get("title"), Some(&Value::String("Hello".into())));
        assert_eq!(data.get("tags"), Some(&json!(["a", "b"])));
    }

    #[test]
    fn to_doc_fields_preserves_original() {
        let mut raw = HashMap::new();
        raw.insert("a".into(), "1".into());

        let form = FormData::raw_only(raw);
        let _merged = form.to_doc_fields();

        // Original still usable after to_doc_fields.
        assert_eq!(form.raw().get("a"), Some(&"1".to_string()));
    }
}
