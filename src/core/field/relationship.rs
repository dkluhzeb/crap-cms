//! Configuration for relationship and join fields.

use serde::{Deserialize, Serialize};

/// Configuration for relationship fields (target collection, cardinality, depth cap).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelationshipConfig {
    pub collection: String,
    pub has_many: bool,
    /// Per-field max depth. If set, limits population depth for this field
    /// regardless of the request-level depth.
    #[serde(default)]
    pub max_depth: Option<i32>,
    /// Polymorphic relationship: additional target collections beyond `collection`.
    /// Empty = single-collection relationship (default, backward compat).
    /// Non-empty = polymorphic (all targets listed here, `collection` = first).
    #[serde(default)]
    pub polymorphic: Vec<String>,
}

impl RelationshipConfig {
    pub fn new(collection: impl Into<String>, has_many: bool) -> Self {
        Self {
            collection: collection.into(),
            has_many,
            max_depth: None,
            polymorphic: vec![],
        }
    }

    /// Returns true if this relationship targets multiple collections.
    pub fn is_polymorphic(&self) -> bool {
        !self.polymorphic.is_empty()
    }

    /// Returns all target collections (polymorphic list, or single `collection`).
    pub fn all_collections(&self) -> Vec<&str> {
        if self.is_polymorphic() {
            self.polymorphic.iter().map(|s| s.as_str()).collect()
        } else {
            vec![self.collection.as_str()]
        }
    }
}

/// Configuration for join (virtual reverse-relationship) fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinConfig {
    /// Target collection slug (the collection whose documents reference this one).
    pub collection: String,
    /// Field name on the target collection that holds this document's ID.
    pub on: String,
}

impl JoinConfig {
    pub fn new(collection: impl Into<String>, on: impl Into<String>) -> Self {
        Self {
            collection: collection.into(),
            on: on.into(),
        }
    }
}
