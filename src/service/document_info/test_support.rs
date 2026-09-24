//! Read hooks shared by the document-info tests.

use anyhow::Result;
use serde_json::{Map, Value};

use crate::{
    core::{Document, DocumentFields, FieldDefinition, ReqContext, collection::Hooks},
    db::AccessResult,
    hooks::{
        AccessCheckInput,
        lifecycle::{AfterReadCtx, access::strip_read_access_data_aware},
    },
    service::{FieldReadStrip, ReadHooks},
};

/// Read hooks that allow every access check and strip every field whose
/// read rule is `deny`.
pub(super) struct DenyMarkedFields;

impl ReadHooks for DenyMarkedFields {
    fn before_read(&self, _: &Hooks, _: &str, _: &str, _: Option<&str>) -> Result<ReqContext> {
        Ok(ReqContext::new())
    }

    fn after_read_one(&self, _: &AfterReadCtx, doc: Document) -> Document {
        doc
    }

    fn check_access(&self, _: &AccessCheckInput<'_>) -> Result<AccessResult> {
        Ok(AccessResult::Allowed)
    }
}

impl FieldReadStrip for DenyMarkedFields {
    fn strip_read_access_map(
        &self,
        fields: &[FieldDefinition],
        level: &mut Map<String, Value>,
        _document: &DocumentFields,
        _collection: &str,
        _user: Option<&Document>,
        _locale: Option<&str>,
    ) {
        strip_read_access_data_aware(fields, level, &|hook, _data| hook.reference() == "deny");
    }
}
