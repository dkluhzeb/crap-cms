//! Single-document relationship population (recursive, cached).

mod dispatch;
mod join;
pub(crate) mod nested;
mod nonpoly;
mod poly;
#[cfg(test)]
mod tests;

pub use dispatch::{
    populate_relationships_cached, populate_relationships_cached_with_singleflight,
};
