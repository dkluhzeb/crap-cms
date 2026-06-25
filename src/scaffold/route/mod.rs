//! `make route` command -- generate custom HTTP route handler files.

mod generator;

pub use generator::{MakeRouteOptions, make_route};
