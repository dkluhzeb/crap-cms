//! Builder for [`FieldDefinition`](super::FieldDefinition).

use serde_json::Value;

use super::{
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
    /// Create a new `FieldDefinitionBuilder` with the given name and type.
    pub fn new(name: impl Into<String>, field_type: FieldType) -> Self {
        Self {
            inner: FieldDefinition {
                name: name.into(),
                field_type,
                ..Default::default()
            },
        }
    }

    /// Set whether the field is required.
    pub fn required(mut self, v: bool) -> Self {
        self.inner.required = v;
        self
    }

    /// Set whether the field must be unique.
    pub fn unique(mut self, v: bool) -> Self {
        self.inner.unique = v;
        self
    }

    /// Set whether to create a database index for this field.
    pub fn index(mut self, v: bool) -> Self {
        self.inner.index = v;
        self
    }

    /// Set the name of the Lua validation function.
    pub fn validate(mut self, v: impl Into<String>) -> Self {
        self.inner.validate = Some(v.into());
        self
    }

    /// Set the default value for this field.
    pub fn default_value(mut self, v: Value) -> Self {
        self.inner.default_value = Some(v);
        self
    }

    /// Set the options for Select or Radio fields.
    pub fn options(mut self, v: Vec<SelectOption>) -> Self {
        self.inner.options = v;
        self
    }

    /// Set the admin UI configuration for this field.
    pub fn admin(mut self, v: FieldAdmin) -> Self {
        self.inner.admin = v;
        self
    }

    /// Set the lifecycle hooks for this field.
    pub fn hooks(mut self, v: FieldHooks) -> Self {
        self.inner.hooks = v;
        self
    }

    /// Set the access control rules for this field.
    pub fn access(mut self, v: FieldAccess) -> Self {
        self.inner.access = v;
        self
    }

    /// Set the MCP-specific configuration for this field.
    pub fn mcp(mut self, v: McpFieldConfig) -> Self {
        self.inner.mcp = v;
        self
    }

    /// Set the relationship configuration for this field.
    pub fn relationship(mut self, v: RelationshipConfig) -> Self {
        self.inner.relationship = Some(v);
        self
    }

    /// Set the sub-fields for Group or Array types.
    pub fn fields(mut self, v: Vec<FieldDefinition>) -> Self {
        self.inner.fields = v;
        self
    }

    /// Set the block definitions for Blocks types.
    pub fn blocks(mut self, v: Vec<BlockDefinition>) -> Self {
        self.inner.blocks = v;
        self
    }

    /// Set the tab definitions for Tabs types.
    pub fn tabs(mut self, v: Vec<FieldTab>) -> Self {
        self.inner.tabs = v;
        self
    }

    /// Set whether this field is localized.
    pub fn localized(mut self, v: bool) -> Self {
        self.inner.localized = v;
        self
    }

    /// Set the picker appearance for date fields.
    pub fn picker_appearance(mut self, v: impl Into<String>) -> Self {
        self.inner.picker_appearance = Some(v.into());
        self
    }

    /// Set the minimum number of rows for Array or Blocks.
    pub fn min_rows(mut self, v: usize) -> Self {
        self.inner.min_rows = Some(v);
        self
    }

    /// Set the maximum number of rows for Array or Blocks.
    pub fn max_rows(mut self, v: usize) -> Self {
        self.inner.max_rows = Some(v);
        self
    }

    /// Set the minimum string length for text fields.
    pub fn min_length(mut self, v: usize) -> Self {
        self.inner.min_length = Some(v);
        self
    }

    /// Set the maximum string length for text fields.
    pub fn max_length(mut self, v: usize) -> Self {
        self.inner.max_length = Some(v);
        self
    }

    /// Set the minimum numeric value.
    pub fn min(mut self, v: f64) -> Self {
        self.inner.min = Some(v);
        self
    }

    /// Set the maximum numeric value.
    pub fn max(mut self, v: f64) -> Self {
        self.inner.max = Some(v);
        self
    }

    /// Set whether this field allows multiple values.
    pub fn has_many(mut self, v: bool) -> Self {
        self.inner.has_many = v;
        self
    }

    /// Set the minimum date value.
    pub fn min_date(mut self, v: impl Into<String>) -> Self {
        self.inner.min_date = Some(v.into());
        self
    }

    /// Set the maximum date value.
    pub fn max_date(mut self, v: impl Into<String>) -> Self {
        self.inner.max_date = Some(v.into());
        self
    }

    /// Set the join configuration for virtual reverse-relationship fields.
    pub fn join(mut self, v: JoinConfig) -> Self {
        self.inner.join = Some(v);
        self
    }

    /// Build the final `FieldDefinition` instance.
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
                SelectOption::new(super::super::LocalizedString::Plain("A".into()), "a"),
                SelectOption::new(super::super::LocalizedString::Plain("B".into()), "b"),
            ])
            .build();
        assert!(fd.has_many);
        assert_eq!(fd.options.len(), 2);
    }
}
