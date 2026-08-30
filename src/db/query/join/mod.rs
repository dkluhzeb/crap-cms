//! Join table operations: has-many relationships, arrays, blocks, hydration.

mod arrays;
mod blocks;
mod helpers;
pub(crate) mod hydrate;
mod relationships;

pub(crate) use arrays::find_all_array_rows_with_parent;
pub use arrays::{find_array_rows, find_array_rows_batch, set_array_rows};
pub use blocks::{find_block_rows, find_block_rows_batch, set_block_rows};
pub use hydrate::{hydrate_document, hydrate_documents, save_join_table_data};
pub(crate) use hydrate::{parse_id_list, parse_polymorphic_values};
pub use relationships::{
    find_polymorphic_related, find_polymorphic_related_batch, find_related_ids,
    find_related_ids_batch, set_polymorphic_related, set_related_ids,
};
