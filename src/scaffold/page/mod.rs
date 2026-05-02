//! `make page` command — scaffold a custom admin page (HBS template
//! plus an optional `crap.pages.register` snippet for sidebar nav).

mod generator;

pub use generator::{MakePageOptions, make_page};
