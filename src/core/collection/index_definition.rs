//! Compound index definition for collection tables.

use serde::{Deserialize, Serialize};

use crate::typegen::lua::LuaAnnotation;

/// A compound index definition (multi-column, optionally unique).
#[derive(Debug, Clone, Serialize, Deserialize, LuaAnnotation)]
#[lua(class = "crap.IndexDefinition")]
pub struct IndexDefinition {
    /// Column names to include in the index (required).
    pub fields: Vec<String>,
    /// Create a UNIQUE index (default: false).
    #[serde(default)]
    #[lua(optional)]
    pub unique: bool,
}

impl IndexDefinition {
    /// Create a new index definition for the given fields.
    #[must_use]
    pub fn new(fields: Vec<String>) -> Self {
        Self {
            fields,
            unique: false,
        }
    }
}
