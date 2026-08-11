//! Shared `ExportFile` shape for the `export` / `import` commands.
//!
//! The envelope (`crap_version`, `exported_at`, `collections`) is fixed
//! shape; per-document payloads inside `collections.<slug>` stay free-form
//! (they carry the document's user-defined fields).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Structural version of the export format. Bump ONLY on a
/// backward-incompatible change. `import` refuses an export whose
/// `format_version` is newer than this binary understands; an equal-or-older
/// version (incl. a pre-versioning export that omits the field → 1) is
/// accepted.
pub(crate) const EXPORT_FORMAT_VERSION: u32 = 1;

fn default_format_version() -> u32 {
    1
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ExportFile {
    /// Export-format structural version (see [`EXPORT_FORMAT_VERSION`]).
    #[serde(default = "default_format_version")]
    pub format_version: u32,
    pub crap_version: String,
    pub exported_at: String,
    /// Map of collection slug → array of exported document JSON.
    /// The inner shape is `Document` serialized via serde, but the values
    /// inside `Document.fields` are user-defined per collection schema, so
    /// the leaf stays `Value`.
    pub collections: Map<String, Value>,
}
