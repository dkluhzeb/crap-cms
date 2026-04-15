//! Input for `search_documents` — lightweight relationship field search.

use crate::db::{FindQuery, LocaleContext};

/// Input for [`search_documents`](crate::service::search_documents).
pub struct SearchDocumentsInput<'a> {
    pub query: &'a FindQuery,
    pub locale_ctx: Option<&'a LocaleContext>,
    /// Whether cursor-based pagination is enabled.
    pub cursor_enabled: bool,
    /// When `true`, drafts are included in results. When `false` and the
    /// collection has drafts enabled, the service injects `_status =
    /// "published"` into the query so only published rows are returned —
    /// matching `find_documents`' semantic. Admin callers (e.g. the
    /// relationship picker) typically set this to `true` so operators can
    /// link to work-in-progress content.
    pub include_drafts: bool,
}
