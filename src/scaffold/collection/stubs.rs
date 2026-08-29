//! In-memory representation of a parsed field tree, used by the parser
//! and the wizard before lowering to Lua via the writer.
//!
//! `FieldStub` is the leaf/container shape; container fields hold either
//! `Vec<FieldStub>` (group / array / row / collapsible), `Vec<BlockStub>`
//! (blocks fields), or `Vec<TabStub>` (tabs fields). The block/tab
//! shapes both bottom out at another `Vec<FieldStub>`, so the three
//! types form a small mutually-referential hierarchy and live in one
//! file.

use crate::core::Builder;

/// Stub for a field definition in the shorthand parser.
#[derive(Builder)]
pub struct FieldStub {
    #[builder(required)]
    pub name: String,
    #[builder(required)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_stub_builder_defaults_and_distinct_flags() {
        let stub = FieldStub::builder("title", "text").build();
        assert_eq!(stub.name, "title");
        assert_eq!(stub.field_type, "text");
        assert!(!stub.required);
        assert!(!stub.localized);
        assert!(stub.fields.is_empty());
        assert!(stub.blocks.is_empty());
        assert!(stub.tabs.is_empty());

        // Distinct flag values so a swapped assignment in `build()` shows up.
        let stub2 = FieldStub::builder("body", "richtext")
            .required(true)
            .localized(false)
            .build();
        assert!(stub2.required);
        assert!(!stub2.localized);
    }

    #[test]
    fn block_and_tab_stub_constructors() {
        let b = BlockStub::new(
            "hero",
            "Hero",
            vec![FieldStub::builder("x", "text").build()],
        );
        assert_eq!(b.block_type, "hero");
        assert_eq!(b.label, "Hero");
        assert_eq!(b.fields.len(), 1);

        let t = TabStub::new("SEO", vec![FieldStub::builder("y", "text").build()]);
        assert_eq!(t.label, "SEO");
        assert_eq!(t.fields.len(), 1);
    }
}
