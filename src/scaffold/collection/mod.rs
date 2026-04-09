//! `make collection` command — generate collection Lua files.

mod generator;
pub(crate) mod parser;
mod types;
mod writer;

pub use generator::make_collection;
pub use parser::parse_fields_shorthand;
pub use types::{BlockStub, CollectionOptions, FieldStub, TabStub, VALID_FIELD_TYPES};
pub use writer::{type_specific_stub, write_field_lua};
