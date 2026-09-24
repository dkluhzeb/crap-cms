//! Fixtures shared by the ref-count backfill's unit tests.

use crate::core::{CollectionDefinition, FieldDefinition, FieldType, Registry, RelationshipConfig};

/// A registry over `collections`, without touching any database.
pub(super) fn registry_of(collections: &[CollectionDefinition]) -> Registry {
    let shared = Registry::shared();
    {
        let mut reg = shared.write().unwrap();
        for c in collections {
            reg.register_collection(c.clone());
        }
    }

    (*Registry::snapshot(&shared)).clone()
}

pub(super) fn upload_to(name: &str, target: &str) -> FieldDefinition {
    FieldDefinition::builder(name, FieldType::Upload)
        .relationship(RelationshipConfig::new(target, false))
        .build()
}

pub(super) fn posts_with(fields: Vec<FieldDefinition>) -> CollectionDefinition {
    let mut posts = CollectionDefinition::new("posts");
    posts.fields = fields;

    posts
}
