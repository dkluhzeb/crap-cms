//! In-memory representation of a parsed field tree, used by the parser
//! and the wizard before lowering to Lua via the writer.
//!
//! `FieldStub` is the leaf/container shape; container fields hold either
//! `Vec<FieldStub>` (group / array / row / collapsible), `Vec<BlockStub>`
//! (blocks fields), or `Vec<TabStub>` (tabs fields). The block/tab
//! shapes both bottom out at another `Vec<FieldStub>`, so the three
//! types form a small mutually-referential hierarchy and live in one
//! file.

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

/// Builder for [`FieldStub`].
pub struct FieldStubBuilder {
    name: String,
    field_type: String,
    required: bool,
    localized: bool,
    fields: Vec<FieldStub>,
    blocks: Vec<BlockStub>,
    tabs: Vec<TabStub>,
}

impl FieldStubBuilder {
    /// Set the required flag.
    pub fn required(mut self, v: bool) -> Self {
        self.required = v;
        self
    }

    /// Set the localized flag.
    pub fn localized(mut self, v: bool) -> Self {
        self.localized = v;
        self
    }

    /// Set nested fields (for group, array, row, collapsible).
    pub fn fields(mut self, v: Vec<FieldStub>) -> Self {
        self.fields = v;
        self
    }

    /// Set block definitions (for blocks fields).
    pub fn blocks(mut self, v: Vec<BlockStub>) -> Self {
        self.blocks = v;
        self
    }

    /// Set tab definitions (for tabs fields).
    pub fn tabs(mut self, v: Vec<TabStub>) -> Self {
        self.tabs = v;
        self
    }

    /// Build the field stub.
    pub fn build(self) -> FieldStub {
        FieldStub {
            name: self.name,
            field_type: self.field_type,
            required: self.required,
            localized: self.localized,
            fields: self.fields,
            blocks: self.blocks,
            tabs: self.tabs,
        }
    }
}

impl FieldStub {
    /// Create a builder with the required name and type.
    pub fn builder(name: impl Into<String>, field_type: impl Into<String>) -> FieldStubBuilder {
        FieldStubBuilder {
            name: name.into(),
            field_type: field_type.into(),
            required: false,
            localized: false,
            fields: Vec::new(),
            blocks: Vec::new(),
            tabs: Vec::new(),
        }
    }
}

/// Stub for a block definition within a blocks field.
pub struct BlockStub {
    pub block_type: String,
    pub label: String,
    pub fields: Vec<FieldStub>,
}

impl BlockStub {
    /// Create a new block stub.
    pub fn new(
        block_type: impl Into<String>,
        label: impl Into<String>,
        fields: Vec<FieldStub>,
    ) -> Self {
        Self {
            block_type: block_type.into(),
            label: label.into(),
            fields,
        }
    }
}

/// Stub for a tab definition within a tabs field.
pub struct TabStub {
    pub label: String,
    pub fields: Vec<FieldStub>,
}

impl TabStub {
    /// Create a new tab stub.
    pub fn new(label: impl Into<String>, fields: Vec<FieldStub>) -> Self {
        Self {
            label: label.into(),
            fields,
        }
    }
}
