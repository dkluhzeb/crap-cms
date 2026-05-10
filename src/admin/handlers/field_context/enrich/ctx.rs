//! Internal context bundle threaded through the enrichment helpers.

use std::collections::HashMap;

use crate::{
    admin::AdminState,
    core::Registry,
    db::{DbConnection, query::LocaleContext},
};

/// Bundled parameters for top-level enrichment functions (`enrich_array`,
/// `enrich_blocks`) that need DB and state access. Module-internal: callers
/// outside `field_context/` should hand off via [`EnrichOptions`](super::EnrichOptions).
pub(in crate::admin::handlers::field_context) struct EnrichCtx<'a> {
    pub state: &'a AdminState,
    pub non_default_locale: bool,
    pub errors: &'a HashMap<String, String>,
    pub conn: &'a dyn DbConnection,
    pub reg: &'a Registry,
    pub rel_locale_ctx: Option<&'a LocaleContext>,
}
