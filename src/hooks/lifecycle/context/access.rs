//! Context passed to collection-level and field-level access hooks.

use serde::Serialize;

use crate::core::{Document, DocumentFields};
use crate::typegen::lua::LuaAnnotation;

/// Context passed to collection- and field-level access functions.
/// Return `true` to allow, `false` / `nil` to deny, or a filter table
/// (read only) to allow with query constraints.
#[derive(Serialize, LuaAnnotation)]
#[lua(class = "crap.AccessContext")]
pub struct AccessContext<'a> {
    /// Full user document from the auth collection (nil if anonymous).
    /// Typed as `crap.AuthUser` (a `crap.Document` variant with an
    /// `[string] any` index signature) so access functions can read
    /// `context.user.role` / `context.user.email` etc. without
    /// per-call casts — the static type can't narrow to a specific
    /// auth-collection doc since projects may have multiple auth
    /// collections. Users who know their auth collection can still
    /// cast: `local u = context.user --[[@as crap.doc.Users]]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(ty = "crap.AuthUser", optional)]
    pub user: Option<&'a Document>,
    /// Document ID (for `update` / `delete` / `find_by_id`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<&'a str>,
    /// Incoming data (for `create` / `update`).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(ty = "table<string, any>", optional)]
    pub data: Option<&'a DocumentFields>,
}
