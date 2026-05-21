//! Configuration for relationship and join fields.

use serde::{Deserialize, Serialize};

use crate::core::Slug;
use crate::typegen::lua::LuaAnnotation;

/// Configuration for relationship fields (target collection, cardinality, depth cap).
#[derive(Debug, Clone, Serialize, Deserialize, LuaAnnotation)]
#[lua(class = "crap.RelationshipConfig")]
pub struct RelationshipConfig {
    /// Target collection slug, or an array of slugs for polymorphic relationships (required). Example: `"posts"` or `{ "posts", "pages" }`.
    #[lua(ty = "string|string[]")]
    pub collection: Slug,
    /// Many-to-many relationship via junction table (default: false).
    #[lua(optional)]
    pub has_many: bool,
    /// Per-field max population depth. Limits depth regardless of request-level depth.
    #[serde(default)]
    pub max_depth: Option<i32>,
    // Polymorphic targets — folded into `collection` on the Lua side
    // (union `string|string[]`). Internal field; the array form of
    // `collection` is split into `polymorphic` + `collection` (first)
    // by the parser.
    #[serde(default)]
    #[lua(skip)]
    pub polymorphic: Vec<Slug>,
}

impl RelationshipConfig {
    /// Create a new relationship configuration for a single target collection.
    pub fn new(collection: impl Into<Slug>, has_many: bool) -> Self {
        Self {
            collection: collection.into(),
            has_many,
            max_depth: None,
            polymorphic: vec![],
        }
    }

    /// Returns true if this relationship targets multiple collections.
    #[must_use]
    pub fn is_polymorphic(&self) -> bool {
        !self.polymorphic.is_empty()
    }

    /// Returns all target collections (polymorphic list, or single `collection`).
    #[must_use]
    pub fn all_collections(&self) -> Vec<&str> {
        if self.is_polymorphic() {
            self.polymorphic
                .iter()
                .map(std::convert::AsRef::as_ref)
                .collect()
        } else {
            vec![self.collection.as_ref()]
        }
    }
}

/// Configuration for join (virtual reverse-relationship) fields.
//
// Surfaced on Lua's `crap.JoinField` via `flatten_from = JoinConfig`
// on `FieldType::Join`. The `crap.JoinConfig` class block itself is
// never emitted — only its fields, inlined into `crap.JoinField` after
// the BaseField inheritance line.
#[derive(Debug, Clone, Serialize, Deserialize, LuaAnnotation)]
#[lua(class = "crap.JoinConfig")]
pub struct JoinConfig {
    /// Target collection slug (required).
    #[lua(ty = "string")]
    pub collection: Slug,
    /// Field on target collection that references this document (required).
    pub on: String,
}

impl JoinConfig {
    /// Create a new join configuration (virtual reverse-relationship).
    pub fn new(collection: impl Into<Slug>, on: impl Into<String>) -> Self {
        Self {
            collection: collection.into(),
            on: on.into(),
        }
    }
}
