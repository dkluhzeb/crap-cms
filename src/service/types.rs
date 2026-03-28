//! Shared type definitions for the service layer write operations.

use std::collections::HashMap;

use serde_json::Value;

use crate::{config::LocaleConfig, core::Document, db::LocaleContext};

use super::{AfterChangeInputBuilder, PersistOptionsBuilder, WriteInputBuilder};

/// Result of a write operation: the document and the request-scoped hook context.
pub type WriteResult = (Document, HashMap<String, Value>);

/// Input data for a write operation (create/update). Bundles the 6 data parameters
/// that callers provide, reducing argument count on public API functions.
pub struct WriteInput<'a> {
    pub data: HashMap<String, String>,
    pub join_data: &'a HashMap<String, Value>,
    pub password: Option<&'a str>,
    pub locale_ctx: Option<&'a LocaleContext>,
    pub locale: Option<String>,
    pub draft: bool,
    pub ui_locale: Option<String>,
}

impl<'a> WriteInput<'a> {
    /// Create a builder with the required data and join_data fields.
    pub fn builder(
        data: HashMap<String, String>,
        join_data: &'a HashMap<String, Value>,
    ) -> WriteInputBuilder<'a> {
        WriteInputBuilder::new(data, join_data)
    }
}

/// Bundled parameters for after-change hook invocation.
pub(crate) struct AfterChangeInput<'a> {
    pub slug: &'a str,
    pub operation: &'a str,
    pub locale: Option<String>,
    pub is_draft: bool,
    pub req_context: HashMap<String, Value>,
    pub user: Option<&'a Document>,
    pub ui_locale: Option<&'a str>,
}

impl<'a> AfterChangeInput<'a> {
    /// Create a builder with the required slug and operation.
    pub fn builder(slug: &'a str, operation: &'a str) -> AfterChangeInputBuilder<'a> {
        AfterChangeInputBuilder::new(slug, operation)
    }
}

/// Optional parameters for the persist_create operation.
#[derive(Default)]
pub struct PersistOptions<'a> {
    pub password: Option<&'a str>,
    pub locale_ctx: Option<&'a LocaleContext>,
    pub locale_config: Option<&'a LocaleConfig>,
    pub is_draft: bool,
}

impl<'a> PersistOptions<'a> {
    /// Create a builder with all fields defaulted.
    pub fn builder() -> PersistOptionsBuilder<'a> {
        PersistOptionsBuilder::new()
    }
}
