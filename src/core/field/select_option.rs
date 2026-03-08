//! Select field options (label/value pairs).

use super::LocalizedString;
use serde::{Deserialize, Serialize};

/// A label/value pair for select field options.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectOption {
    pub label: LocalizedString,
    pub value: String,
}

impl SelectOption {
    pub fn new(label: LocalizedString, value: impl Into<String>) -> Self {
        Self { label, value: value.into() }
    }
}
