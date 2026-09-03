//! Context passed to a collection's `live.filter` broadcast-gate function.

use serde::Serialize;
use serde_json::Value;

use crate::core::{DocumentFields, event::EventUser};
use crate::typegen::lua::LuaAnnotation;

/// Context passed to the `live.filter` function — the per-collection gate that
/// decides whether a committed mutation is broadcast to live subscribers.
///
/// Runs **after** the write commits, with no CRUD/transaction access. The
/// function returns `true` to broadcast, `false`/`nil` to suppress.
#[derive(Serialize, LuaAnnotation)]
#[lua(class = "crap.LiveFilterContext")]
pub struct LiveFilterContext<'a> {
    /// Collection (or global) slug the mutation targets.
    pub collection: &'a str,
    /// The mutation operation: `"create"`, `"update"`, `"delete"`,
    /// `"undelete"`, `"unpublish"`, or `"restore"`.
    pub operation: &'a str,
    /// The document's field data for this event.
    #[lua(ty = "table<string, any>")]
    pub data: &'a DocumentFields,
    /// The affected document's id, exposed to Lua as `ctx.id` (consistent with
    /// the other hook contexts; the serialized event payload uses `document_id`).
    #[serde(rename = "id")]
    #[lua(rename = "id")]
    pub document_id: &'a str,
    /// The user who caused the mutation (`nil` for anonymous/system changes).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(ty = "{ id: string, email: string }", optional)]
    pub edited_by: Option<&'a EventUser>,
    /// Per-config options from the filter's `{ ref, options }` table; `nil` when
    /// configured as a bare ref string.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(ty = "table", optional)]
    pub options: Option<&'a Value>,
}
