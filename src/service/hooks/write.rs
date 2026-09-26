//! `WriteHooks` trait and implementations for abstracting write hook execution
//! across different API surfaces (pool-based vs inline Lua VM).

mod lua;
mod runner;
mod shape;
mod snapshot_keep;
mod unreadable;

pub use lua::LuaWriteHooks;
pub use runner::RunnerWriteHooks;
pub use shape::SnapshotLocales;
pub use snapshot_keep::{SnapshotReadKeep, StoredByLocale};
pub use unreadable::UpdateStored;

use anyhow::Result;
use serde_json::{Map, Value};

use crate::{
    core::{
        Document, DocumentFields, FieldDefinition, Hooks, Registry, ValidationError,
        nest_group_fields,
    },
    db::{AccessResult, DbConnection},
    hooks::{
        HookContext, HookEvent, ValidationCtx,
        lifecycle::{
            AccessCheckInput,
            access::{WriteStripInput, has_any_field_access},
        },
    },
    service::hooks::FieldReadStrip,
};

use shape::{
    LeafShape, drop_locale_columns_of_stripped, drop_paths, prefill_checkboxes, removed_paths,
    restore_stripped_checkboxes, unfill_kept_checkboxes,
};
use snapshot_keep::keep_unreadable_snapshot;
use unreadable::{ReadJudge, keep_unreadable};

/// Local alias to disambiguate from the file-wide `anyhow::Result`.
type ValidateResult = std::result::Result<(), ValidationError>;

/// Whether an update's field strip judges the stored document: some field
/// carries an `access.update` rule (which judges the stored value), or an
/// `access.read` rule (a write never changes a value its writer cannot read).
/// The loaders of that stored document skip the read when this is false.
#[must_use]
pub(crate) fn update_strip_needs_stored(fields: &[FieldDefinition]) -> bool {
    has_any_field_access(fields, |f| f.access.update.as_ref())
        || has_any_field_access(fields, |f| f.access.read.as_ref())
}

/// Shared body of the create/update strips: round-trip `data` through the
/// map-level strip, with `input.document` as `ctx.document`.
fn strip_in_place<H: WriteHooks + ?Sized>(
    hooks: &H,
    fields: &[FieldDefinition],
    data: &mut DocumentFields,
    input: &WriteStripInput<'_>,
) {
    let mut level: Map<String, Value> = std::mem::take(data).into_inner().into_iter().collect();

    hooks.strip_write_access_map(fields, &mut level, input);

    *data = level.into_iter().collect();
}

/// Trait for executing write hooks, abstracting over VM acquisition strategy.
///
/// Two implementations exist:
/// - [`RunnerWriteHooks`]: acquires a Lua VM from the pool (admin, gRPC, MCP)
/// - [`LuaWriteHooks`]: uses the current Lua VM inline (Lua CRUD hooks)
///
/// The data-aware field-read strip a write applies to the document it reports
/// lives in [`FieldReadStrip`], shared with the read surface so both strip the
/// same way.
pub trait WriteHooks: FieldReadStrip {
    /// Full before-write pipeline: field `BeforeValidate` → richtext attr hooks →
    /// collection `BeforeValidate` → validate → field `BeforeChange` → collection `BeforeChange`.
    ///
    /// # Errors
    ///
    /// Returns an error if any hook stage or validation fails.
    fn run_before_write(
        &self,
        hooks: &Hooks,
        fields: &[FieldDefinition],
        ctx: HookContext,
        val_ctx: &ValidationCtx,
    ) -> Result<HookContext>;

    /// After-write hooks: field `AfterChange` → collection `AfterChange` → registered hooks.
    ///
    /// # Errors
    ///
    /// Returns an error if any hook execution fails.
    fn run_after_write(
        &self,
        hooks: &Hooks,
        fields: &[FieldDefinition],
        event: HookEvent,
        ctx: HookContext,
        conn: &dyn DbConnection,
    ) -> Result<HookContext>;

