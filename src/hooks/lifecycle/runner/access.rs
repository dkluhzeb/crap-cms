//! `HookRunner` methods for access control.

use anyhow::Result;
use serde_json::Map;
use tracing::error;

use super::vm_pool::reset_instruction_budget;
use crate::{
    core::{Document, DocumentFields, FieldDefinition, FieldDenial, HookRef},
    db::{AccessResult, DbConnection},
    hooks::{
        HookRunner,
        lifecycle::{
            AccessCheckInput,
            access::{
                ReadStripInput, WriteStripInput, check_collection_access, collect_denials_flat,
                collect_read_denied_with_lua, has_any_field_access, strip_access_data_aware,
                strip_read_access_data_aware, strip_read_access_with_lua,
                strip_write_access_with_lua,
            },
            types::TxContextGuard,
        },
    },
};

impl HookRunner {
    /// Run a collection-level or global-level access check.
    ///
    /// `access_ref` is the Lua function ref (e.g., "`hooks.access.admin_only`").
    /// If `None`, access is allowed (no restriction configured).
    /// The function receives `{ user = ..., id = ..., data = ... }` and returns:
    /// - `true` → Allowed
    /// - `false` / `nil` → Denied
    /// - `table` → Constrained (read only: additional WHERE filters)
    ///
    /// # Errors
    ///
    /// Returns an error if VM acquisition or the access function call fails.
    pub fn check_access(
        &self,
        input: &AccessCheckInput<'_>,
        conn: &dyn DbConnection,
    ) -> Result<AccessResult> {
        // No access function configured — the in-Lua path would
        // only read the `default_deny` flag from the `LuaVmInfra` app-data and
        // return immediately, so skip the entire VM round-trip.
        // With pool size 16 and 50 concurrent reads, the previous
        // unconditional `pool.acquire()` serialized 34 requests on
        // the VM-pool mutex per tick (was 26% of total CPU spent in
        // futex syscalls). The cached `default_deny` flag is set
        // at builder time from `[access] default_deny`.
        if input.access.is_none() {
            return Ok(if self.default_deny {
                AccessResult::Denied
            } else {
                AccessResult::Allowed
            });
        }

        let lua = self.pool.acquire()?;
        let _guard = TxContextGuard::set(&lua, conn, None, None, None);

        // All access-constraint validation (operators, system columns, dotted
        // paths, locale-scoped fields) lives in `check_collection_access` — the
        // single chokepoint every access-resolving surface passes through.
        check_collection_access(&lua, input)
    }

    /// Data-aware field-**read** strip for pool surfaces (admin, gRPC, MCP):
    /// acquire a VM with `conn` threaded (so a data-dependent access fn may do
    /// CRUD), then strip read-denied fields from `level` in place. `document` is
    /// the full document exposed as `ctx.document`; each field's own immediate
    /// level is `ctx.data`. Skips VM acquisition entirely when no field carries
    /// an `access.read` function.
    ///
    /// Fail-closed: if the Lua VM pool is exhausted, every read-access-controlled
    /// field (at any depth) is stripped rather than silently retained.
    pub fn strip_read_access(
        &self,
        fields: &[FieldDefinition],
        level: &mut Map<String, serde_json::Value>,
        input: &ReadStripInput<'_>,
        conn: &dyn DbConnection,
    ) {
        if !has_any_field_access(fields, |f| f.access.read.as_ref()) {
            return;
        }

        let lua = match self.pool.acquire() {
            Ok(l) => l,
            Err(e) => {
                error!("Lua VM pool exhausted during field read access strip: {e}");

                // Fail closed: deny every read-access-controlled field, at any
                // depth, using the same data-aware walker with a constant-deny rule.
                strip_read_access_data_aware(fields, level, &|_hook, _data| true);

                return;
            }
        };

        let _guard = TxContextGuard::set(&lua, conn, None, None, None);

        strip_read_access_with_lua(&lua, fields, level, input);
    }

