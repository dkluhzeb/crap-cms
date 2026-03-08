//! Builder for [`FieldDefinition`](crate::core::field::FieldDefinition).

use crate::core::field::{
    BlockDefinition, FieldAccess, FieldAdmin, FieldDefinition, FieldHooks, FieldTab, FieldType,
    JoinConfig, McpFieldConfig, RelationshipConfig, SelectOption,
};

/// Builder for [`FieldDefinition`].
///
/// `name` and `field_type` are taken in `new()`. All other fields default via
/// [`FieldDefinition::default()`].
pub struct FieldDefinitionBuilder {
    inner: FieldDefinition,
}

impl FieldDefinitionBuilder {
    pub fn new(name: impl Into<String>, field_type: FieldType) -> Self {
        Self {
            inner: FieldDefinition {
                name: name.into(),
                field_type,
                ..Default::default()
            },
        }
    }

    pub fn required(mut self, v: bool) -> Self {
        self.inner.required = v;
        self
    }

    pub fn unique(mut self, v: bool) -> Self {
        self.inner.unique = v;
        self
    }

    pub fn index(mut self, v: bool) -> Self {
        self.inner.index = v;
        self
    }

    pub fn validate(mut self, v: impl Into<String>) -> Self {
        self.inner.validate = Some(v.into());
        self
    }

    pub fn default_value(mut self, v: serde_json::Value) -> Self {
        self.inner.default_value = Some(v);
        self
    }

    pub fn options(mut self, v: Vec<SelectOption>) -> Self {
        self.inner.options = v;
        self
    }

    pub fn admin(mut self, v: FieldAdmin) -> Self {
        self.inner.admin = v;
        self
    }

    pub fn hooks(mut self, v: FieldHooks) -> Self {
        self.inner.hooks = v;
        self
    }

    pub fn access(mut self, v: FieldAccess) -> Self {
        self.inner.access = v;
        self
    }

    pub fn mcp(mut self, v: McpFieldConfig) -> Self {
        self.inner.mcp = v;
        self
    }

    pub fn relationship(mut self, v: RelationshipConfig) -> Self {
        self.inner.relationship = Some(v);
        self
    }

    pub fn fields(mut self, v: Vec<FieldDefinition>) -> Self {
        self.inner.fields = v;
        self
    }

    pub fn blocks(mut self, v: Vec<BlockDefinition>) -> Self {
        self.inner.blocks = v;
        self
    }

    pub fn tabs(mut self, v: Vec<FieldTab>) -> Self {
        self.inner.tabs = v;
        self
    }

    pub fn localized(mut self, v: bool) -> Self {
        self.inner.localized = v;
        self
    }

    pub fn picker_appearance(mut self, v: impl Into<String>) -> Self {
        self.inner.picker_appearance = Some(v.into());
        self
    }

    pub fn min_rows(mut self, v: usize) -> Self {
        self.inner.min_rows = Some(v);
        self
    }

    pub fn max_rows(mut self, v: usize) -> Self {
        self.inner.max_rows = Some(v);
        self
    }

    pub fn min_length(mut self, v: usize) -> Self {
        self.inner.min_length = Some(v);
        self
    }

    pub fn max_length(mut self, v: usize) -> Self {
        self.inner.max_length = Some(v);
        self
    }

    pub fn min(mut self, v: f64) -> Self {
        self.inner.min = Some(v);
        self
    }

    pub fn max(mut self, v: f64) -> Self {
        self.inner.max = Some(v);
        self
    }

    pub fn has_many(mut self, v: bool) -> Self {
        self.inner.has_many = v;
        self
    }

    pub fn min_date(mut self, v: impl Into<String>) -> Self {
        self.inner.min_date = Some(v.into());
        self
    }

    pub fn max_date(mut self, v: impl Into<String>) -> Self {
        self.inner.max_date = Some(v.into());
        self
    }

    pub fn join(mut self, v: JoinConfig) -> Self {
        self.inner.join = Some(v);
        self
    }

    pub fn build(self) -> FieldDefinition {
        self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_field_definition_with_defaults() {
        let fd = FieldDefinitionBuilder::new("title", FieldType::Text).build();
        assert_eq!(fd.name, "title");
        assert_eq!(fd.field_type, FieldType::Text);
        assert!(!fd.required);
        assert!(!fd.unique);
        assert!(fd.options.is_empty());
        assert!(fd.relationship.is_none());
        assert!(fd.fields.is_empty());
    }

    #[test]
    fn builds_field_definition_with_overrides() {
        let fd = FieldDefinitionBuilder::new("email", FieldType::Email)
            .required(true)
            .unique(true)
            .index(true)
            .max_length(255)
            .build();
        assert_eq!(fd.name, "email");
        assert_eq!(fd.field_type, FieldType::Email);
        assert!(fd.required);
        assert!(fd.unique);
        assert!(fd.index);
        assert_eq!(fd.max_length, Some(255));
    }

    #[test]
    fn builds_field_definition_with_relationship() {
        let fd = FieldDefinitionBuilder::new("author", FieldType::Relationship)
            .relationship(RelationshipConfig::new("users", false))
            .build();
        assert!(fd.relationship.is_some());
        assert_eq!(fd.relationship.unwrap().collection, "users");
    }

    #[test]
    fn builds_field_definition_with_has_many() {
        let fd = FieldDefinitionBuilder::new("tags", FieldType::Select)
            .has_many(true)
            .options(vec![
                SelectOption::new(crate::core::field::LocalizedString::Plain("A".into()), "a"),
                SelectOption::new(crate::core::field::LocalizedString::Plain("B".into()), "b"),
            ])
            .build();
        assert!(fd.has_many);
        assert_eq!(fd.options.len(), 2);
    }
}
