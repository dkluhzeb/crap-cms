//! `make component` -- scaffold a custom Web Component file.

mod generator;

pub(crate) use generator::component_path;
pub use generator::{MakeComponentOptions, make_component};
