//! Custom `ProseMirror` node types for richtext fields.
//!
//! Provides data model types for defining custom structured nodes (CTAs, embeds,
//! alerts, etc.) that can be embedded inside richtext content. Also includes a
//! `ProseMirror` JSON → HTML renderer that handles both standard PM nodes and
//! custom nodes via a callback.

pub mod crap_node;
pub mod node_def;
pub mod node_name;
pub mod renderer;

pub use crap_node::{CrapNodeTag, decode_entities, find_crap_nodes};
pub use node_def::{RichtextNodeDef, RichtextNodeDefBuilder};
pub use node_name::{RESERVED_NODE_NAMES, validate_node_name};
pub use renderer::{render_html_custom_nodes, render_prosemirror_to_html};
