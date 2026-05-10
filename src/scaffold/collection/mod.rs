//! `make collection` command -- generate collection Lua files.

mod collection_options;
mod field_types;
mod generator;
pub(crate) mod parser;
mod stubs;
mod writer;

pub use collection_options::CollectionOptions;
pub use field_types::{CONTAINER_TYPES, VALID_FIELD_TYPES};
pub use generator::make_collection;
pub use parser::parse_fields_shorthand;
pub use stubs::{BlockStub, FieldStub, TabStub};
pub use writer::write_field_lua;
