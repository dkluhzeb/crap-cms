//! Join table operations: has-many relationships, arrays, blocks, hydration.

mod relationships;
mod arrays;
mod blocks;
mod hydrate;

pub use relationships::{set_related_ids, find_related_ids, set_polymorphic_related, find_polymorphic_related};
pub use arrays::{set_array_rows, find_array_rows};
pub use blocks::{set_block_rows, find_block_rows};
pub use hydrate::{hydrate_document, save_join_table_data};
