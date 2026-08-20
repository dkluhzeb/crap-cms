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

    /// Cap the remaining populate `depth` by this relationship's own
    /// `max_depth`: the effective depth to descend with is the smaller of the
    /// two (an unset `max_depth` imposes no cap). One place for the per-field
    /// recursion-limit rule, shared by every populate dispatch site.
    #[must_use]
    pub fn cap_depth(&self, current: i32) -> i32 {
        match self.max_depth {
            Some(max) if max < current => max,
            _ => current,
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_sets_single_target_with_defaults() {
        let rc = RelationshipConfig::new("posts", true);
        assert_eq!(rc.collection.as_ref(), "posts");
        assert!(rc.has_many);
        assert_eq!(rc.max_depth, None);
        assert!(!rc.is_polymorphic());
        assert_eq!(rc.all_collections(), vec!["posts"]);
    }

    #[test]
    fn single_target_is_not_polymorphic() {
        let rc = RelationshipConfig::new("media", false);
        assert!(!rc.is_polymorphic());
        // With no polymorphic targets, `all_collections` falls back to the
        // single `collection`.
        assert_eq!(rc.all_collections(), vec!["media"]);
    }

    #[test]
    fn polymorphic_reports_every_target_in_order() {
        let rc = RelationshipConfig {
            collection: "posts".into(),
            has_many: false,
            max_depth: Some(2),
            polymorphic: vec!["posts".into(), "pages".into()],
        };
        assert!(rc.is_polymorphic());
        // Polymorphic wins over the single `collection`, preserving order.
        assert_eq!(rc.all_collections(), vec!["posts", "pages"]);
    }

    #[test]
    fn join_config_new_sets_collection_and_on() {
        let jc = JoinConfig::new("comments", "post_id");
        assert_eq!(jc.collection.as_ref(), "comments");
        assert_eq!(jc.on, "post_id");
    }
}
