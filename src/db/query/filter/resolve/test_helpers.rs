//! Shared test fixtures for resolve/ submodules.

use tempfile::TempDir;

use crate::{
    config::CrapConfig,
    core::{BlockDefinition, FieldDefinition, FieldType, RelationshipConfig, ValidationError},
    db::{BoxedConnection, pool},
};

use super::resolve_filter;

pub(super) fn test_conn() -> (TempDir, BoxedConnection) {
    let dir = TempDir::new().unwrap();
    let config = CrapConfig::default();
    let p = pool::create_pool(dir.path(), &config).unwrap();
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

/// The validation error `path` resolves to against `fields`, and the field it
/// names.
pub(super) fn path_error(fields: &[FieldDefinition], path: &str) -> (String, String) {
    let (_dir, conn) = test_conn();
    let err = resolve_filter(&conn, path, "posts", fields, None).unwrap_err();
    let ve = err
        .downcast_ref::<ValidationError>()
        .unwrap_or_else(|| panic!("{path}: untyped error {err:#}"));

    (ve.errors[0].field.clone(), ve.errors[0].message.clone())
}