    /// Run collection-level hooks with CRUD access (for `BeforeDelete` / `AfterDelete`).
    ///
    /// # Errors
    ///
    /// Returns an error if any hook execution fails.
    fn run_hooks_with_conn(
        &self,
        hooks: &Hooks,
        event: HookEvent,
        ctx: HookContext,
        conn: &dyn DbConnection,
    ) -> Result<HookContext>;

    /// Whether a `before_delete` / `after_delete` hook will actually run for a
    /// collection with these `hooks`. Callers use it to decide whether to
    /// pre-load the document's field data into the delete-hook context — so the
    /// load is skipped when no delete hook would see it.
    ///
    /// Default: true when the collection declares any delete hook. Surface
    /// impls refine this with their `hooks_enabled` flag (and, for the pool
    /// runner, knowledge of globally-registered delete hooks).
    fn runs_delete_hooks(&self, hooks: &Hooks) -> bool {
        !hooks.before_delete.is_empty() || !hooks.after_delete.is_empty()
    }

    /// Whether this surface writes with access overridden (a system write):
    /// every access check allows and no field rule strips. Default `false`.
    fn overrides_access(&self) -> bool {
        false
    }

    /// The registry this surface validates writes against, for the persist
    /// steps that resolve definitions beyond the collection's own (the search
    /// index reads rich text custom nodes' `searchable_attrs`). Default `None`
    /// for lightweight test/override impls.
    fn registry(&self) -> Option<&Registry> {
        None
    }

