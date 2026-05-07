//! Shared `ExportFile` shape for the `export` / `import` commands.
//!
//! The envelope (`crap_version`, `exported_at`, `collections`) is fixed
//! shape; per-document payloads inside `collections.<slug>` stay free-form
//! (they carry the document's user-defined fields).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ExportFile {
    pub crap_version: String,
    pub exported_at: String,
    /// Map of collection slug → array of exported document JSON.
    /// The inner shape is `Document` serialized via serde, but the values
    /// inside `Document.fields` are user-defined per collection schema, so
    /// the leaf stays `Value`.
    pub collections: Map<String, Value>,
}
