//! Internal context bundle threaded through the enrichment helpers.

use std::collections::HashMap;

use crate::{
    admin::AdminState,
    core::{Document, Registry},
    db::{DbConnection, query::LocaleContext},
};

/// Bundled parameters for top-level enrichment functions (`enrich_array`,
/// `enrich_blocks`) that need DB and state access. Module-internal: callers
/// outside `field_context/` should hand off via [`EnrichOptions`](super::EnrichOptions).
#[derive(Clone, Copy)]
pub(in crate::admin::handlers::field_context) struct EnrichCtx<'a> {
    pub state: &'a AdminState,
    pub non_default_locale: bool,
    pub errors: &'a HashMap<String, String>,
    pub conn: &'a dyn DbConnection,
    pub reg: &'a Registry,
    pub rel_locale_ctx: Option<&'a LocaleContext>,
    /// The viewer, so relationship/join/upload label reads are access-gated.
    pub user: Option<&'a Document>,
    /// The document being edited — a join field lists the documents that
    /// reference it, wherever the join sits. `None` on a create form.
    pub doc_id: Option<&'a str>,
    /// Whether a container enclosing the field being enriched declares
    /// `admin.readonly`. It cascades downward; narrowed per level when the
    /// walk descends into a container. A container locked only by the locale
    /// does not set it — the locale lock is recomputed per field from
    /// `non_default_locale`.
    pub ancestor_readonly: bool,
}
