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
    /// The **incoming** data for `create` / `update` (what is being written),
    /// `nil` for reads/deletes. This is the submitted change, *not* the existing
    /// stored row. To gate on existing persisted values (e.g. "users may only
    /// edit their own rows"), return a **filter table** instead of a boolean —
    /// e.g. `return { author_id = ctx.user.id }` — and the system enforces that
    /// the target row matches it.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(ty = "table<string, any>", optional)]
    pub data: Option<&'a DocumentFields>,
    /// The locale this operation targets, when localization is enabled —
    /// the requested locale, or the default locale when none was specified.
    /// `nil` when localization is disabled. Lets access functions enforce
    /// per-locale rules, e.g. restrict a user to certain locales or lock a
    /// field to the default locale. Also `nil` when the access function is
    /// invoked outside a single-locale operation (e.g. manually via
    /// `crap.access.check`, or a nested join read-access check) — gate
    /// defensively (`if ctx.locale and ... then`).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(optional)]
    pub locale: Option<&'a str>,
    /// The operation triggering this check: `"create"`, `"update"`, `"delete"`,
    /// `"trash"` (soft delete), `"undelete"`, `"unpublish"`, `"restore"`,
    /// `"find"`, `"find_by_id"`, `"count"`, `"search"`, `"get"` (global read),
    /// `"read"` (admin read-gating: nav, back-references, condition eval, upload
    /// serve), `"subscribe"`, … Lets one shared access function branch on the
    /// operation instead of registering a separate function per operation.
    pub operation: &'a str,
    /// The collection (or job) slug this check is for — so a function reused
    /// across collections can tell which one it is gating.
    pub collection: &'a str,
    /// Admin UI locale code (e.g. `"en"`, `"de"`) when the check originates from
    /// an admin request; `nil` otherwise (gRPC/REST/internal checks). Distinct
    /// from `locale` (the content locale) — this is the operator's UI language.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(optional)]
    pub ui_locale: Option<&'a str>,
}

/// Bundled inputs for an access check (`HookRunner::check_access`,
/// `check_access_with_lua`, and the `ReadHooks`/`WriteHooks::check_access`
/// trait methods). Grouped into a struct so the access-check signature stays
/// within the argument-count budget as `operation`/`collection` were added.
pub struct AccessCheckInput<'a> {
    /// The access function ref (`"module.fn"`), or `None` when no access
    /// function is configured (default-allow / default-deny applies).
    pub access_ref: Option<&'a str>,
    pub user: Option<&'a Document>,
    pub id: Option<&'a str>,
    pub data: Option<&'a DocumentFields>,
    pub locale: Option<&'a str>,
    pub operation: &'a str,
    pub collection: &'a str,
    pub ui_locale: Option<&'a str>,
}
