//! DB-access enrichment for field contexts (relationship options, array rows, upload thumbnails).

mod children;
mod enrich_ctx;
mod enrich_options;
mod enrich_types;
mod enrichment;
mod field_types;
mod nested;
mod sub_field_opts;

pub use enrich_options::EnrichOptions;
pub use enrichment::{enrich_field_contexts, enrich_polymorphic_selected};
pub use nested::{build_enriched_sub_field_context, enrich_nested_fields};
pub use sub_field_opts::SubFieldOpts;

pub(super) use enrich_ctx::EnrichCtx;

#[cfg(all(test, feature = "sqlite"))]
mod test_helpers;
