//! Schema/field definition conversion to protobuf.

use crate::{
    api::content,
    core::{FieldDefinition, FieldType},
};

/// Convert a `FieldDefinition` to a protobuf `FieldInfo`, including options, blocks, and relationship metadata.
pub(in crate::api::handlers) fn field_def_to_proto(field: &FieldDefinition) -> content::FieldInfo {
    // Tabs stores sub-fields in field.tabs[*].fields, not field.fields.
    // Flatten all tab sub-fields into the proto `fields` list.
    let sub_fields: Vec<_> = if field.field_type == FieldType::Tabs {
        field
            .tabs
            .iter()
            .flat_map(|tab| tab.fields.iter())
            .map(field_def_to_proto)
            .collect()
    } else {
        field.fields.iter().map(field_def_to_proto).collect()
    };

    content::FieldInfo {
        name: field.name.clone(),
        r#type: field.field_type.as_str().to_string(),
        required: field.required,
        unique: field.unique,
        relationship_collection: field
            .relationship
            .as_ref()
            .map(|r| r.collection.to_string()),
        relationship_has_many: field.relationship.as_ref().map(|r| r.has_many),
        options: field
            .options
            .iter()
            .map(|o| content::SelectOptionInfo {
                label: o.label.resolve_default().to_string(),
                value: o.value.clone(),
            })
            .collect(),
        fields: sub_fields,
        relationship_max_depth: field.relationship.as_ref().and_then(|r| r.max_depth),
        blocks: field
            .blocks
            .iter()
            .map(|bd| content::BlockInfo {
                block_type: bd.block_type.clone(),
                label: bd.label.as_ref().map(|ls| ls.resolve_default().to_string()),
                fields: bd.fields.iter().map(field_def_to_proto).collect(),
                group: bd.group.clone(),
                image_url: bd.image_url.clone(),
            })
            .collect(),
        localized: field.localized,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::field::{
        BlockDefinition, FieldDefinition, FieldType, LocalizedString, RelationshipConfig,
        SelectOption,
    };

    fn make_field(name: &str, field_type: FieldType) -> FieldDefinition {
        FieldDefinition::builder(name, field_type).build()
    }

    #[test]
    fn field_def_to_proto_simple_text() {
        let field = FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .unique(true)
            .build();

        let proto = field_def_to_proto(&field);
        assert_eq!(proto.name, "title");
        assert_eq!(proto.r#type, "text");
        assert!(proto.required);
        assert!(proto.unique);
        assert!(proto.relationship_collection.is_none());
        assert!(proto.relationship_has_many.is_none());
        assert!(proto.options.is_empty());
        assert!(proto.fields.is_empty());
        assert!(proto.blocks.is_empty());
        assert!(!proto.localized);
    }

    #[test]
    fn field_def_to_proto_with_relationship() {
        let field = FieldDefinition::builder("author", FieldType::Relationship)
            .relationship({
                let mut rc = RelationshipConfig::new("authors", true);
                rc.max_depth = Some(3);
                rc
            })
            .build();

        let proto = field_def_to_proto(&field);
        assert_eq!(proto.r#type, "relationship");
        assert_eq!(proto.relationship_collection.as_deref(), Some("authors"));
        assert_eq!(proto.relationship_has_many, Some(true));
        assert_eq!(proto.relationship_max_depth, Some(3));
    }

    #[test]
    fn field_def_to_proto_with_options() {
        let field = FieldDefinition::builder("status", FieldType::Select)
            .options(vec![
                SelectOption::new(LocalizedString::Plain("Draft".to_string()), "draft"),
                SelectOption::new(LocalizedString::Plain("Published".to_string()), "published"),
            ])
            .build();

        let proto = field_def_to_proto(&field);
        assert_eq!(proto.options.len(), 2);
        assert_eq!(proto.options[0].label, "Draft");
        assert_eq!(proto.options[0].value, "draft");
        assert_eq!(proto.options[1].label, "Published");
        assert_eq!(proto.options[1].value, "published");
    }

    #[test]
    fn field_def_to_proto_with_blocks() {
        let field = FieldDefinition::builder("content", FieldType::Blocks)
            .blocks(vec![{
                let mut bd = BlockDefinition::new(
                    "text_block",
                    vec![make_field("body", FieldType::Textarea)],
                );
                bd.label = Some(LocalizedString::Plain("Text Block".to_string()));
                bd
            }])
            .build();

        let proto = field_def_to_proto(&field);
        assert_eq!(proto.blocks.len(), 1);
        assert_eq!(proto.blocks[0].block_type, "text_block");
        assert_eq!(proto.blocks[0].label.as_deref(), Some("Text Block"));
        assert_eq!(proto.blocks[0].fields.len(), 1);
        assert_eq!(proto.blocks[0].fields[0].name, "body");
        assert_eq!(proto.blocks[0].fields[0].r#type, "textarea");
    }

    #[test]
    fn field_def_to_proto_localized() {
        let field = FieldDefinition::builder("title", FieldType::Text)
            .localized(true)
            .build();

        let proto = field_def_to_proto(&field);
        assert!(proto.localized);
    }
}
