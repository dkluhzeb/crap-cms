//! How a field's value is stored and read back: the predicates the column
//! encoding, the read decoding and the JSON-stored row forms share.

use crate::core::{FieldDefinition, FieldType};

impl FieldDefinition {
    /// Whether the field's text is JSON that reads return parsed: a `json`
    /// field, or a rich text field stored as a JSON document
    /// (`admin.format = "json"`). The column keeps the JSON text; every read
    /// surface — a document, a row, a snapshot, an event — returns the value it
    /// spells.
    #[must_use]
    pub fn parses_json(&self) -> bool {
        match self.field_type {
            FieldType::Json => true,
            FieldType::Richtext => self.admin.richtext_format.as_deref() == Some("json"),
            _ => false,
        }
    }

    /// Whether the field is a has-many relationship or upload — a list of ids
    /// kept in a join table at the top level and as JSON text inside a row.
    #[must_use]
    pub fn is_has_many_reference(&self) -> bool {
        self.field_type.is_reference() && self.relationship.as_ref().is_some_and(|rc| rc.has_many)
    }

    /// Whether the field holds a list of values: a scalar has-many list or a
    /// has-many reference (whose list-ness lives on its relationship config).
    #[must_use]
    pub fn is_list(&self) -> bool {
        self.has_many || self.is_has_many_reference()
    }
}

#[cfg(test)]
mod tests {
    use crate::core::{FieldAdmin, FieldDefinition, FieldType, RelationshipConfig};

    #[test]
    fn a_json_field_and_json_rich_text_parse_their_text() {
        let json = FieldDefinition::builder("meta", FieldType::Json).build();
        let json_richtext = FieldDefinition::builder("body", FieldType::Richtext)
            .admin(FieldAdmin::builder().richtext_format("json").build())
            .build();
        let html_richtext = FieldDefinition::builder("body", FieldType::Richtext).build();
        let text = FieldDefinition::builder("title", FieldType::Text).build();

        assert!(json.parses_json());
        assert!(json_richtext.parses_json());
        assert!(!html_richtext.parses_json());
        assert!(!text.parses_json());
    }

    #[test]
    fn only_a_has_many_relationship_or_upload_is_a_has_many_reference() {
        let many = FieldDefinition::builder("tags", FieldType::Relationship)
            .relationship(RelationshipConfig::new("tags", true))
            .build();
        let one = FieldDefinition::builder("author", FieldType::Upload)
            .relationship(RelationshipConfig::new("media", false))
            .build();
        let scalar_list = FieldDefinition::builder("tags", FieldType::Text)
            .has_many(true)
            .build();

        assert!(many.is_has_many_reference());
        assert!(!one.is_has_many_reference());
        assert!(!scalar_list.is_has_many_reference());

        assert!(many.is_list());
        assert!(scalar_list.is_list());
        assert!(!one.is_list());
    }
}
