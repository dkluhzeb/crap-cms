//! Global document update.

use serde_json::{Map, Value};

use crate::{
    core::{
        Document, DocumentFields, collection::GlobalDefinition, event::EventOperation,
        reject_nul_characters,
    },
    db::{
        DbConnection, LocaleContext, query,
        query::{StoredRow, helpers::global_table},
    },
    hooks::{AccessCheckInput, HookContext, ValidationCtx},
    service::{
        AfterChangeInput, Gated, NonObjectGroups, ServiceContext, ServiceError, UpdateStored,
        WriteHooks, WriteInput, WriteResult, admit_global_update_input, draft_save_base,
        global_access_allowed, helpers as svc_helpers,
        persist::{DraftDocumentArgs, draft_document},
        run_after_change_hooks, run_pool_write, update_strip_needs_stored,
        versions::{self, VersionSnapshotCtx},
        write::{
            PendingDraft, PublishStored, WriterHeldGate, claim_revision,
            refuse_unreadable_references, reject_locale_locked_fields,
        },
    },
};

type Result<T> = std::result::Result<T, ServiceError>;

/// The DB write phase of a global update: where it lands, the post-hook data,
/// and the pending draft a publish makes live. Every field is required and it
/// is built at the one call site, so a plain literal stands in for a builder.
#[derive(Clone, Copy)]
struct GlobalPersist<'a> {
    gtable: &'a str,
    final_ctx: &'a HookContext,
    locale_ctx: Option<&'a LocaleContext>,
    is_draft: bool,
    /// The publisher-stripped pending draft snapshot, when this write publishes
    /// one. It carries the locales the request does not target and the shared
    /// values a default-locale draft save recorded.
    pending_draft: Option<&'a Map<String, Value>>,
}

/// What the before-write hook chain needs beyond the definition and the
/// context: the request, the table it lands in, whether it is a draft save,
/// and the pending-draft snapshot a publish writes back afterwards. Every field is
/// required and it is built at the one call site, so a plain literal stands in
/// for a builder.
struct GlobalBeforeWrite<'a> {
    input: &'a WriteInput<'a>,
    gtable: &'a str,
    is_draft: bool,
    locale_overlay: Option<&'a Map<String, Value>>,
}

/// Load the stored global that field-level `access.update` rules judge as
/// `ctx.document` — the global twin of
/// [`stored_fields_for_update_rules`](crate::service::stored_fields_for_update_rules).
/// Skips the read when no field configures `access.update` or `access.read`.
///
/// # Errors
///
/// Returns an error if the global row cannot be read.
pub(crate) fn stored_global_fields_for_update_rules(
    conn: &dyn DbConnection,
    slug: &str,
    def: &GlobalDefinition,
    locale_ctx: Option<&LocaleContext>,
) -> Result<DocumentFields> {
    if !update_strip_needs_stored(&def.fields) {
        return Ok(DocumentFields::default());
    }

    Ok(query::get_global(conn, slug, def, locale_ctx)?.fields)
}

/// The global twin of the collection update's field strip (`admit_update`):
/// write-denied fields are stripped (each `access.update` rule sees `ctx.data`
/// = its level and `ctx.document` = the stored global, never the patch), every
/// value the writer cannot read is kept as it is (judged against what the
/// write replaces: the pending draft for a draft save, else the stored global),
/// the request's non-object groups are refused, and the pending draft a
/// publish makes live is returned — stripped and kept the same way per locale.
///
/// # Errors
///
/// Returns a backend error if the stored global or its draft cannot be read,
/// or the non-object group refusal.
fn strip_global_input(
    ctx: &ServiceContext,
    conn: &dyn DbConnection,
    input: &mut WriteInput<'_>,
    (pending_draft, groups): (PendingDraft, NonObjectGroups),
) -> Result<Option<Value>> {
    let def = ctx.global_def()?;
    let write_hooks = ctx.write_hooks()?;
    let gtable = global_table(ctx.slug);

    let stored = stored_global_fields_for_update_rules(conn, ctx.slug, def, input.locale_ctx)?;
    let draft = draft_save_base(
        conn,
        &StoredRow {
            table: &gtable,
            id: "default",
            fields: &def.fields,
            locale_ctx: input.locale_ctx,
        },
        input.draft && def.has_drafts() && def.has_versions(),
    )?;
    write_hooks.strip_write_access_update(
        &def.fields,
        &mut input.data,
        UpdateStored::new(&stored, draft.as_ref().unwrap_or(&stored)),
        ctx.slug,
        ctx.user,
        input.locale_ctx.map(LocaleContext::access_locale),
    );

    // A non-object group the strip left is refused; one it dropped is silent.
    groups.refuse_unstripped(&input.data)?;

    // The draft goes live as ONE unit, so the locales this request does not
    // target come from the snapshot — stripped and kept exactly like the
    // merged data above, each locale against the global as it stores it.
    let load = |locale_ctx: Option<&LocaleContext>| {
        stored_global_fields_for_update_rules(conn, ctx.slug, def, locale_ctx)
    };

    pending_draft.publishing_snapshot(
        ctx,
        write_hooks,
        PublishStored::new(&stored, &load),
        input.locale_ctx,
    )
}

