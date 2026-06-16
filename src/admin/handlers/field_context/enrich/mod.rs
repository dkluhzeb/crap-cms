//! DB-access enrichment for field contexts (relationship options, array rows, upload thumbnails).

mod children;
mod ctx;
mod enrichment;
mod field_types;
mod gated;
mod nested;
mod options;
mod sub_field_opts;
mod types;

pub use enrichment::{enrich_field_contexts, enrich_polymorphic_selected};
pub use nested::{build_enriched_sub_field_context, enrich_nested_fields};
pub use options::EnrichOptions;
pub use sub_field_opts::SubFieldOpts;

pub(super) use ctx::EnrichCtx;
pub(in crate::admin::handlers::field_context) use gated::{gated_find, gated_find_by_id};

#[cfg(all(test, feature = "sqlite"))]
mod test_helpers;