    /// Batched [`strip_read_access`](Self::strip_read_access) for a list read:
    /// acquire the Lua VM and set the `TxContext` **once** for the whole batch,
    /// then strip each document under that single lease. This is the documented
    /// per-query perf model (one VM held across all docs); only the in-VM Lua
    /// eval is per-doc — the per-document pool-mutex + `TxContext` churn is gone.
    /// Each document still gets its own `ctx.document` (the full pre-strip doc)
    /// and per-row `ctx.data`. Skips VM acquisition when no field carries an
    /// `access.read` function.
    ///
    /// Fail-closed: if the Lua VM pool is exhausted, every read-access-controlled
    /// field (at any depth) is stripped from every document in the batch.
    pub fn strip_read_access_batch(
        &self,
        fields: &[FieldDefinition],
        docs: &mut [Document],
        collection: &str,
        user: Option<&Document>,
        locale: Option<&str>,
        conn: &dyn DbConnection,
    ) {
        if docs.is_empty() || !has_any_field_access(fields, |f| f.access.read.as_ref()) {
            return;
        }

        let lua = match self.pool.acquire() {
            Ok(l) => Some(l),
            Err(e) => {
                error!("Lua VM pool exhausted during batched field read access strip: {e}");

                None
            }
        };

        // Hold the VM + TxContext for the entire batch (set once). When the pool
        // is exhausted `lua` is None and every doc fails closed below.
        let lua = lua.as_ref();
        let _guard = lua.map(|l| TxContextGuard::set(l, conn, None, None, None));

        for doc in docs.iter_mut() {
            // One instruction budget per document, as for a single strip.
            if let Some(l) = lua {
                reset_instruction_budget(l);
            }

            let document = doc.fields.clone();
            let mut level: Map<String, serde_json::Value> = std::mem::take(&mut doc.fields)
                .into_inner()
                .into_iter()
                .collect();

            match lua {
                Some(l) => {
                    let input = ReadStripInput {
                        document: &document,
                        collection,
                        user,
                        locale,
                    };
                    strip_read_access_with_lua(l, fields, &mut level, &input);
                }
                None => strip_read_access_data_aware(fields, &mut level, &|_hook, _data| true),
            }

            doc.fields = level.into_iter().collect();
        }
    }

    /// Data-aware field-**write** strip (create/update) for pool surfaces:
    /// acquire a VM with `conn` threaded, then remove from `level` every field
    /// the user may not write under `operation`. `document` is the full incoming
    /// document (`ctx.document`); each field's own level is `ctx.data`. Skips VM
    /// acquisition when no field configures the relevant write-access function.
    ///
    /// Fail-closed: VM-pool exhaustion strips every write-access-controlled field.
    pub fn strip_write_access(
        &self,
        fields: &[FieldDefinition],
        level: &mut Map<String, serde_json::Value>,
        input: &WriteStripInput<'_>,
        conn: &dyn DbConnection,
    ) {
        let Some(extract) = FieldDefinition::write_access_extractor(input.operation) else {
            return;
        };

        if !has_any_field_access(fields, extract) {
            return;
        }

        let lua = match self.pool.acquire() {
            Ok(l) => l,
            Err(e) => {
                error!("Lua VM pool exhausted during field write access strip: {e}");

                strip_access_data_aware(fields, level, &extract, &|_hook, _data| true);

                return;
            }
        };

        let _guard = TxContextGuard::set(&lua, conn, None, None, None);

        strip_write_access_with_lua(&lua, fields, level, input);
    }

    /// Data-aware field-**read** strip for the **live event** path (gRPC
    /// subscribe, admin SSE): like [`strip_read_access`](Self::strip_read_access)
    /// but **connection-less** — the event pipeline has no DB transaction, so a
    /// VM is acquired without a [`TxContextGuard`]. Pure `access.read` rules
    /// (the overwhelming majority) work; a rule that performs CRUD raises and is
    /// treated as denied, matching the connection-less event `after_read`
    /// contract. `document` is the event's full document (`ctx.document`).
    ///
    /// Fail-closed: VM-pool exhaustion strips every read-access-controlled field.
    pub fn strip_read_access_for_event(
        &self,
        fields: &[FieldDefinition],
        level: &mut Map<String, serde_json::Value>,
        document: &DocumentFields,
        collection: &str,
        user: Option<&Document>,
    ) {
        if !has_any_field_access(fields, |f| f.access.read.as_ref()) {
            return;
        }

        let lua = match self.pool.acquire() {
            Ok(l) => l,
            Err(e) => {
                error!("Lua VM pool exhausted during live-event field read strip: {e}");

                strip_read_access_data_aware(fields, level, &|_hook, _data| true);

                return;
            }
        };

        let input = ReadStripInput {
            document,
            collection,
            user,
            locale: None,
        };
        strip_read_access_with_lua(&lua, fields, level, &input);
    }

