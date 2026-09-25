//! Batch relationship population across multiple documents.

mod dispatch;
mod refs;

#[cfg(all(test, feature = "sqlite"))]
mod join_tests;
#[cfg(all(test, feature = "sqlite"))]
mod nonpoly_tests;
#[cfg(all(test, feature = "sqlite"))]
mod poly_tests;
#[cfg(all(test, feature = "sqlite"))]
mod tests;

pub(crate) use dispatch::populate_relationships_batch_cached;
pub use dispatch::populate_relationships_batch_cached_with_singleflight;
