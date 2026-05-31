//! Input data for write operations (create/update).

use std::collections::HashMap;

use serde_json::Value;

use crate::{core::DocumentFields, db::LocaleContext};

/// Wrap each string value in `Value::String` for the form-input boundary.
///
/// HTML form parsing yields `HashMap<String, String>` because every form
/// field is a string in HTTP. Typed write paths (gRPC, Lua, JSON API)
/// produce `DocumentFields` directly. This helper bridges the form side
/// into the typed pipeline at a single point — the [`WriteInput::builder`]
/// call site — so the rest of the code sees one typed shape end-to-end.
#[must_use]
pub fn values_from_strings(map: HashMap<String, String>) -> DocumentFields {
    map.into_iter()
        .map(|(k, v)| (k, Value::String(v)))
        .collect()
}

/// Bundles the data parameters that callers provide for write operations,
/// reducing argument count on public API functions.
///
/// `data` is a single typed map that carries both scalar field values
/// (which become column writes) and structured field values (arrays /
/// blocks / has-many, which become join-table writes). The dispatch
/// happens internally based on each field's `field_type`.
pub struct WriteInput<'a> {
    pub data: DocumentFields,
    pub password: Option<&'a str>,
    pub locale_ctx: Option<&'a LocaleContext>,
    pub locale: Option<String>,
    pub draft: bool,
    pub ui_locale: Option<String>,
}

impl<'a> WriteInput<'a> {
    /// Create a builder with the required `data` field.
    pub fn builder(data: impl Into<DocumentFields>) -> WriteInputBuilder<'a> {
        WriteInputBuilder::new(data.into())
    }
}

/// Builder for [`WriteInput`]. Created via [`WriteInput::builder`].
pub struct WriteInputBuilder<'a> {
    pub(in crate::service) data: DocumentFields,
    pub(in crate::service) password: Option<&'a str>,
    pub(in crate::service) locale_ctx: Option<&'a LocaleContext>,
    pub(in crate::service) locale: Option<String>,
    pub(in crate::service) draft: bool,
    pub(in crate::service) ui_locale: Option<String>,
}

impl<'a> WriteInputBuilder<'a> {
    pub fn new(data: DocumentFields) -> Self {
        Self {
            data,
            password: None,
            locale_ctx: None,
            locale: None,
            draft: false,
            ui_locale: None,
        }
    }

    pub fn password(mut self, password: Option<&'a str>) -> Self {
        self.password = password;

        self
    }

    pub fn locale_ctx(mut self, locale_ctx: Option<&'a LocaleContext>) -> Self {
        self.locale_ctx = locale_ctx;

        self
    }

    pub fn locale(mut self, locale: Option<String>) -> Self {
        self.locale = locale;

        self
    }

    pub fn draft(mut self, draft: bool) -> Self {
        self.draft = draft;

        self
    }

    pub fn ui_locale(mut self, ui_locale: Option<String>) -> Self {
        self.ui_locale = ui_locale;

        self
    }

    pub fn build(self) -> WriteInput<'a> {
        WriteInput {
            data: self.data,
            password: self.password,
            locale_ctx: self.locale_ctx,
            locale: self.locale,
            draft: self.draft,
            ui_locale: self.ui_locale,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn values_from_strings_wraps_each_value_as_a_json_string() {
        let mut m = HashMap::new();
        m.insert("a".to_string(), "1".to_string());
        m.insert("b".to_string(), "2".to_string());
        let df = values_from_strings(m);
        assert_eq!(df.get("a"), Some(&json!("1")));
        assert_eq!(df.get("b"), Some(&json!("2")));
        assert_eq!(df.len(), 2);
    }

    #[test]
    fn values_from_strings_empty_map_is_empty() {
        assert!(values_from_strings(HashMap::new()).is_empty());
    }

    /// Distinct value per field so a swapped assignment in `build()` shows up.
    #[test]
    fn builder_wires_each_field_to_its_own_slot() {
        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!("hi"));
        let wi = WriteInput::builder(data)
            .password(Some("pw"))
            .locale(Some("de".to_string()))
            .draft(true)
            .ui_locale(Some("en".to_string()))
            .build();

        assert_eq!(wi.data.get("title"), Some(&json!("hi")));
        assert_eq!(wi.password, Some("pw"));
        assert_eq!(wi.locale.as_deref(), Some("de"));
        assert!(wi.draft);
        assert_eq!(wi.ui_locale.as_deref(), Some("en"));
        assert!(wi.locale_ctx.is_none());
    }
}
