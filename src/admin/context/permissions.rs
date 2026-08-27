//! Per-user permission flags for collection / global pages — drives
//! UI gating ("show the Create button only if the user can create").
//!
//! Computed in the handler with one shared transaction across all checks,
//! then passed to the template under `{{perms.*}}`. Server-side enforcement
//! still runs its own `check_access_or_forbid` on every write — these flags
//! only suppress UI elements that would always 403.

use axum::Extension;
use schemars::JsonSchema;
use serde::Serialize;

use crate::{
    admin::{
        AdminState,
        handlers::shared::{get_user_doc, has_access_with_conn},
    },
    core::{AuthUser, CollectionDefinition, GlobalDefinition},
};

/// Per-user permissions for a collection page.
///
/// Field semantics:
/// - `read` — can the user view the collection at all. This is the **union**
///   of the document-list views the read path serves (published ∪ draft ∪
///   trash, each resolved with its fallback), matching the service's
///   union-and-downgrade read model — a user allowed only drafts (or only
///   trash) can still view the collection, so `read` is `true` for them.
/// - `create` — can the user create new items.
/// - `update` — can the user update existing items (drives the Save /
///   Publish / Save Draft / Unpublish row in the edit sidebar).
/// - `delete` — can the user *hard*-delete items (drives "Empty Trash"
///   and per-row "Delete permanently" buttons).
/// - `trash` — can the user soft-delete items. Only meaningful for
///   collections with `soft_delete = true`. When `def.access.trash` is
///   unset, falls back to `update` (via `resolve_trash()`, matching the
///   soft-delete enforcement path).
#[derive(Debug, Serialize, JsonSchema, Default)]
pub struct CollectionPermissions {
    pub read: bool,
    pub create: bool,
    pub update: bool,
    pub delete: bool,
    pub trash: bool,
    /// Whether the user may view draft (unpublished) content — gated on
    /// `resolve_draft()` (`access.draft`, or `access.update` as the fallback),
    /// mirroring what the read paths enforce. A pure UI hint (e.g. a Drafts
    /// tab): the read paths request every view unconditionally and the service
    /// downgrades per access, so this never gates the request itself.
    pub draft: bool,
    /// Whether the user may view version history — gated on `resolve_versions()`
    /// (`access.versions`, or `access.update` as the fallback), matching the
    /// service gate. Drives whether the version-history sidebar panel is shown.
    /// Only meaningful when `versions` is enabled. A UI hint only; the service
    /// enforces the real gate.
    pub versions: bool,
}

