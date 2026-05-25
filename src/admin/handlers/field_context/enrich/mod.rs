//! DB-access enrichment for field contexts (relationship options, array rows, upload thumbnails).

mod children;
mod ctx;
mod enrichment;
mod field_types;
mod nested;
mod options;
mod sub_field_opts;
mod types;

pub use enrichment::{enrich_field_contexts, enrich_polymorphic_selected};
pub use nested::{build_enriched_sub_field_context, enrich_nested_fields};
pub use options::EnrichOptions;
pub use sub_field_opts::SubFieldOpts;

pub(super) use ctx::EnrichCtx;

#[cfg(all(test, feature = "sqlite"))]
mod test_helpers;
