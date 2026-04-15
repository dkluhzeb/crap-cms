//! Batch relationship population across multiple documents.

mod dispatch;
mod nonpoly;
mod poly;
#[cfg(test)]
mod tests;

pub use dispatch::{
    populate_relationships_batch_cached, populate_relationships_batch_cached_with_singleflight,
};
