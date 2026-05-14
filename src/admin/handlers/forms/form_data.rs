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
    core::{DocumentFields, FieldDefinition},
    service::values_from_strings,
};

use super::{join_data::extract_join_data_from_form, select_has_many::transform_select_has_many};

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
    use crate::core::FieldType;
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
