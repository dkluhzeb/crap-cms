//! `db cleanup` subcommand: detect and remove orphan columns, stale locale
//! rows and orphan tables.

mod apply;
mod command;
mod display;
mod scan;

#[cfg(test)]
mod test_support;

pub use command::cleanup;
