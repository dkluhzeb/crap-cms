//! `bench` command — benchmark hooks, queries, and write cycles.

mod create;
mod dispatch;
mod helpers;
mod hooks;
mod queries;

pub use dispatch::run;
