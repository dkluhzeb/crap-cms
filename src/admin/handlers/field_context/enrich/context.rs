//! Types and options structs for field context enrichment.

use std::collections::HashMap;

use crate::{
    admin::AdminState,
    core::Registry,
    db::{DbConnection, query::LocaleContext},
};

use super::{EnrichOptionsBuilder, SubFieldOptsBuilder};

/// Bundled parameters for sub-field enrichment functions (`sub_array`, `sub_blocks`,
/// `sub_row_collapsible`, `sub_tabs`, `build_enriched_sub_field_context`) to avoid
/// too many arguments.
pub struct SubFieldOpts<'a> {
    pub locale_locked: bool,
    pub non_default_locale: bool,
    pub depth: usize,
    pub errors: &'a HashMap<String, String>,
}

impl<'a> SubFieldOpts<'a> {
    pub fn builder(errors: &'a HashMap<String, String>) -> SubFieldOptsBuilder<'a> {
        SubFieldOptsBuilder::new(errors)
    }
}

/// Bundled parameters for top-level enrichment functions (`enrich_array`, `enrich_blocks`)
/// that need DB and state access.
pub(in crate::admin::handlers::field_context) struct EnrichCtx<'a> {
    pub state: &'a AdminState,
    pub non_default_locale: bool,
    pub errors: &'a HashMap<String, String>,
    pub conn: &'a dyn DbConnection,
    pub reg: &'a Registry,
    pub rel_locale_ctx: Option<&'a LocaleContext>,
}

/// Bundled parameters for [`enrich_field_contexts`] to avoid too many arguments.
pub struct EnrichOptions<'a> {
    pub filter_hidden: bool,
    pub non_default_locale: bool,
    pub errors: &'a HashMap<String, String>,
    pub doc_id: Option<&'a str>,
}

impl<'a> EnrichOptions<'a> {
    pub fn builder(errors: &'a HashMap<String, String>) -> EnrichOptionsBuilder<'a> {
        EnrichOptionsBuilder::new(errors)
    }
}
