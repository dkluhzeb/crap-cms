//! Shared test fixtures for resolve/ submodules.

use crate::core::{BlockDefinition, FieldDefinition, FieldType, RelationshipConfig};
pub(super) fn test_conn() -> (tempfile::TempDir, crate::db::BoxedConnection) {
    let dir = tempfile::TempDir::new().unwrap();
    let config = crate::config::CrapConfig::default();
    let p = crate::db::pool::create_pool(dir.path(), &config).unwrap();
    (dir, p.get().unwrap())
}

pub(super) fn make_field(name: &str, ft: FieldType, localized: bool) -> FieldDefinition {
    FieldDefinition::builder(name, ft)
        .localized(localized)
        .build()
}

pub(super) fn make_array_field(name: &str, sub_fields: Vec<FieldDefinition>) -> FieldDefinition {
    FieldDefinition::builder(name, FieldType::Array)
        .fields(sub_fields)
        .build()
}

pub(super) fn make_blocks_field(name: &str, blocks: Vec<BlockDefinition>) -> FieldDefinition {
    FieldDefinition::builder(name, FieldType::Blocks)
        .blocks(blocks)
        .build()
}

pub(super) fn make_has_many_field(name: &str, collection: &str) -> FieldDefinition {
    FieldDefinition::builder(name, FieldType::Relationship)
        .relationship(RelationshipConfig::new(collection, true))
        .build()
}

pub(super) fn make_block_def(block_type: &str, fields: Vec<FieldDefinition>) -> BlockDefinition {
    BlockDefinition::new(block_type, fields)
}
