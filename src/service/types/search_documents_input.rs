//! Input for `search_documents` — lightweight relationship field search.

use crate::db::{FindQuery, LocaleContext};

/// Input for [`search_documents`](crate::service::search_documents).
pub struct SearchDocumentsInput<'a> {
    pub query: &'a FindQuery,
    pub locale_ctx: Option<&'a LocaleContext>,
    /// Whether cursor-based pagination is enabled.
    pub cursor_enabled: bool,
}
