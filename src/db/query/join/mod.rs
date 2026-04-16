//! Join table operations: has-many relationships, arrays, blocks, hydration.

mod arrays;
mod blocks;
mod helpers;
pub(crate) mod hydrate;
mod relationships;

pub use arrays::{find_array_rows, set_array_rows};
pub use blocks::{find_block_rows, set_block_rows};
pub use hydrate::{hydrate_document, save_join_table_data};
pub(crate) use hydrate::{parse_id_list, parse_polymorphic_values};
pub use relationships::{
    find_polymorphic_related, find_related_ids, set_polymorphic_related, set_related_ids,
};
