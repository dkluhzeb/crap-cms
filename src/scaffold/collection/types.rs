//! Data types and constants for collection scaffolding.

/// Valid field types for collection definitions.
pub const VALID_FIELD_TYPES: &[&str] = &[
    "text",
    "number",
    "textarea",
    "select",
    "radio",
    "checkbox",
    "date",
    "email",
    "json",
    "richtext",
    "code",
    "relationship",
    "array",
    "group",
    "upload",
    "blocks",
    "row",
    "collapsible",
    "tabs",
    "join",
];

/// Container field types that support nested subfields.
pub const CONTAINER_TYPES: &[&str] = &["group", "array", "row", "collapsible"];

/// Boolean flags for collection scaffolding.
pub struct CollectionOptions {
    pub no_timestamps: bool,
    pub auth: bool,
    pub upload: bool,
    pub versions: bool,
    pub force: bool,
}

impl CollectionOptions {
    /// Create default options (all flags off).
    pub fn new() -> Self {
        Self {
            no_timestamps: false,
            auth: false,
            upload: false,
            versions: false,
            force: false,
        }
    }
}

impl Default for CollectionOptions {
    fn default() -> Self {
        Self::new()
    }
}

/// Stub for a field definition in the shorthand parser.
pub struct FieldStub {
    pub name: String,
    pub field_type: String,
    pub required: bool,
    pub localized: bool,
    pub fields: Vec<FieldStub>,
    pub blocks: Vec<BlockStub>,
    pub tabs: Vec<TabStub>,
}

/// Stub for a block definition within a blocks field.
pub struct BlockStub {
    pub block_type: String,
    pub label: String,
    pub fields: Vec<FieldStub>,
}

/// Stub for a tab definition within a tabs field.
pub struct TabStub {
    pub label: String,
    pub fields: Vec<FieldStub>,
}