    /// Collection-level access check. Returns the access result (Allowed/Denied/Constrained).
    ///
    /// `locale` is the locale this operation targets (the resolved/default
    /// locale, or `None` when localization is disabled or the op is
    /// locale-agnostic), exposed to access functions as `context.locale`.
    ///
    /// # Errors
    ///
    /// Returns an error if the access hook itself raises (e.g. a Lua runtime error).
    fn check_access(&self, input: &AccessCheckInput<'_>) -> Result<AccessResult>;

    /// Data-aware field-**write** strip: remove from `level` every field the user
    /// may not write under `input.operation` (`"create"` | `"update"`),
    /// evaluating each `access.create` / `access.update` rule with `ctx.data` =
    /// the field's own immediate level and `ctx.document` = `input.document`
    /// (the stored document on update, the incoming one on create). The
    /// write-path mirror of `strip_read_access_map`. Default no-op so
    /// lightweight test/override impls keep their behavior.
    fn strip_write_access_map(
        &self,
        fields: &[FieldDefinition],
        level: &mut Map<String, Value>,
        input: &WriteStripInput<'_>,
    ) {
        let _ = (fields, level, input);
    }

    /// Strip create-denied fields from incoming [`DocumentFields`] in place.
    /// No row exists yet, so each `access.create` rule sees the full pre-strip
    /// incoming data as `ctx.document`.
    fn strip_write_access_create(
        &self,
        fields: &[FieldDefinition],
        data: &mut DocumentFields,
        collection: &str,
        user: Option<&Document>,
        locale: Option<&str>,
    ) {
        if !has_any_field_access(fields, |f| f.access.create.as_ref()) {
            return;
        }

        let document = data.clone();
        strip_in_place(
            self,
            fields,
            data,
            &WriteStripInput {
                document: &document,
                collection,
                user,
                locale,
                operation: "create",
            },
        );
    }

    /// Strip update-denied fields from an incoming patch in place, and keep
    /// every stored value the writer cannot read.
    ///
    /// Each `access.update` rule sees the stored row (`stored.row`) as
    /// `ctx.document`. The patch is what the rule is judging, so it cannot also
    /// be the evidence: a rule gating a field on `ctx.document.owner` would
    /// otherwise pass for any caller who puts their own id in `owner` in the
    /// same write. Callers load the row with [`stored_fields_for_update_rules`]
    /// (or its global twin); an empty document makes a stored-value rule deny.
    ///
    /// A write never changes a value its writer cannot read: every
    /// `access.read` rule is judged against what the write replaces
    /// (`stored.replaced` — the pending draft for a draft save, else the row;
    /// `ctx.data` = the stored level, per array/blocks row), and a value it
    /// hides is kept as it is — at every depth — whatever the patch carries
    /// for it. A row the write adds is judged against an empty row.
    ///
    /// [`stored_fields_for_update_rules`]: crate::service::stored_fields_for_update_rules
    fn strip_write_access_update(
        &self,
        fields: &[FieldDefinition],
        data: &mut DocumentFields,
        stored: UpdateStored<'_>,
        collection: &str,
        user: Option<&Document>,
        locale: Option<&str>,
    ) {
        if !update_strip_needs_stored(fields) {
            return;
        }

        let document = nest_group_fields(stored.row, fields);
        let replaced = nest_group_fields(stored.replaced, fields);
        let replaced_nested: Map<String, Value> =
            replaced.clone().into_inner().into_iter().collect();
        let original: Map<String, Value> = data.clone().into_inner().into_iter().collect();

        // The replaced content is resolved for the write's locale, so a
        // checkbox filled in or put back here carries that locale's value.
        let shape = LeafShape::of(fields, false);
        let mut level = original.clone();
        let filled = prefill_checkboxes(&shape, &mut level, &replaced_nested);
        let before = level.clone();
        *data = level.into_iter().collect();

        strip_in_place(
            self,
            fields,
            data,
            &WriteStripInput {
                document: &document,
                collection,
                user,
                locale,
                operation: "update",
            },
        );

        let mut level: Map<String, Value> = std::mem::take(data).into_inner().into_iter().collect();

        // After the write strip, so a value the writer can neither read nor
        // write is judged once each way and kept either way.
        keep_unreadable(
            self,
            fields,
            &mut level,
            &ReadJudge::builder(&replaced, collection)
                .user(user)
                .locale(locale)
                .build(),
        );

        let removed = removed_paths(&before, &level);
        unfill_kept_checkboxes(&filled, &removed, &mut level, &original);
        restore_stripped_checkboxes(&shape, &removed, &mut level, &replaced_nested);
        *data = level.into_iter().collect();
    }

    /// Strip **write**-denied fields from a version-snapshot `Value::Object` in
    /// place (no-op for a non-object snapshot), evaluating each field's
    /// `access.update` rule. Used by the version-restore path: a user who may
    /// `update` a document but is write-denied on a specific field must not be
    /// able to use a restore to overwrite that field's live value. The denied
    /// field is dropped from the snapshot — with its per-locale columns and a
    /// date's timezone companion — so the partial restore leaves its stored
    /// value untouched (the same input-stripping model `update` uses).
    ///
    /// Each rule judges `stored` — the live row — as `ctx.document`, exactly as
    /// an update does: the snapshot is the value under judgment, so it cannot
    /// also be the evidence. Mirrors
    /// [`FieldReadStrip::strip_read_access_doc`].
    ///
    /// A snapshot carries a column per locale, and writing it back publishes
    /// every one of them, so a localized field is judged once per configured
    /// locale with `ctx.locale` set to that locale and only the denied locale's
    /// columns are dropped; a shared field is judged once, at the write's own
    /// locale, exactly like the request strip.
    fn strip_write_access_value(
        &self,
        fields: &[FieldDefinition],
        snapshot: &mut Value,
        stored: &DocumentFields,
        collection: &str,
        user: Option<&Document>,
        locales: SnapshotLocales<'_>,
    ) {
        if !has_any_field_access(fields, |f| f.access.update.as_ref()) {
            return;
        }

        let Some(obj) = snapshot.as_object() else {
            return;
        };

        // Legacy snapshots may be stored in the flat `group__sub` column form,
        // but the data-aware strip walks the canonical nested shape. Normalize
        // first so a write-denied group sub-field is stripped regardless of how
        // the snapshot was stored; `nest_group_fields` is idempotent for the
        // nested snapshots current code writes. Mirrors the read-path variant.
        let nested = nest_group_fields(&obj.clone().into_iter().collect(), fields);
        let original: Map<String, Value> = nested.into_inner().into_iter().collect();
        let document = nest_group_fields(stored, fields);
        let stored_nested: Map<String, Value> = document.clone().into_inner().into_iter().collect();

        let shape = LeafShape::of(fields, !locales.configured.is_empty());
        let mut before = original.clone();
        let filled = prefill_checkboxes(&shape, &mut before, &stored_nested);

        let judge = |locale: Option<&str>| {
            let mut run = before.clone();
            self.strip_write_access_map(
                fields,
                &mut run,
                &WriteStripInput {
                    document: &document,
                    collection,
                    user,
                    locale,
                    operation: "update",
                },
            );

            removed_paths(&before, &run)
        };

        let mut level = before.clone();

        let shared: Vec<String> = judge(locales.request)
            .into_iter()
            .filter(|path| {
                !shape
                    .leaves_under(std::slice::from_ref(path))
                    .all(|leaf| shape.localized.contains(leaf))
            })
            .collect();
        let shared_leaves: Vec<String> = shape
            .leaves_under(&shared)
            .filter(|leaf| !shape.localized.contains(*leaf))
            .cloned()
            .collect();
        drop_paths(&mut level, "", &shared_leaves);
        drop_locale_columns_of_stripped(&before, &mut level);
        unfill_kept_checkboxes(&filled, &shared_leaves, &mut level, &original);
        restore_stripped_checkboxes(&shape, &shared_leaves, &mut level, &stored_nested);

        for code in locales.configured {
            let denied = judge(Some(code));
            let columns: Vec<String> = shape
                .leaves_under(&denied)
                .filter(|leaf| shape.localized.contains(*leaf))
                .flat_map(|leaf| shape.locale_columns(leaf, code, locales.default == Some(code)))
                .collect();
            drop_paths(&mut level, "", &columns);
        }

        *snapshot = Value::Object(level);
    }

    /// Keep, in a snapshot written back over a document — a publish making
    /// its pending draft live, a version restore — every stored value the
    /// writer cannot read: a write never changes a value its writer cannot
    /// see.
    ///
    /// Each `access.read` rule judges the document as stored in each locale
    /// the write-back writes (`keep.stored`), with `ctx.locale` set to that
    /// locale; a shared value is judged once, at the locale the write runs
    /// under. A value the rule hides is taken out of the snapshot — with its
    /// per-locale keys and companions — so the write-back leaves the stored
    /// value in place; inside a list, a row matched by `id` keeps its hidden
    /// values and a row the document does not hold is judged against an empty
    /// row. No-op for a non-object snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error if a configured locale code has no column form.
    fn keep_unreadable_value(
        &self,
        fields: &[FieldDefinition],
        snapshot: &mut Value,
        keep: &SnapshotReadKeep<'_>,
    ) -> Result<()> {
        // Every lookup and edit below finds a value flat or inside its group
        // object, so the snapshot is taken as stored: nesting it would move a
        // group's per-locale keys into the group.
        let Some(obj) = snapshot.as_object_mut() else {
            return Ok(());
        };

        keep_unreadable_snapshot(self, fields, obj, keep)
    }

    /// Run schema-level field validation (required, unique, regex, type checks,
    /// richtext node attrs, …) without firing any user-defined hooks. Used by
    /// the version restore path so a snapshot whose data violates the current
    /// schema (e.g. an old version from before a `required = true` tightening)
    /// is rejected rather than silently overwriting valid live data.
    ///
    /// # Errors
    ///
    /// Returns a `ValidationError` (containing per-field error messages) when
    /// any schema check fails. Never raises a runtime/IO error — `ValidateResult`
    /// is a `Result<(), ValidationError>` purely as a structured way to surface
    /// the collected errors.
    fn validate_fields(
        &self,
        fields: &[FieldDefinition],
        data: &DocumentFields,
        ctx: &ValidationCtx,
    ) -> ValidateResult;
}
