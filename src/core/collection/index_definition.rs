//! Compound index definition for collection tables.

use serde::{Deserialize, Serialize};

/// A compound index definition (multi-column, optionally unique).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexDefinition {
    /// List of field names that make up the index.
    pub fields: Vec<String>,
    /// Whether this index should enforce uniqueness.
    #[serde(default)]
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
