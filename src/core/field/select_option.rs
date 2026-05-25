//! Select field options (label/value pairs).

use crate::core::LocalizedString;
use crate::typegen::lua::LuaAnnotation;
use serde::{Deserialize, Serialize};

/// A label/value pair for select field options.
#[derive(Debug, Clone, Serialize, Deserialize, LuaAnnotation)]
#[lua(class = "crap.SelectOption")]
pub struct SelectOption {
    /// Display text in the admin UI.
    #[lua(ty = "crap.LocalizedString")]
    pub label: LocalizedString,
    /// Stored value.
    pub value: String,
}

impl SelectOption {
    pub fn new(label: LocalizedString, value: impl Into<String>) -> Self {
        Self {
            label,
            value: value.into(),
        }
    }
}