/// Update a global document.
///
/// **Pool mode** (`ctx.pool` set): opens a transaction, commits after success.
/// **Conn mode** (`ctx.conn` set, Lua CRUD path): runs on the existing connection.
///
/// # Errors
///
/// Returns service-layer errors (access denied, validation, hook errors) or
/// a backend error if the DB transaction or persistence fails.
#[cfg(not(tarpaulin_include))]
pub fn update_global_document(ctx: &ServiceContext, input: WriteInput<'_>) -> Result<WriteResult> {
    if ctx.pool.is_some() {
        update_global_pool(ctx, input)
    } else {
        update_global_conn(ctx, input)
    }
}

fn update_global_pool(ctx: &ServiceContext, input: WriteInput<'_>) -> Result<WriteResult> {
    let (result, _) = run_pool_write(
        ctx,
        None,
        |inner| update_global_gated(inner, input),
        |ctx, (result, row)| {
            ctx.publish_mutation_event(EventOperation::Update, &result.0.id, row.clone());
        },
    )?;

    Ok(result)
}

fn update_global_conn(ctx: &ServiceContext, input: WriteInput<'_>) -> Result<WriteResult> {
    let (result, row) = update_global_gated(ctx, input)?;

    ctx.clear_cache();

    ctx.publish_mutation_event(EventOperation::Update, &result.0.id, row);

    Ok(result)
}

/// Core logic for global update — accepts `ServiceContext` for hook abstraction.
///
/// # Errors
///
/// Returns service-layer errors (access denied, validation, hook errors) or
/// a backend error if persistence fails.
pub fn update_global_in_conn(ctx: &ServiceContext, input: WriteInput<'_>) -> Result<WriteResult> {
    update_global_gated(ctx, input).map(|(result, _)| result)
}

