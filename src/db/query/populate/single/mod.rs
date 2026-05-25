//! Single-document relationship population (recursive, cached).

mod dispatch;
mod join;
pub(crate) mod nested;
mod nonpoly;
mod poly;
#[cfg(all(test, feature = "sqlite"))]
mod tests;

pub(crate) use dispatch::populate_relationships_cached;
pub use dispatch::populate_relationships_cached_with_singleflight;