impl CollectionPermissions {
    /// Compute all permission flags for `auth_user` against `def`. Opens
    /// a single transaction, runs each `check_access` against it, commits.
    /// Returns all-false if the connection pool is exhausted (fail-closed
    /// at the UI layer — the server will still enforce the real check).
    pub fn for_user(
        state: &AdminState,
        def: &CollectionDefinition,
        auth_user: Option<&Extension<AuthUser>>,
    ) -> Self {
        let user_doc = get_user_doc(auth_user);

        let Ok(mut conn) = state.infra.pool.get() else {
            return Self::default();
        };
        let Ok(tx) = conn.transaction() else {
            return Self::default();
        };

        let read_published = has_access_with_conn(
            state,
            def.access.read.as_ref(),
            user_doc,
            &tx,
            "read",
            &def.slug,
        );
        let create = has_access_with_conn(
            state,
            def.access.create.as_ref(),
            user_doc,
            &tx,
            "create",
            &def.slug,
        );
        let update = has_access_with_conn(
            state,
            def.access.update.as_ref(),
            user_doc,
            &tx,
            "update",
            &def.slug,
        );
        let delete = has_access_with_conn(
            state,
            def.access.delete.as_ref(),
            user_doc,
            &tx,
            "delete",
            &def.slug,
        );
        // Mirror the real soft-delete path: gate on `resolve_trash()` (the
        // `trash` fn, or `update` as the documented fallback) and report the
        // `"trash"` operation — so the grid's "can trash?" matches what the
        // server actually enforces on a soft delete.
        let trash = if def.soft_delete {
            has_access_with_conn(
                state,
                def.access.resolve_trash(),
                user_doc,
                &tx,
                "trash",
                &def.slug,
            )
        } else {
            false
        };

        // Mirror the read paths' draft gate: viewing drafts is gated on
        // `resolve_draft()` (the `draft` fn, or `update` as the fallback), with
        // the `"find"` operation the list path reports.
        let draft = if def.has_drafts() {
            has_access_with_conn(
                state,
                def.access.resolve_draft(),
                user_doc,
                &tx,
                "find",
                &def.slug,
            )
        } else {
            false
        };

        // Mirror the service version gate: `versions ?? update` — when the
        // toggle is unset, history follows edit access.
        let versions = def.has_versions()
            && has_access_with_conn(
                state,
                def.access.resolve_versions(),
                user_doc,
                &tx,
                "find",
                &def.slug,
            );

        let _ = tx.commit();

        // "Can view the collection at all" = the union of the document-list
        // views the read path serves (published ∪ draft ∪ trash). Versions is a
        // per-document history, gated separately by `versions`, and isn't part
        // of viewing the list.
        let read = read_published || draft || trash;

        Self {
            read,
            create,
            update,
            delete,
            trash,
            draft,
            versions,
        }
    }
}

/// Per-user permissions for a global page. Globals only have `read` and
/// `update` access — no create/delete (the row always exists).
#[derive(Debug, Serialize, JsonSchema, Default)]
pub struct GlobalPermissions {
    pub read: bool,
    pub update: bool,
    /// Whether the user may view the global's draft (unpublished) content —
    /// gated on `resolve_draft()` (`access.draft`, or `access.update`). A UI
    /// hint only — the read path requests drafts unconditionally and downgrades.
    pub draft: bool,
    /// Whether the user may view the global's version history — gated on
    /// `resolve_versions()` (`access.versions`, or `access.update`). Drives
    /// whether the version-history panel is shown. A UI hint only.
    pub versions: bool,
}

impl GlobalPermissions {
    /// Compute permission flags for `auth_user` against `def`. Shares the
    /// transaction-batching pattern of [`CollectionPermissions::for_user`].
    pub fn for_user(
        state: &AdminState,
        def: &GlobalDefinition,
        auth_user: Option<&Extension<AuthUser>>,
    ) -> Self {
        let user_doc = get_user_doc(auth_user);

        let Ok(mut conn) = state.infra.pool.get() else {
            return Self::default();
        };
        let Ok(tx) = conn.transaction() else {
            return Self::default();
        };

        let read_published = has_access_with_conn(
            state,
            def.access.read.as_ref(),
            user_doc,
            &tx,
            "read",
            &def.slug,
        );
        let update = has_access_with_conn(
            state,
            def.access.update.as_ref(),
            user_doc,
            &tx,
            "update",
            &def.slug,
        );

        // Mirror the global read path's draft gate: viewing the draft is gated
        // on `resolve_draft()`, with the `"get"` operation the read reports.
        let draft = if def.has_drafts() {
            has_access_with_conn(
                state,
                def.access.resolve_draft(),
                user_doc,
                &tx,
                "get",
                &def.slug,
            )
        } else {
            false
        };

        // Mirror the service version gate: `versions ?? update`.
        let versions = def.has_versions()
            && has_access_with_conn(
                state,
                def.access.resolve_versions(),
                user_doc,
                &tx,
                "get",
                &def.slug,
            );

        let _ = tx.commit();

        // "Can view the global at all" = published ∪ draft (globals have no
        // trash view). Versions is gated separately.
        let read = read_published || draft;

        Self {
            read,
            update,
            draft,
            versions,
        }
    }
}
