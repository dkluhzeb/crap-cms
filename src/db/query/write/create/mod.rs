//! Create operation and its helpers.

mod collector;
mod insert;
mod leaf;

#[cfg(test)]
mod companion_tests;
#[cfg(test)]
mod test_support;

pub use insert::create;