    /// Data-aware collection of read-denied field **names** for a single
    /// `document` — for surfaces that need denial paths rather than an in-place
    /// value strip (the admin form's input dropping, the
    /// `crap.access.field_read_denied` introspection API). Skips VM acquisition
    /// when no field configures read access.
    ///
    /// Fail-closed: if the Lua VM pool is exhausted, every read-access-controlled
    /// field (at any depth) is reported denied.
    pub fn read_denied_names(
        &self,
        fields: &[FieldDefinition],
        document: &DocumentFields,
        collection: &str,
        user: Option<&Document>,
        locale: Option<&str>,
        conn: &dyn DbConnection,
    ) -> Vec<FieldDenial> {
        if !has_any_field_access(fields, |f| f.access.read.as_ref()) {
            return Vec::new();
        }

        let lua = match self.pool.acquire() {
            Ok(l) => l,
            Err(e) => {
                error!("Lua VM pool exhausted during field read denial computation: {e}");

                return deny_all_access_controlled(fields, |f| f.access.read.as_ref());
            }
        };

        let _guard = TxContextGuard::set(&lua, conn, None, None, None);

        collect_read_denied_with_lua(&lua, fields, document, collection, user, locale)
    }
}

/// Fail-closed denial set when the Lua VM pool is unavailable: deny EVERY
/// access-controlled field, at any depth (including inside array/blocks rows).
/// Uses the shared [`collect_denials_flat`] walker so the fail-closed path can
/// never diverge from the normal Lua-evaluated path's container recursion.
fn deny_all_access_controlled(
    fields: &[FieldDefinition],
    extractor: impl Fn(&FieldDefinition) -> Option<&HookRef> + Copy,
) -> Vec<FieldDenial> {
    let is_denied = |field: &FieldDefinition| extractor(field).is_some();

    let mut denied = Vec::new();
    collect_denials_flat(fields, &is_denied, "", &mut denied);

    denied
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{FieldAccess, FieldTab, FieldType};
    fn make_field(name: &str, access: FieldAccess) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text)
            .access(access)
            .build()
    }

    #[test]
    fn deny_all_finds_top_level() {
        let fields = vec![make_field(
            "secret",
            FieldAccess {
                read: Some(HookRef::new("hooks.deny")),
                ..Default::default()
            },
        )];
        let denied = deny_all_access_controlled(&fields, |f| f.access.read.as_ref());
        assert_eq!(denied, vec![FieldDenial::Flat("secret".into())]);
    }

    #[test]
    fn deny_all_recurses_into_group_with_prefix() {
        let fields = vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![make_field(
                    "title",
                    FieldAccess {
                        read: Some(HookRef::new("hooks.deny")),
                        ..Default::default()
                    },
                )])
                .build(),
        ];
        let denied = deny_all_access_controlled(&fields, |f| f.access.read.as_ref());
        assert_eq!(denied, vec![FieldDenial::Flat("seo__title".into())]);
    }

    #[test]
    fn deny_all_recurses_into_tabs() {
        let fields = vec![
            FieldDefinition::builder("layout", FieldType::Tabs)
                .tabs(vec![FieldTab::new(
                    "Main",
                    vec![make_field(
                        "hidden",
                        FieldAccess {
                            read: Some(HookRef::new("hooks.deny")),
                            ..Default::default()
                        },
                    )],
                )])
                .build(),
        ];
        let denied = deny_all_access_controlled(&fields, |f| f.access.read.as_ref());
        assert_eq!(denied, vec![FieldDenial::Flat("hidden".into())]);
    }

    #[test]
    fn deny_all_empty_when_no_access_configured() {
        let fields = vec![
            make_field("title", FieldAccess::default()),
            make_field("body", FieldAccess::default()),
        ];
        let denied = deny_all_access_controlled(&fields, |f| f.access.read.as_ref());
        assert!(denied.is_empty());
    }
}
