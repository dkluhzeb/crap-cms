//! DB-access enrichment for field contexts (relationship options, array rows, upload thumbnails).

mod children;
mod context;
mod enrich_options_builder;
mod enrich_types;
mod enrichment;
mod field_types;
mod nested;
mod sub_field_opts_builder;

pub use context::{EnrichOptions, SubFieldOpts};
pub use enrich_options_builder::EnrichOptionsBuilder;
pub use enrichment::{enrich_field_contexts, enrich_polymorphic_selected};
pub use nested::{build_enriched_sub_field_context, enrich_nested_fields};
pub use sub_field_opts_builder::SubFieldOptsBuilder;

pub(super) use context::EnrichCtx;

#[cfg(test)]
mod tests;
