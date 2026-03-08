use serde::{Deserialize, Serialize};

use crate::core::field::SelectOption;
use super::node_attr_builder::NodeAttrBuilder;

/// Attribute type for custom node attributes (maps to form input type in admin).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum NodeAttrType {
    Text,
    Number,
    Select,
    Checkbox,
    Textarea,
}

impl NodeAttrType {
    pub fn as_str(&self) -> &'static str {
        match self {
            NodeAttrType::Text => "text",
            NodeAttrType::Number => "number",
            NodeAttrType::Select => "select",
            NodeAttrType::Checkbox => "checkbox",
            NodeAttrType::Textarea => "textarea",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "number" => NodeAttrType::Number,
            "select" => NodeAttrType::Select,
            "checkbox" => NodeAttrType::Checkbox,
            "textarea" => NodeAttrType::Textarea,
            _ => NodeAttrType::Text,
        }
    }
}

/// A single attribute on a custom richtext node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeAttr {
    pub name: String,
    pub attr_type: NodeAttrType,
    pub label: String,
    pub required: bool,
    #[serde(default)]
    pub default_value: Option<serde_json::Value>,
    #[serde(default)]
    pub options: Vec<SelectOption>,
}

impl NodeAttr {
    pub fn builder(name: impl Into<String>, label: impl Into<String>) -> NodeAttrBuilder {
        NodeAttrBuilder::new(name, label)
    }
}
