//! Custom `ProseMirror` node types for richtext fields.
//!
//! Provides data model types for defining custom structured nodes (CTAs, embeds,
//! alerts, etc.) that can be embedded inside richtext content. Also includes a
//! `ProseMirror` JSON → HTML renderer that handles both standard PM nodes and
//! custom nodes via a callback.

pub mod node_def;
pub mod renderer;

pub use node_def::{RichtextNodeDef, RichtextNodeDefBuilder};
pub use renderer::{render_html_custom_nodes, render_prosemirror_to_html};
