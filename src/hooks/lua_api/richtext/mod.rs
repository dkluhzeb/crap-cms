//! Registers `crap.richtext` — custom `ProseMirror` node registration and rendering.

mod api;
mod render;
mod spec;
#[cfg(test)]
mod test_support;

pub use api::{register_richtext_init, register_richtext_pool_init};
pub(crate) use api::{render_crap_richtext_init_lua, render_crap_richtext_render_lua};
pub(crate) use render::RichtextRenderOptions;
pub(crate) use spec::RichtextNodeSpec;
