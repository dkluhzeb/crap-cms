//! Fixtures shared by the locale submodule tests.

use crate::core::{FieldAdmin, FieldDefinition, FieldType};

/// A localized code field with a language allow-list — it stores a
/// per-locale `snippet_lang` companion.
pub(super) fn localized_code_lang_field(name: &str) -> FieldDefinition {
    FieldDefinition::builder(name, FieldType::Code)
        .admin(
            FieldAdmin::builder()
                .languages(vec!["javascript".to_string(), "python".to_string()])
                .build(),
        )
        .localized(true)
        .build()
}
