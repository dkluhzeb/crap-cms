//! Context passed to per-field hook functions
//! (`before_validate`, `before_change`, `after_change`, `after_read`).

use serde::Serialize;
use serde_json::Value;

use crate::{
    core::{Document, DocumentFields},
    typegen::lua::LuaAnnotation,
};

/// Context passed to per-field hook functions.
#[derive(Serialize, LuaAnnotation)]
#[lua(class = "crap.FieldHookContext")]
pub struct FieldHookContext<'a> {
    /// Name of the field being processed.
    pub field_name: &'a str,
    /// Collection slug.
    pub collection: &'a str,
    /// The operation: `"create"`, `"update"`, `"find"`, `"find_by_id"`,
    /// …
    pub operation: &'a str,
    /// The id of the document being processed on `update` / read; `nil` on
    /// `create` (no row exists yet). Mirrors the `id` on the validator and
    /// access contexts.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(ty = "string", optional)]
    pub id: Option<&'a str>,
    /// Content locale for this operation (e.g. `"en"`, `"de"`). Nil when the
    /// operation is not locale-scoped. Distinct from `ui_locale` (the admin UI
    /// language) — this is the locale of the data being written or read.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(ty = "string", optional)]
    pub locale: Option<&'a str>,
    /// The **nearest scope** (read-only snapshot): the full document for a
    /// top-level field, or the current row for a hook on a field inside an
    /// array/blocks row.
    #[lua(ty = "table<string, any>")]
    pub data: &'a DocumentFields,
    /// The **full document** being written or read — a read-only snapshot taken
    /// before any field hook in this pass ran (so it does not reflect changes
    /// made by earlier field hooks in the same pass). Matches `data` at the top
    /// level; for a sub-field hook inside an array/blocks row it's the parent
    /// document, so the hook can cross-reference fields outside its row.
    #[lua(ty = "table<string, any>")]
    pub document: &'a DocumentFields,
    /// Authenticated user document (nil if unauthenticated or no auth
    /// collection).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(ty = "table", optional)]
    pub user: Option<&'a Document>,
    /// Admin UI locale code (e.g., `"en"`, `"de"`). Nil if not set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ui_locale: Option<&'a str>,
    /// Per-config options from this field hook's `{ ref, options }` table; `nil`
    /// when the hook was configured as a bare ref string.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(ty = "table", optional)]
    pub options: Option<&'a Value>,
}
