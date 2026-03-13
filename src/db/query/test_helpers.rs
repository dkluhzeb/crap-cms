//! Shared test helpers for db::query module tests.

use crate::core::{
    collection::CollectionDefinition,
    field::{FieldDefinition, FieldTab, FieldType},
};

pub fn make_field(name: &str, field_type: FieldType) -> FieldDefinition {
    FieldDefinition::builder(name, field_type).build()
}

pub fn make_localized_field(name: &str, field_type: FieldType) -> FieldDefinition {
    FieldDefinition::builder(name, field_type)
        .localized(true)
        .build()
}

pub fn make_group_field(name: &str, sub_fields: Vec<FieldDefinition>) -> FieldDefinition {
    FieldDefinition::builder(name, FieldType::Group)
        .fields(sub_fields)
        .build()
}

pub fn make_collection_def(
    slug: &str,
    fields: Vec<FieldDefinition>,
    timestamps: bool,
) -> CollectionDefinition {
    let mut def = CollectionDefinition::new(slug);
    def.fields = fields;
    def.timestamps = timestamps;
    def
}

pub fn make_locale_config() -> crate::config::LocaleConfig {
    crate::config::LocaleConfig {
        default_locale: "en".to_string(),
        locales: vec!["en".to_string(), "de".to_string()],
        fallback: true,
    }
}

pub fn make_row_field(name: &str, sub_fields: Vec<FieldDefinition>) -> FieldDefinition {
    FieldDefinition::builder(name, FieldType::Row)
        .fields(sub_fields)
        .build()
}

pub fn make_collapsible_field(name: &str, sub_fields: Vec<FieldDefinition>) -> FieldDefinition {
    FieldDefinition::builder(name, FieldType::Collapsible)
        .fields(sub_fields)
        .build()
}

pub fn make_tabs_field(name: &str, tabs: Vec<FieldTab>) -> FieldDefinition {
    FieldDefinition::builder(name, FieldType::Tabs)
        .tabs(tabs)
        .build()
}
