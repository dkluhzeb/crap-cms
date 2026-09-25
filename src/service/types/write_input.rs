//! Input data for write operations (create/update).

use std::collections::HashMap;

use serde_json::Value;

use crate::{
    core::{DocumentFields, upload::QueuedConversion},
    db::LocaleContext,
};

/// The image conversions the file a write stored still needs, carried into the
/// write so the job rows are inserted inside the write transaction.
///
/// Queueing them afterwards on a second connection is not atomic with the
/// document: a crash between the commit and the insert leaves the row's
/// `{size}_{fmt}_url` columns unfilled forever, with nothing left to retry
/// them.
///
/// `Some` with an empty `queued` still means *this write stored a file* — a
/// format-less upload replaces the previous file just the same, so the
/// conversions queued for that one are cancelled either way.
pub struct UploadConversions {
    pub queued: Vec<QueuedConversion>,
    pub max_attempts: u32,
}

impl UploadConversions {
    #[must_use]
    pub fn new(queued: Vec<QueuedConversion>, max_attempts: u32) -> Self {
        Self {
            queued,
            max_attempts,
        }
    }
}

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
    pub draft: bool,
    /// Set only by the upload multipart handlers after they have processed a
    /// real file and injected the server-derived metadata columns. When false
    /// (every other surface: Lua, gRPC, MCP, generic admin), the write
    /// chokepoint strips the derived upload columns from `data` so a caller
    /// can't forge `url`/`*_url`/dimensions. See `CollectionUpload::derived_field_names`.
    pub trusted_upload_metadata: bool,
    /// Set by the upload write when it stored a file: the conversions that file
    /// queued, inserted inside the write transaction. `None` on every write
    /// that stored no file.
    pub upload_conversions: Option<UploadConversions>,
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
    pub(in crate::service) draft: bool,
    pub(in crate::service) trusted_upload_metadata: bool,
    pub(in crate::service) upload_conversions: Option<UploadConversions>,
}

impl<'a> WriteInputBuilder<'a> {
    #[must_use]
    pub fn new(data: DocumentFields) -> Self {
        Self {
            data,
            password: None,
            locale_ctx: None,
            draft: false,
            trusted_upload_metadata: false,
            upload_conversions: None,
        }
    }

    #[must_use]
    pub fn password(mut self, password: Option<&'a str>) -> Self {
        self.password = password;

        self
    }

    #[must_use]
    pub fn locale_ctx(mut self, locale_ctx: Option<&'a LocaleContext>) -> Self {
        self.locale_ctx = locale_ctx;

        self
    }

    #[must_use]
    pub fn draft(mut self, draft: bool) -> Self {
        self.draft = draft;

        self
    }

    /// Mark this write as carrying trusted, server-computed upload metadata
    /// (the multipart upload handlers, after `inject_upload_metadata`). Leaves
    /// the derived upload columns untouched by the write-chokepoint strip.
    #[must_use]
    pub fn trusted_upload_metadata(mut self, trusted: bool) -> Self {
        self.trusted_upload_metadata = trusted;

        self
    }

    /// Carry the conversions the stored file queued into the write transaction.
    /// Setting it (even to an empty list) marks the write as one that stored a
    /// file, which cancels the conversions still queued for the previous one.
    #[must_use]
    pub fn upload_conversions(mut self, conversions: Option<UploadConversions>) -> Self {
        self.upload_conversions = conversions;

        self
    }

    #[must_use]
    pub fn build(self) -> WriteInput<'a> {
        WriteInput {
            data: self.data,
            password: self.password,
            locale_ctx: self.locale_ctx,
            draft: self.draft,
            trusted_upload_metadata: self.trusted_upload_metadata,
            upload_conversions: self.upload_conversions,
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
            .draft(true)
            .build();

        assert_eq!(wi.data.get("title"), Some(&json!("hi")));
        assert_eq!(wi.password, Some("pw"));
        assert!(wi.draft);
        assert!(wi.locale_ctx.is_none());
    }
}