/// [`update_global_in_conn`], plus the stored row the update's live event is
/// built from.
fn update_global_gated(
    ctx: &ServiceContext,
    mut input: WriteInput<'_>,
) -> Result<Gated<WriteResult>> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let write_hooks = ctx.write_hooks()?;
    let def = ctx.global_def()?;

    let gtable = global_table(ctx.slug);

    // Serialize concurrent writers of the global's single row before the write
    // reads anything it builds on: the pending draft below and the
    // outgoing-ref snapshot at persist time are both plain SELECTs, so without
    // the lock a publisher can write back a draft a concurrent draft save has
    // already superseded, and two writers can double-apply a ref-count delta.
    // No-op on SQLite, whose IMMEDIATE transaction serializes writers already.
    conn.lock_row(&gtable, "default")?;

    // The admission prefix the `validate` dry-run runs too: canonicalize the
    // incoming data, adopt the pending draft as the write's base (publishing
    // means the same thing on a global as on a collection), and refuse a
    // non-default-locale write that carries a locale-locked field rather than
    // silently skipping it — once the access gate has admitted the caller.
    let admission = admit_global_update_input(ctx, def, &mut input)?;

    check_global_update_access(
        ctx,
        write_hooks,
        def,
        Some(&input.data),
        input.locale_ctx.map(LocaleContext::access_locale),
    )?;

    // The input's refusal, raised only past the access gate (see `Admission`).
    let admitted = admission.admit()?;

    // The collection update's revision step (`admit_update`): under the lock,
    // after the access gate, in this transaction.
    claim_revision(conn, &gtable, "default", input.expected_revision)?;

    let is_draft = input.draft && def.has_drafts();
    let ui_locale = ctx.ui_locale.as_deref();

    let publishing_draft = strip_global_input(ctx, conn, &mut input, admitted)?;

    let final_ctx = run_global_before_write_hooks(
        write_hooks,
        ctx,
        def,
        &GlobalBeforeWrite {
            input: &input,
            gtable: &gtable,
            is_draft,
            locale_overlay: publishing_draft.as_ref().and_then(Value::as_object),
        },
    )?;

    // A draft save leaves the published global where it is, which its live
    // event records beside the pending draft it describes.
    let snapshot_only = is_draft && def.has_versions();
    let row_before = ctx.update_row_before("default", snapshot_only, input.locale_ctx)?;

    // Both reported shapes carry their rows for the write's locale before
    // after-change hooks see them: a draft save its snapshot, a published write
    // the global as `get_global` reads it.
    let mut doc = persist_global_update(
        conn,
        ctx,
        def,
        &GlobalPersist {
            gtable: &gtable,
            final_ctx: &final_ctx,
            locale_ctx: input.locale_ctx,
            is_draft,
            pending_draft: publishing_draft.as_ref().and_then(Value::as_object),
        },
    )?;

    let after_ctx = run_after_change_hooks(
        write_hooks,
        &def.hooks,
        &def.fields,
        &doc,
        AfterChangeInput::builder(ctx.slug, "update")
            .locale(
                input
                    .locale_ctx
                    .map(LocaleContext::access_locale)
                    .map(String::from),
            )
            .draft(is_draft)
            .req_context(final_ctx.context)
            .user(ctx.user)
            .ui_locale(ui_locale)
            .build(),
        conn,
    )?;

    // The global as stored, before anything is shaped or stripped for the
    // writer: the live event is built from it.
    let row = ctx
        .write_event_row(&doc, input.locale_ctx, snapshot_only)?
        .map(|row| row.before_write(row_before, snapshot_only));

    svc_helpers::strip_reported(ctx, write_hooks, &mut doc, input.locale_ctx)?;

    Ok(((doc, after_ctx), row))
}

/// Enforce the global-update access check. Globals don't support
/// filter-based access — the `Constrained` variant is rejected so
/// access hooks have to be boolean (true/false on `ctx.user`).
pub(crate) fn check_global_update_access(
    ctx: &ServiceContext,
    write_hooks: &dyn WriteHooks,
    def: &GlobalDefinition,
    data: Option<&DocumentFields>,
    locale: Option<&str>,
) -> Result<()> {
    let access = write_hooks.check_access(
        &AccessCheckInput::builder("update", ctx.slug)
            .access(def.access.update.as_ref())
            .user(ctx.user)
            .id(Some("default"))
            .data(data)
            .locale(locale)
            .ui_locale(ctx.ui_locale.as_deref())
            .build(),
    )?;

    if !global_access_allowed(&access, ctx.slug)? {
        return Err(ServiceError::AccessDenied("Update access denied".into()));
    }

    Ok(())
}

/// Build the hook + validation contexts and run the before-write
/// hook chain. The returned `ReqContext` carries the (possibly
/// mutated) data forward into the persistence step.
fn run_global_before_write_hooks(
    write_hooks: &dyn WriteHooks,
    ctx: &ServiceContext,
    def: &GlobalDefinition,
    call: &GlobalBeforeWrite<'_>,
) -> Result<HookContext> {
    let input = call.input;

    let hook_data = input.data.clone();
    let hook_ctx = ctx
        .hook_context("update")
        .data(hook_data)
        .document_id("default")
        .locale(input.locale_ctx.map(LocaleContext::access_locale))
        .draft(call.is_draft)
        .build();

    // A publish writes the draft's other locales back over the row after this
    // validation, so the completeness gate judges that snapshot rather than the
    // locales it is about to replace.
    let conn = ctx.resolve_conn()?;

    // The stored values a resubmitted value no check would now accept may lean
    // on — only what this writer may read.
    let held_gate = WriterHeldGate::builder(write_hooks, ctx.slug, &def.fields)
        .draft_access(def.access.resolve_draft())
        .user(ctx.user)
        .locale(input.locale_ctx.map(LocaleContext::access_locale))
        .build();

    let val_ctx = ValidationCtx::builder(conn.as_ref(), call.gtable)
        .exclude_id(Some("default"))
        .draft(call.is_draft)
        .locale_ctx(input.locale_ctx)
        .user(ctx.user)
        .ui_locale(ctx.ui_locale.as_deref())
        .locale_overlay(call.locale_overlay)
        .versioned_drafts(def.has_drafts())
        .held_gate(Some(&held_gate))
        .build();

    Ok(write_hooks.run_before_write(&def.hooks, &def.fields, hook_ctx, &val_ctx)?)
}

