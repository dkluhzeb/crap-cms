//! Custom `ProseMirror` node types for richtext fields.
//!
//! Provides data model types for defining custom structured nodes (CTAs, embeds,
//! alerts, etc.) that can be embedded inside richtext content, the document
//! model a JSON-format field accepts, plain-text extraction for both storage
//! formats, and a `ProseMirror` JSON → HTML renderer that handles both standard
//! PM nodes and custom nodes via a callback.

pub mod crap_node;
pub mod document;
mod html_lex;
pub mod node_def;
pub mod node_name;
pub mod renderer;
pub mod text;

pub use crap_node::{CrapNodeTag, decode_entities, find_crap_nodes};
pub use document::{DocumentError, RICHTEXT_FEATURES, RichtextSchema, parse_document};
pub use node_def::{RichtextNodeDef, RichtextNodeDefBuilder};
pub use node_name::{RESERVED_NODE_NAMES, validate_node_name};
pub use renderer::{
    render_html_custom_nodes, render_prosemirror_document, render_prosemirror_to_html,
};
pub use text::{SearchableAttrs, document_text, html_text, richtext_is_blank, richtext_plain_text};
