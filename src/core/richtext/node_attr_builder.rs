use crate::core::field::SelectOption;
use super::{NodeAttr, NodeAttrType};

/// Builder for [`NodeAttr`].
///
/// `name` and `label` are taken in `new()`. `attr_type` defaults to `NodeAttrType::Text`.
pub struct NodeAttrBuilder {
    name: String,
    attr_type: NodeAttrType,
    label: String,
    required: bool,
    default_value: Option<serde_json::Value>,
    options: Vec<SelectOption>,
}

impl NodeAttrBuilder {
    pub fn new(name: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            label: label.into(),
            attr_type: NodeAttrType::Text,
            required: false,
            default_value: None,
            options: Vec::new(),
        }
    }

    pub fn attr_type(mut self, t: NodeAttrType) -> Self {
        self.attr_type = t;
        self
    }

    pub fn required(mut self, r: bool) -> Self {
        self.required = r;
        self
    }

    pub fn default_value(mut self, v: serde_json::Value) -> Self {
        self.default_value = Some(v);
        self
    }

    pub fn options(mut self, o: Vec<SelectOption>) -> Self {
        self.options = o;
        self
    }

    pub fn build(self) -> NodeAttr {
        NodeAttr {
            name: self.name,
            attr_type: self.attr_type,
            label: self.label,
            required: self.required,
            default_value: self.default_value,
            options: self.options,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::richtext::NodeAttrType;

    #[test]
    fn builds_node_attr_with_defaults() {
        let attr = NodeAttrBuilder::new("href", "Link URL").build();
        assert_eq!(attr.name, "href");
        assert_eq!(attr.label, "Link URL");
        assert_eq!(attr.attr_type, NodeAttrType::Text);
        assert!(!attr.required);
        assert!(attr.default_value.is_none());
        assert!(attr.options.is_empty());
    }

    #[test]
    fn builds_node_attr_with_all_fields() {
        let attr = NodeAttrBuilder::new("size", "Size")
            .attr_type(NodeAttrType::Select)
            .required(true)
            .default_value(serde_json::json!("medium"))
            .build();
        assert_eq!(attr.attr_type, NodeAttrType::Select);
        assert!(attr.required);
        assert_eq!(attr.default_value, Some(serde_json::json!("medium")));
    }
}
