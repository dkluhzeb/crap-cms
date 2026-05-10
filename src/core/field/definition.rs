//! Complete definition of a single field within a collection.

use crate::core::{
    BlockDefinition, FieldAdmin, FieldTab, FieldType, RelationshipConfig, SelectOption,
    field::JoinConfig,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Lua function references for field-level access control (read/create/update).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FieldAccess {
    /// Lua function name for read access control.
    #[serde(default)]
    pub read: Option<String>,
    /// Lua function name for create access control.
    #[serde(default)]
    pub create: Option<String>,
    /// Lua function name for update access control.
    #[serde(default)]
    pub update: Option<String>,
}

/// Lua function references for field-level lifecycle hooks.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FieldHooks {
    /// Lua function names for before-validate hooks.
    #[serde(default)]
    pub before_validate: Vec<String>,
    /// Lua function names for before-change hooks.
    #[serde(default)]
    pub before_change: Vec<String>,
    /// Lua function names for after-change hooks.
    #[serde(default)]
    pub after_change: Vec<String>,
    /// Lua function names for after-read hooks.
    #[serde(default)]
    pub after_read: Vec<String>,
}

/// Complete definition of a single field within a collection.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FieldDefinition {
    /// Unique identifier for the field within the collection. Used as column name.
    pub name: String,
    /// The data type of the field.
    #[serde(rename = "type")]
    pub field_type: FieldType,
    /// Whether the field is required to have a value.
    #[serde(default)]
    pub required: bool,
    /// Whether the field must have a unique value across the collection.
    #[serde(default)]
    pub unique: bool,
    /// Whether to create a database index for this field.
    #[serde(default)]
    pub index: bool,
    /// Optional Lua validation function name.
    #[serde(default)]
    pub validate: Option<String>,
    /// Default value for the field when creating new items.
    #[serde(default)]
    pub default_value: Option<Value>,
    /// List of options for Select and Radio fields.
    #[serde(default)]
    pub options: Vec<SelectOption>,
    /// Configuration for the admin UI representation of this field.
    #[serde(default)]
    pub admin: FieldAdmin,
    /// Lifecycle hooks specific to this field.
    #[serde(default)]
    pub hooks: FieldHooks,
    /// Access control rules for this field.
    #[serde(default)]
    pub access: FieldAccess,
    /// MCP-specific configuration for this field.
    #[serde(default)]
    pub mcp: McpFieldConfig,
    /// Configuration for Relationship and Upload fields.
    #[serde(default)]
    pub relationship: Option<RelationshipConfig>,
    /// Sub-fields for Group and Array types.
    #[serde(default)]
    pub fields: Vec<FieldDefinition>,
    /// Block definitions for Blocks type.
    #[serde(default)]
    pub blocks: Vec<BlockDefinition>,
    /// Tab definitions for Tabs layout type.
    #[serde(default)]
    pub tabs: Vec<FieldTab>,
    /// Whether the field's value is localized.
    #[serde(default)]
    pub localized: bool,
    /// For date fields: controls the HTML input type and storage format.
    /// Valid values: "dayOnly" (default), "dayAndTime", "timeOnly", "monthOnly".
    #[serde(default)]
    pub picker_appearance: Option<String>,
    /// Minimum number of rows (array/blocks). Validated on create/update.
    #[serde(default)]
    pub min_rows: Option<usize>,
    /// Maximum number of rows (array/blocks). Validated on create/update.
    #[serde(default)]
    pub max_rows: Option<usize>,
    /// Minimum string length (text/textarea). Validated server-side.
    #[serde(default)]
    pub min_length: Option<usize>,
    /// Maximum string length (text/textarea). Validated server-side + HTML attr.
    #[serde(default)]
    pub max_length: Option<usize>,
    /// Minimum numeric value (number fields). Validated server-side + HTML attr.
    #[serde(default)]
    pub min: Option<f64>,
    /// Maximum numeric value (number fields). Validated server-side + HTML attr.
    #[serde(default)]
    pub max: Option<f64>,
    /// Allow multiple values (select). Stored as JSON array in TEXT column.
    #[serde(default)]
    pub has_many: bool,
    /// Minimum date value (date fields). ISO format: "2024-01-01". Validated server-side + HTML min attr.
    #[serde(default)]
    pub min_date: Option<String>,
    /// Maximum date value (date fields). ISO format: "2025-12-31". Validated server-side + HTML max attr.
    #[serde(default)]
    pub max_date: Option<String>,
    /// Whether to store an IANA timezone alongside the date value.
    /// Only meaningful for dayOnly and dayAndTime picker appearances.
    #[serde(default)]
    pub timezone: bool,
    /// Default IANA timezone for the admin UI dropdown (e.g., "America/New_York").
    #[serde(default)]
    pub default_timezone: Option<String>,
    /// Configuration for join (virtual reverse-relationship) fields.
    #[serde(default)]
    pub join: Option<JoinConfig>,
    /// Strip this field from all read responses (gRPC, Lua, MCP, admin JSON, REST).
    /// Writes are not stripped — internal hooks/Lua can still write the column.
    /// Implies the field is also skipped in the admin form (no data to render).
    /// See `admin.hidden` for the admin-form-only equivalent (still returned in API).
    #[serde(default)]
    pub hidden: bool,
}

