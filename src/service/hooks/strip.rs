//! The data-aware field-**read** strip, shared by the read and the write hook
//! traits.
//!
//! A read and a write both hand their caller a document the caller may not see
//! in full, and both must remove the read-denied fields from it the same way.
//! The strip lives here once — required [`strip_read_access_map`], provided
//! document and batch forms on top of it — and
//! [`ReadHooks`](crate::service::hooks::ReadHooks) /
//! [`WriteHooks`](crate::service::hooks::WriteHooks) inherit it, so the two
//! surfaces cannot drift apart on an access-control path.

use serde_json::{Map, Value};

use crate::{
    core::{Builder, Document, DocumentFields, FieldDefinition},
    hooks::lifecycle::access::has_any_field_access,
};

/// Who and what a field-read strip judges access for: the schema of the
/// documents being stripped, the collection they belong to, the reader, and the
/// locale the rules are evaluated in.
#[derive(Builder)]
pub struct ReadStripArgs<'a> {
    #[builder(required)]
    pub fields: &'a [FieldDefinition],
    #[builder(required)]
    pub collection: &'a str,
    pub user: Option<&'a Document>,
    pub locale: Option<&'a str>,
}

/// The data-aware field-read strip of one hook surface.
pub trait FieldReadStrip {
    /// Remove read-denied fields from `level` in place, evaluating each
    /// `access.read` rule with `ctx.data` = the field's own immediate level (the
    /// row, for fields inside an array/blocks row) and `ctx.document` =
    /// `document` (the full document). The universal `Map` form covers
    /// documents, version snapshots, populated targets, and live events.
    ///
    /// Default no-op so the many lightweight test/override hook impls that
    /// don't enforce field access keep their behavior; the real surfaces
    /// (`RunnerReadHooks`, `LuaReadHooks`, `RunnerWriteHooks`, `LuaWriteHooks`)
    /// override it.
    fn strip_read_access_map(
        &self,
        fields: &[FieldDefinition],
        level: &mut Map<String, Value>,
        document: &DocumentFields,
        collection: &str,
        user: Option<&Document>,
        locale: Option<&str>,
    ) {
        let _ = (fields, level, document, collection, user, locale);
    }

    /// Convenience over [`strip_read_access_map`](Self::strip_read_access_map):
    /// strip read-denied fields from a [`Document`] in place, capturing the full
    /// pre-strip document as `ctx.document`.
    fn strip_read_access_doc(
        &self,
        fields: &[FieldDefinition],
        doc: &mut Document,
        collection: &str,
        user: Option<&Document>,
        locale: Option<&str>,
    ) {
        // Skip the per-document clone + map round-trip entirely when no field
        // configures read access (the common case) — the read hot path pays nothing.
        if !has_any_field_access(fields, |f| f.access.read.as_ref()) {
            return;
        }

        let document = doc.fields.clone();
        let mut level: Map<String, Value> = std::mem::take(&mut doc.fields)
            .into_inner()
            .into_iter()
            .collect();

        self.strip_read_access_map(fields, &mut level, &document, collection, user, locale);

        doc.fields = level.into_iter().collect();
    }

    /// Batched form of [`strip_read_access_doc`](Self::strip_read_access_doc)
    /// for a list read. The default loops per document; `RunnerReadHooks`
    /// overrides it to acquire the Lua VM **once** for the whole batch (the
    /// per-query perf model) instead of once per document. Each document is
    /// still stripped against its own `ctx.document` / per-row `ctx.data`.
    fn strip_read_access_docs(
        &self,
        fields: &[FieldDefinition],
        docs: &mut [Document],
        collection: &str,
        user: Option<&Document>,
        locale: Option<&str>,
    ) {
        for doc in docs.iter_mut() {
            self.strip_read_access_doc(fields, doc, collection, user, locale);
        }
    }
}
