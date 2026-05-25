//! Context passed to per-field hook functions
//! (`before_validate`, `before_change`, `after_change`, `after_read`).

use serde::Serialize;

use crate::core::{Document, DocumentFields};
use crate::typegen::lua::LuaAnnotation;

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
    /// Full document data (read-only snapshot).
    #[lua(ty = "table<string, any>")]
    pub data: &'a DocumentFields,
    /// Authenticated user document (nil if unauthenticated or no auth
    /// collection).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(ty = "table", optional)]
    pub user: Option<&'a Document>,
    /// Admin UI locale code (e.g., `"en"`, `"de"`). Nil if not set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ui_locale: Option<&'a str>,
}