impl FieldDefinition {
    /// Create a new `FieldDefinitionBuilder` with the given name and type.
    pub fn builder(name: impl Into<String>, field_type: FieldType) -> FieldDefinitionBuilder {
        FieldDefinitionBuilder::new(name, field_type)
    }

    /// Whether this field has a column on the parent table.
    /// False for Array, Group, Row, Blocks, and has-many Relationship (they use join tables or prefixed/promoted columns).
    pub fn has_parent_column(&self) -> bool {
        match self.field_type {
            FieldType::Array => false,
            FieldType::Group => false, // sub-fields get prefixed columns instead
            FieldType::Row => false,   // sub-fields promoted to parent level (no prefix)
            FieldType::Collapsible => false, // sub-fields promoted to parent level (no prefix)
            FieldType::Tabs => false,  // sub-fields promoted to parent level (no prefix)
            FieldType::Blocks => false, // uses a join table
            FieldType::Join => false,  // virtual field, no column
            FieldType::Relationship | FieldType::Upload => {
                match &self.relationship {
                    Some(rc) => !rc.has_many,
                    None => true, // default to has-one
                }
            }
            _ => true,
        }
    }
}

/// Convert a snake_case identifier to Title Case.
///
/// Examples: `"my_field"` → `"My Field"`, `"site_settings"` → `"Site Settings"`.
/// Used to auto-generate human-readable labels from field and collection names.
pub fn to_title_case(s: &str) -> String {
    s.split('_')
        .filter(|w| !w.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Recursively flatten layout wrappers (Row, Collapsible, Tabs) to extract leaf fields.
/// Used by Array join table DDL, read, write, and form parsing — layout wrappers are
/// transparent inside arrays, so their children should be promoted as individual columns.
pub fn flatten_array_sub_fields(fields: &[FieldDefinition]) -> Vec<&FieldDefinition> {
    let mut result = Vec::new();
    for f in fields {
        match f.field_type {
            FieldType::Row | FieldType::Collapsible => {
                result.extend(flatten_array_sub_fields(&f.fields));
            }
            FieldType::Tabs => {
                for tab in &f.tabs {
                    result.extend(flatten_array_sub_fields(&tab.fields));
                }
            }
            _ => result.push(f),
        }
    }
    result
}

/// MCP-specific configuration for a field.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct McpFieldConfig {
    /// Description used in MCP tool JSON Schema for this field.
    pub description: Option<String>,
}

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

    /// Set whether to store an IANA timezone alongside the date value.
    pub fn timezone(mut self, v: bool) -> Self {
        self.inner.timezone = v;
        self
    }

    /// Set the default IANA timezone for the admin UI dropdown.
    pub fn default_timezone(mut self, v: impl Into<String>) -> Self {
        self.inner.default_timezone = Some(v.into());
        self
    }

    /// Set the join configuration for virtual reverse-relationship fields.
    pub fn join(mut self, v: JoinConfig) -> Self {
        self.inner.join = Some(v);
        self
    }

    /// Set whether this field is stripped from all read responses (API-hidden).
    /// See [`FieldDefinition::hidden`] for full semantics.
    pub fn hidden(mut self, v: bool) -> Self {
        self.inner.hidden = v;
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
    use crate::core::LocalizedString;

    #[test]
    fn has_parent_column_scalar_types() {
        for ft in [
            FieldType::Text,
            FieldType::Number,
            FieldType::Textarea,
            FieldType::Select,
            FieldType::Checkbox,
            FieldType::Date,
            FieldType::Email,
            FieldType::Json,
            FieldType::Richtext,
            FieldType::Upload,
        ] {
            let f = FieldDefinition {
                field_type: ft.clone(),
                ..Default::default()
            };
            assert!(f.has_parent_column(), "{:?} should have parent column", ft);
        }
    }

    #[test]
    fn has_parent_column_array_false() {
        let f = FieldDefinition {
            field_type: FieldType::Array,
            ..Default::default()
        };
        assert!(!f.has_parent_column());
    }

    #[test]
    fn has_parent_column_group_false() {
        let f = FieldDefinition {
            field_type: FieldType::Group,
            ..Default::default()
        };
        assert!(!f.has_parent_column());
    }

    #[test]
    fn has_parent_column_blocks_false() {
        let f = FieldDefinition {
            field_type: FieldType::Blocks,
            ..Default::default()
        };
        assert!(!f.has_parent_column());
    }

    #[test]
    fn has_parent_column_row_false() {
        let f = FieldDefinition {
            field_type: FieldType::Row,
            ..Default::default()
        };
        assert!(!f.has_parent_column(), "Row should not have parent column");
    }

    #[test]
    fn has_parent_column_collapsible_false() {
        let f = FieldDefinition {
            field_type: FieldType::Collapsible,
            ..Default::default()
        };
        assert!(
            !f.has_parent_column(),
            "Collapsible should not have parent column"
        );
    }

    #[test]
    fn has_parent_column_tabs_false() {
        let f = FieldDefinition {
            field_type: FieldType::Tabs,
            ..Default::default()
        };
        assert!(!f.has_parent_column(), "Tabs should not have parent column");
    }

    #[test]
    fn has_parent_column_relationship_has_one() {
        let f = FieldDefinition {
            field_type: FieldType::Relationship,
            relationship: Some(RelationshipConfig::new("posts", false)),
            ..Default::default()
        };
        assert!(
            f.has_parent_column(),
            "has-one relationship should have parent column"
        );
    }

    #[test]
    fn has_parent_column_relationship_has_many() {
        let f = FieldDefinition {
            field_type: FieldType::Relationship,
            relationship: Some(RelationshipConfig::new("tags", true)),
            ..Default::default()
        };
        assert!(
            !f.has_parent_column(),
            "has-many relationship should not have parent column"
        );
    }

    #[test]
    fn has_parent_column_relationship_no_config() {
        let f = FieldDefinition {
            field_type: FieldType::Relationship,
            relationship: None,
            ..Default::default()
        };
        assert!(
            f.has_parent_column(),
            "relationship with no config defaults to has-one"
        );
    }

    #[test]
    fn has_parent_column_upload_has_many_false() {
        let f = FieldDefinition {
            field_type: FieldType::Upload,
            relationship: Some(RelationshipConfig::new("media", true)),
            ..Default::default()
        };
        assert!(
            !f.has_parent_column(),
            "has-many upload should not have parent column"
        );
    }

    #[test]
    fn has_parent_column_upload_has_one_true() {
        let f = FieldDefinition {
            field_type: FieldType::Upload,
            relationship: Some(RelationshipConfig::new("media", false)),
            ..Default::default()
        };
        assert!(
            f.has_parent_column(),
            "has-one upload should have parent column"
        );
    }

    #[test]
    fn field_definition_default() {
        let f = FieldDefinition::default();
        assert_eq!(f.name, "");
        assert_eq!(f.field_type, FieldType::Text);
        assert!(!f.required);
        assert!(!f.unique);
        assert!(!f.index);
        assert!(!f.localized);
        assert!(f.validate.is_none());
        assert!(f.default_value.is_none());
        assert!(f.options.is_empty());
        assert!(f.relationship.is_none());
        assert!(f.fields.is_empty());
        assert!(f.blocks.is_empty());
        assert!(f.tabs.is_empty());
        assert!(f.min_rows.is_none());
        assert!(f.max_rows.is_none());
        assert!(f.min_length.is_none());
        assert!(f.max_length.is_none());
        assert!(f.min.is_none());
        assert!(f.max.is_none());
    }

    fn text_field(name: &str) -> FieldDefinition {
        FieldDefinition {
            name: name.to_string(),
            field_type: FieldType::Text,
            ..Default::default()
        }
    }

    #[test]
    fn flatten_array_sub_fields_basic() {
        let fields = vec![
            text_field("title"),
            FieldDefinition {
                name: "layout".to_string(),
                field_type: FieldType::Row,
                fields: vec![text_field("slug"), text_field("author")],
                ..Default::default()
            },
            FieldDefinition {
                name: "settings".to_string(),
                field_type: FieldType::Tabs,
                tabs: vec![
                    FieldTab::new("General", vec![text_field("color")]),
                    FieldTab::new("Advanced", vec![text_field("cache")]),
                ],
                ..Default::default()
            },
        ];
        let flat = flatten_array_sub_fields(&fields);
        let names: Vec<&str> = flat.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["title", "slug", "author", "color", "cache"]);
    }

    #[test]
    fn flatten_array_sub_fields_nested() {
        let fields = vec![FieldDefinition {
            name: "layout".to_string(),
            field_type: FieldType::Tabs,
            tabs: vec![FieldTab::new(
                "Tab",
                vec![FieldDefinition {
                    name: "row".to_string(),
                    field_type: FieldType::Row,
                    fields: vec![text_field("a"), text_field("b")],
                    ..Default::default()
                }],
            )],
            ..Default::default()
        }];
        let flat = flatten_array_sub_fields(&fields);
        let names: Vec<&str> = flat.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    fn flatten_array_sub_fields_empty() {
        let flat = flatten_array_sub_fields(&[]);
        assert!(flat.is_empty());
    }

    #[test]
    fn flatten_array_sub_fields_collapsible() {
        let fields = vec![FieldDefinition {
            name: "advanced".to_string(),
            field_type: FieldType::Collapsible,
            fields: vec![text_field("x"), text_field("y")],
            ..Default::default()
        }];
        let flat = flatten_array_sub_fields(&fields);
        let names: Vec<&str> = flat.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["x", "y"]);
    }

    #[test]
    fn to_title_case_single_word() {
        assert_eq!(to_title_case("posts"), "Posts");
    }

    #[test]
    fn to_title_case_multi_word() {
        assert_eq!(to_title_case("site_settings"), "Site Settings");
    }

    #[test]
    fn to_title_case_three_words() {
        assert_eq!(to_title_case("my_cool_thing"), "My Cool Thing");
    }

    #[test]
    fn to_title_case_empty() {
        assert_eq!(to_title_case(""), "");
    }

    #[test]
    fn to_title_case_double_underscore() {
        assert_eq!(to_title_case("seo__title"), "Seo Title");
    }

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
                SelectOption::new(LocalizedString::Plain("A".into()), "a"),
                SelectOption::new(LocalizedString::Plain("B".into()), "b"),
            ])
            .build();
        assert!(fd.has_many);
        assert_eq!(fd.options.len(), 2);
    }
}
