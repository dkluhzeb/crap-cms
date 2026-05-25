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
/// - `read` — can the user view the collection at all.
/// - `create` — can the user create new items.
/// - `update` — can the user update existing items (drives the Save /
///   Publish / Save Draft / Unpublish row in the edit sidebar).
/// - `delete` — can the user *hard*-delete items (drives "Empty Trash"
///   and per-row "Delete permanently" buttons).
/// - `trash` — can the user soft-delete items. Only meaningful for
///   collections with `soft_delete = true`. When `def.access.trash` is
///   unset, falls back to `delete` (the legacy combined semantics).
#[derive(Debug, Serialize, JsonSchema, Default)]
pub struct CollectionPermissions {
    pub read: bool,
    pub create: bool,
    pub update: bool,
    pub delete: bool,
    pub trash: bool,
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

        let Ok(mut conn) = state.pool.get() else {
            return Self::default();
        };
        let Ok(tx) = conn.transaction() else {
            return Self::default();
        };

        let read = has_access_with_conn(state, def.access.read.as_deref(), user_doc, &tx);
        let create = has_access_with_conn(state, def.access.create.as_deref(), user_doc, &tx);
        let update = has_access_with_conn(state, def.access.update.as_deref(), user_doc, &tx);
        let delete = has_access_with_conn(state, def.access.delete.as_deref(), user_doc, &tx);
        let trash = if def.soft_delete {
            match def.access.trash.as_deref() {
                Some(_) => has_access_with_conn(state, def.access.trash.as_deref(), user_doc, &tx),
                // No explicit `access.trash` configured → soft-delete is gated
                // by the same fn as hard-delete (legacy behaviour).
                None => delete,
            }
        } else {
            false
        };

        let _ = tx.commit();

        Self {
            read,
            create,
            update,
            delete,
            trash,
        }
    }
}

/// Per-user permissions for a global page. Globals only have `read` and
/// `update` access — no create/delete (the row always exists).
#[derive(Debug, Serialize, JsonSchema, Default)]
pub struct GlobalPermissions {
    pub read: bool,
    pub update: bool,
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

        let Ok(mut conn) = state.pool.get() else {
            return Self::default();
        };
        let Ok(tx) = conn.transaction() else {
            return Self::default();
        };

        let read = has_access_with_conn(state, def.access.read.as_deref(), user_doc, &tx);
        let update = has_access_with_conn(state, def.access.update.as_deref(), user_doc, &tx);

        let _ = tx.commit();

        Self { read, update }
    }
}
