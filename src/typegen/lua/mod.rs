//! LuaLS type definition generator for IDE support in hooks and init.lua.
//!
//! Split into:
//! - [`render`]: top-level [`render`](render::render) entry + per-collection /
//!   per-global / find-overload / template-data renderers.
//! - [`field`]: single-field rendering ([`write_field`](field::write_field) +
//!   `field_to_lua_type` mapping).
//!
//! Shared test fixtures (`text_field`, `select_field`, `checkbox_field`)
//! live in `pub(super) mod test_helpers` since both files exercise them.

mod field;
mod render;

pub(super) use render::render;

#[cfg(test)]
pub(super) mod test_helpers {
    use crate::core::{
        FieldDefinition, FieldType,
        field::{LocalizedString, SelectOption},
    };

    pub fn text_field(name: &str, required: bool) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text)
            .required(required)
            .build()
    }

    pub fn select_field(name: &str, required: bool, opts: &[&str]) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Select)
            .required(required)
            .options(
                opts.iter()
                    .map(|v| SelectOption::new(LocalizedString::Plain(v.to_string()), *v))
                    .collect(),
            )
            .build()
    }

    pub fn checkbox_field(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Checkbox).build()
    }
}
