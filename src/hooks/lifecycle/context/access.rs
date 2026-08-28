//! Context passed to collection-level and field-level access hooks.

use serde::Serialize;
use serde_json::Value;

use crate::core::{Document, DocumentFields, HookRef};
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
    /// The data for this check.
    ///
    /// - **Collection access:** the **incoming** data for `create` / `update`
    ///   (the submitted change, *not* the stored row), `nil` for reads/deletes.
    ///   To gate on existing persisted values, return a **filter table** instead
    ///   of a boolean (e.g. `return { author_id = ctx.user.id }`).
    /// - **Field access:** the field's **immediate level** — the row object for a
    ///   field inside an array/blocks row, the group object for a field in a
    ///   group, the whole document at top level. Same meaning as `ctx.data` in a
    ///   field lifecycle hook, so a field rule can gate on adjacent (sibling)
    ///   values — e.g. lock a field unless `ctx.data.kind == "advanced"`. Present
    ///   on reads too (the level being read).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(ty = "table<string, any>", optional)]
    pub data: Option<&'a DocumentFields>,
    /// **Field access only.** The full document the field belongs to (the stored
    /// document on a read/`update`; the full incoming document on `create`),
    /// stable as the check descends into array/blocks rows — mirrors
    /// `ctx.document` in a field lifecycle hook. Lets a field rule depend on
    /// values outside its own level, e.g. make a field read-only unless
    /// `ctx.document.status == "published"`. `nil` for collection-level checks
    /// (use a filter table there instead).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(ty = "table<string, any>", optional)]
    pub document: Option<&'a DocumentFields>,
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
    /// Per-config options from the hook ref's `{ ref, options }` table; `nil`
    /// when the access rule was configured as a bare ref string.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(ty = "table", optional)]
    pub options: Option<&'a Value>,
}

/// Bundled inputs for an access check (`HookRunner::check_access`,
/// `check_access_with_lua`, and the `ReadHooks`/`WriteHooks::check_access`
/// trait methods). Grouped into a struct so the access-check signature stays
/// within the argument-count budget as `operation`/`collection` were added.
///
/// Construct via [`AccessCheckInput::builder`] — `operation` and `collection`
/// are always required; every other field defaults to `None` and is set only
/// when relevant. The builder is the single construction path so adding a new
/// optional input is a one-line change here, not an edit at every call site.
pub struct AccessCheckInput<'a> {
    /// The access hook ref (a bare ref or a `{ ref, options }` table), or `None`
    /// when no access function is configured (default-allow / default-deny
    /// applies). The ref's `options`, if any, reach the function as
    /// `ctx.options`.
    pub access: Option<&'a HookRef>,
    pub user: Option<&'a Document>,
    pub id: Option<&'a str>,
    /// Collection access: incoming write data (or `None`). Field access: the
    /// field's immediate level. See [`AccessContext::data`].
    pub data: Option<&'a DocumentFields>,
    /// Field-access only: the full document. See [`AccessContext::document`].
    pub document: Option<&'a DocumentFields>,
    pub locale: Option<&'a str>,
    pub operation: &'a str,
    pub collection: &'a str,
    pub ui_locale: Option<&'a str>,
    /// Whether the *operation itself* injects `_status` (a non-draft write on
    /// a drafts-enabled collection). When set, a `Constrained` result may
    /// filter on `_status` — the constraint validation chokepoint
    /// (`check_collection_access`) reads this instead of hard-coding `false`,
    /// so the allowance works on every surface that resolves access.
    pub injecting_status: bool,
}

impl<'a> AccessCheckInput<'a> {
    /// Start building an access-check input for the given `operation` and
    /// `collection` (the only required fields). All other fields default to
    /// `None`; chain the setters for the ones the call site cares about, then
    /// [`build`](AccessCheckInputBuilder::build).
    #[must_use]
    pub fn builder(operation: &'a str, collection: &'a str) -> AccessCheckInputBuilder<'a> {
        AccessCheckInputBuilder {
            access: None,
            user: None,
            id: None,
            data: None,
            document: None,
            locale: None,
            operation,
            collection,
            ui_locale: None,
            injecting_status: false,
        }
    }
}

/// Builder for [`AccessCheckInput`]. Optional setters take `Option<&T>` so a
/// caller's already-optional value (`ctx.user`, `input.ui_locale.as_deref()`,
/// …) flows straight through without an `if let`.
pub struct AccessCheckInputBuilder<'a> {
    access: Option<&'a HookRef>,
    user: Option<&'a Document>,
    id: Option<&'a str>,
    data: Option<&'a DocumentFields>,
    document: Option<&'a DocumentFields>,
    locale: Option<&'a str>,
    operation: &'a str,
    collection: &'a str,
    ui_locale: Option<&'a str>,
    injecting_status: bool,
}

impl<'a> AccessCheckInputBuilder<'a> {
    /// The access hook ref to evaluate (`None` ⇒ default-allow / default-deny).
    #[must_use]
    pub fn access(mut self, access: Option<&'a HookRef>) -> Self {
        self.access = access;
        self
    }

    /// The acting user document (`None` ⇒ anonymous).
    #[must_use]
    pub fn user(mut self, user: Option<&'a Document>) -> Self {
        self.user = user;
        self
    }

    /// The target document id (`update` / `delete` / `find_by_id`).
    #[must_use]
    pub fn id(mut self, id: Option<&'a str>) -> Self {
        self.id = id;
        self
    }

    /// Collection access: incoming write data. Field access: the field's
    /// immediate level. See [`AccessContext::data`].
    #[must_use]
    pub fn data(mut self, data: Option<&'a DocumentFields>) -> Self {
        self.data = data;
        self
    }

    /// Field-access only: the full document. See [`AccessContext::document`].
    #[must_use]
    pub fn document(mut self, document: Option<&'a DocumentFields>) -> Self {
        self.document = document;
        self
    }

    /// The content locale this operation targets (`None` ⇒ localization off).
    #[must_use]
    pub fn locale(mut self, locale: Option<&'a str>) -> Self {
        self.locale = locale;
        self
    }

    /// The operator's admin UI locale (`None` ⇒ non-admin origin).
    #[must_use]
    pub fn ui_locale(mut self, ui_locale: Option<&'a str>) -> Self {
        self.ui_locale = ui_locale;
        self
    }

    /// Whether the operation itself injects `_status` (permits `_status`
    /// constraints from the access hook). See [`AccessCheckInput::injecting_status`].
    #[must_use]
    pub fn injecting_status(mut self, injecting_status: bool) -> Self {
        self.injecting_status = injecting_status;
        self
    }

    /// Finalize the [`AccessCheckInput`].
    #[must_use]
    pub fn build(self) -> AccessCheckInput<'a> {
        AccessCheckInput {
            access: self.access,
            user: self.user,
            id: self.id,
            data: self.data,
            document: self.document,
            locale: self.locale,
            operation: self.operation,
            collection: self.collection,
            ui_locale: self.ui_locale,
            injecting_status: self.injecting_status,
        }
    }
}