/// Either save a draft version (when `is_draft && has_versions`) or
/// run the published-update pipeline (column update + join tables +
/// ref-count delta + version snapshot if versioned).
fn persist_global_update(
    conn: &dyn DbConnection,
    ctx: &ServiceContext,
    def: &GlobalDefinition,
    persist: &GlobalPersist<'_>,
) -> Result<Document> {
    // Final post-hook data, draft or published (see the collection persist
    // path).
    reject_nul_characters(&persist.final_ctx.data, &def.fields)?;

    if persist.is_draft && def.has_versions() {
        let existing_doc = query::get_global(conn, ctx.slug, def, persist.locale_ctx)?;
        let snapshot = versions::save_draft_version(&versions::SaveDraftArgs {
            conn,
            table: persist.gtable,
            parent_id: "default",
            fields: &def.fields,
            versions: def.versions.as_ref(),
            existing_doc: &existing_doc,
            data: &persist.final_ctx.data,
            locale_ctx: persist.locale_ctx,
        })?;
        // The draft content, not the untouched published row (see
        // `persist::draft_document`).
        return Ok(draft_document(&DraftDocumentArgs {
            id: "default",
            snapshot: &snapshot,
            existing: &existing_doc,
            fields: &def.fields,
            locale_ctx: persist.locale_ctx,
        })?);
    }

    persist_global_published_update(conn, ctx, def, persist)
}

/// Published-write path: snapshot the outgoing refs, write the row +
/// join tables, apply the ref-count delta, and (if the global is
/// versioned) capture a "published" snapshot.
fn persist_global_published_update(
    conn: &dyn DbConnection,
    ctx: &ServiceContext,
    def: &GlobalDefinition,
    persist: &GlobalPersist<'_>,
) -> Result<Document> {
    let GlobalPersist {
        gtable,
        final_ctx,
        locale_ctx,
        pending_draft,
        ..
    } = *persist;

    let locale_cfg = locale_ctx
        .map(|lctx| lctx.config.clone())
        .unwrap_or_default();

    // Final post-hook data (see the collection persist path).
    reject_locale_locked_fields(&def.fields, &final_ctx.data, locale_ctx)?;

    // The row lock this snapshot needs is already held: `update_global_in_conn`
    // takes it at the top of the write, before anything is read.
    let old_refs = query::ref_count::snapshot_outgoing_refs(
        conn,
        gtable,
        "default",
        &def.fields,
        &locale_cfg,
    )?;

    // See `persist_update`: the merged data carries the draft's values for the
    // locale this write targets, and the write-back carries its other locales
    // and shared values. Inside the ref-count bracket, because it moves
    // relationships of its own. Without localization the merged data is
    // already the whole draft.
    if let Some(pending) = pending_draft.filter(|_| locale_cfg.is_enabled()) {
        query::write_global_snapshot_base(conn, ctx.slug, def, pending, &locale_cfg)?;
    }

    let final_data = final_ctx.to_value_map();
    let mut doc = query::update_global(conn, ctx.slug, def, &final_data, locale_ctx)?;

    query::save_join_table_data(
        conn,
        gtable,
        &def.fields,
        "default",
        &final_ctx.data,
        locale_ctx,
    )?;

    // A refused reference — to a missing document, or a new one to a document
    // the writer may not read — is reported on the field holding it.
    query::ref_count::after_update(conn, gtable, "default", &def.fields, &locale_cfg, &old_refs)
        .and_then(|added| refuse_unreadable_references(ctx, &added, locale_ctx))
        .map_err(|e| query::ref_count::anchor_to_fields(e, &def.fields, &final_ctx.data))?;

    if def.has_versions() {
        // Also stamps `doc` published: publishing an unpublished global read
        // the row back while it still said `draft`.
        let snap_ctx =
            VersionSnapshotCtx::for_global(gtable, def, locale_ctx.map(|lctx| &lctx.config));
        versions::create_version_snapshot(conn, &snap_ctx, "published", &mut doc)?;
    }

    Ok(doc)
}
