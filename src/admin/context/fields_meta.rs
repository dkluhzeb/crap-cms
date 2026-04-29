//! Field metadata exposed at `{{collection.fields_meta}}` and
//! `{{global.fields_meta}}` — a flat array of definitions templates use for
//! conditional logic (e.g., showing labels, descriptions, placeholders).

use serde::Serialize;

use crate::core::FieldDefinition;

/// Metadata about a single field as it appears to templates.
#[derive(Serialize)]
pub struct FieldMeta {
    pub name: String,
    pub field_type: String,
    pub required: bool,
    pub unique: bool,
    pub localized: bool,
    pub admin: FieldAdminMeta,
}

/// Admin-presentation metadata for a field (label, description, layout hints).
#[derive(Serialize)]
pub struct FieldAdminMeta {
    pub label: Option<String>,
    pub hidden: bool,
    pub readonly: bool,
    pub width: Option<String>,
    pub description: Option<String>,
    pub placeholder: Option<String>,
}

impl FieldMeta {
    /// Build the typed metadata for a single field definition.
    pub fn from_def(field: &FieldDefinition) -> Self {
        Self {
            name: field.name.clone(),
            field_type: field.field_type.as_str().to_string(),
            required: field.required,
            unique: field.unique,
            localized: field.localized,
            admin: FieldAdminMeta {
                label: field
                    .admin
                    .label
                    .as_ref()
                    .map(|ls| ls.resolve_default().to_string()),
                hidden: field.admin.hidden,
                readonly: field.admin.readonly,
                width: field.admin.width.clone(),
                description: field
                    .admin
                    .description
                    .as_ref()
                    .map(|ls| ls.resolve_default().to_string()),
                placeholder: field
                    .admin
                    .placeholder
                    .as_ref()
                    .map(|ls| ls.resolve_default().to_string()),
            },
        }
    }

    /// Build a vector of metadata entries from a slice of field definitions.
    pub fn from_defs(fields: &[FieldDefinition]) -> Vec<Self> {
        fields.iter().map(Self::from_def).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::core::field::{FieldAdmin, FieldType, LocalizedString};

    #[test]
    fn from_def_includes_admin_info() {
        let field = FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .unique(true)
            .localized(true)
            .admin(
                FieldAdmin::builder()
                    .label(LocalizedString::Plain("Title".to_string()))
                    .hidden(false)
                    .readonly(true)
                    .width("50%")
                    .description(LocalizedString::Plain("The title field".to_string()))
                    .placeholder(LocalizedString::Plain("Enter title".to_string()))
                    .build(),
            )
            .build();
        let meta = FieldMeta::from_def(&field);
        let v = serde_json::to_value(&meta).unwrap();
        assert_eq!(v["name"], "title");
        assert_eq!(v["field_type"], "text");
        assert_eq!(v["required"], true);
        assert_eq!(v["unique"], true);
        assert_eq!(v["localized"], true);
        assert_eq!(v["admin"]["label"], "Title");
        assert_eq!(v["admin"]["hidden"], false);
        assert_eq!(v["admin"]["readonly"], true);
        assert_eq!(v["admin"]["width"], "50%");
        assert_eq!(v["admin"]["description"], "The title field");
        assert_eq!(v["admin"]["placeholder"], "Enter title");
    }

    #[test]
    fn from_defs_empty_returns_empty_vec() {
        let metas = FieldMeta::from_defs(&[]);
        assert!(metas.is_empty());
    }

    #[test]
    fn from_defs_preserves_order() {
        let fields = vec![
            FieldDefinition::builder("first", FieldType::Text).build(),
            FieldDefinition::builder("second", FieldType::Number).build(),
        ];
        let metas = FieldMeta::from_defs(&fields);
        assert_eq!(metas.len(), 2);
        assert_eq!(metas[0].name, "first");
        assert_eq!(metas[1].name, "second");
    }
}
