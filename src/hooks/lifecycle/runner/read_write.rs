//! `HookRunner` methods for CRUD lifecycle orchestration.

use anyhow::Result;

use super::run::{FieldHooksCall, FieldWriteCtx};
use super::vm_pool::reset_instruction_budget;
use crate::{
    core::{
        Document, DocumentFields, FieldDefinition, FieldError, Hooks, ReqContext, ValidationError,
    },
    db::{DbConnection, LocaleContext},
    hooks::{
        HookContext, HookEvent, HookRunner, ValidationCtx,
        lifecycle::{
            LuaCrudInfra,
            execution::{
                AfterReadCtx, FieldHookMeta, apply_after_read_inner, has_field_hooks_for_event,
            },
            types::{FieldHookEvent, TxContextGuard},
            validation::{
                richtext_attrs::{apply_node_attr_before_validate, has_node_attr_before_validate},
                validate_write_fields,
            },
        },
    },
};

/// Bundled inputs for [`HookRunner::apply_after_read_for_event`] — the event
/// surface's equivalent of [`AfterReadCtx`]. Carries the real triggering
/// `operation` (create/update/delete) and the event `timestamp` so an
/// `after_read` hook on a live event sees the same `ctx.operation` and
/// `ctx.data.updated_at` it would on a normal read, instead of a synthetic
/// `"subscribe"` op with no timestamp.
pub struct EventAfterReadInput<'a> {
    pub collection: &'a str,
    pub hooks: &'a Hooks,
    pub fields: &'a [FieldDefinition],
    pub document_id: &'a str,
    pub data: &'a DocumentFields,
    pub user: Option<&'a Document>,
    /// The real operation that produced the event: `"create"`, `"update"`, or `"delete"`.
    pub operation: &'a str,
    /// ISO-8601 timestamp of the event (surfaced to the hook as `updated_at`).
    pub timestamp: &'a str,
}

impl HookRunner {
    /// Fire `before_read` hooks. Returns error to abort the read.
    /// Runs collection-level hook refs, then global registered hooks.
    /// No CRUD access — uses `run_hooks` (no connection).
    ///
    /// # Errors
    ///
    /// Returns an error if any `before_read` hook fails or aborts the read.
    pub fn fire_before_read(&self, hooks: &Hooks, ctx: HookContext) -> Result<ReqContext> {
        // Surface the (possibly hook-modified) shared context so the read path can
        // hand it to `after_read` — the read-lifecycle analogue of how the write
        // lifecycle threads `context` from `before_*` into `after_*`.
        Ok(self.run_hooks(hooks, HookEvent::BeforeRead, ctx)?.context)
    }

    /// Fire `after_read` hooks on a single document. Returns transformed doc.
    /// Field-level `after_read` hooks run first, then collection-level, then global registered.
    /// On error: logs warning, returns original doc unmodified.
    pub fn apply_after_read(&self, ctx: &AfterReadCtx, doc: Document) -> Document {
        let lua = match self.pool.acquire() {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!("VM pool error in apply_after_read: {}", e);

                return doc;
            }
        };

        // Expose the reader + UI locale to field `after_read` hooks (which read
        // them from VM app_data). The freshly-acquired pool VM has none set, so
        // unlike the inline path it would otherwise see `nil`.
        let _identity = TxContextGuard::set_identity(
            &lua,
            ctx.user.cloned(),
            ctx.ui_locale.map(std::string::ToString::to_string),
        );

        apply_after_read_inner(&lua, ctx, doc)
    }

    /// Apply `after_read` hooks to event data, matching the normal Find read pipeline.
    /// Used by SSE and gRPC Subscribe to ensure event data consistency.
    /// Returns the original data unchanged if no hooks are configured.
    #[must_use]
    pub fn apply_after_read_for_event(&self, input: &EventAfterReadInput<'_>) -> DocumentFields {
        let has_field_hooks = has_field_hooks_for_event(input.fields, &FieldHookEvent::AfterRead);
        let has_collection_hooks = !input.hooks.after_read.is_empty();
        let has_registered = self.has_registered_hooks_for("after_read");

        if !has_field_hooks && !has_collection_hooks && !has_registered {
            return input.data.clone();
        }

        let doc = Document {
            id: input.document_id.to_string().into(),
            fields: input.data.clone(),
            created_at: None,
            updated_at: Some(input.timestamp.to_string()),
        };

        let ctx = AfterReadCtx {
            hooks: input.hooks,
            fields: input.fields,
            collection: input.collection,
            operation: input.operation,
            locale: None,
            user: input.user,
            ui_locale: None,
            // Live-event after_read: no `before_read` ran, so no seeded context.
            context: ReqContext::new(),
        };

        self.apply_after_read(&ctx, doc).fields
    }

    /// Fire `after_read` hooks on a list of documents.
    /// Acquires a single VM for the entire batch instead of one per document.
    pub fn apply_after_read_many(&self, ctx: &AfterReadCtx, docs: Vec<Document>) -> Vec<Document> {
        let has_field_hooks = has_field_hooks_for_event(ctx.fields, &FieldHookEvent::AfterRead);
        let has_collection_hooks = !ctx.hooks.after_read.is_empty();
        let has_registered = self.has_registered_hooks_for("after_read");

        // No hooks at all — skip VM acquisition entirely
        if !has_field_hooks && !has_collection_hooks && !has_registered {
            return docs;
        }

        let lua = match self.pool.acquire() {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!("VM pool error in apply_after_read_many: {}", e);

                return docs;
            }
        };

        // Same as `apply_after_read`: expose user + UI locale to field
        // `after_read` hooks on the freshly-acquired pool VM.
        let _identity = TxContextGuard::set_identity(
            &lua,
            ctx.user.cloned(),
            ctx.ui_locale.map(std::string::ToString::to_string),
        );

        docs.into_iter()
            .map(|doc| {
                // One instruction budget per document, as for a single read.
                reset_instruction_budget(&lua);

                apply_after_read_inner(&lua, ctx, doc)
            })
            .collect()
    }

    /// Run the full before-write lifecycle:
    ///   field `BeforeValidate` → collection `BeforeValidate` → `validate_fields` →
    ///   field `BeforeChange` → collection `BeforeChange`.
    /// Returns the final hook context with validated, hook-processed data.
    /// Callers use `HookContext::to_value_map()` on the result to get the data for query functions.
    ///
    /// Field hooks in before-write get full CRUD access (same transaction).
    /// The authenticated user, draft flag, and UI locale are extracted from `ctx`.
    ///
    /// # Errors
    ///
    /// Returns an error if any field hook, collection hook, or validation stage fails.
    pub fn run_before_write(
        &self,
        hooks: &Hooks,
        fields: &[FieldDefinition],
        mut ctx: HookContext,
        val_ctx: &ValidationCtx,
        infra: Option<LuaCrudInfra>,
    ) -> Result<HookContext> {
        // Field-level before_validate (normalize inputs, CRUD available)
        let wctx = FieldWriteCtx::builder(val_ctx.conn)
            .user(ctx.user.as_ref())
            .ui_locale(ctx.ui_locale.as_deref())
            .infra(infra.clone())
            .build();

        self.run_field_hooks_with_conn(
            &mut ctx.data,
            &FieldHooksCall {
                fields,
                event: FieldHookEvent::BeforeValidate,
                collection: &ctx.collection,
                operation: &ctx.operation,
                id: ctx.document_id.as_deref(),
                locale: val_ctx.locale_ctx.map(LocaleContext::access_locale),
            },
            wctx,
        )?;

        // Run before_validate hooks on richtext node attrs (normalize attr values)
        let wctx = FieldWriteCtx::builder(val_ctx.conn)
            .user(ctx.user.as_ref())
            .ui_locale(ctx.ui_locale.as_deref())
            .infra(infra.clone())
            .build();

        self.run_richtext_node_attr_before_validate(
            fields,
            &mut ctx.data,
            &FieldHookMeta {
                collection: &ctx.collection,
                operation: &ctx.operation,
                id: ctx.document_id.as_deref(),
                locale: val_ctx.locale_ctx.map(LocaleContext::access_locale),
            },
            wctx,
        )?;

        // Collection-level before_validate
        let ctx = self.run_hooks_with_conn(
            hooks,
            HookEvent::BeforeValidate,
            ctx,
            val_ctx.conn,
            infra.clone(),
        )?;

        // Validation (skip required checks for drafts)
        self.validate_fields(fields, &ctx.data, val_ctx)?;

        // Field-level before_change (post-validation transforms, CRUD available)
        let mut ctx = ctx;
        let wctx = FieldWriteCtx::builder(val_ctx.conn)
            .user(ctx.user.as_ref())
            .ui_locale(ctx.ui_locale.as_deref())
            .infra(infra.clone())
            .build();

        self.run_field_hooks_with_conn(
            &mut ctx.data,
            &FieldHooksCall {
                fields,
                event: FieldHookEvent::BeforeChange,
                collection: &ctx.collection,
                operation: &ctx.operation,
                id: ctx.document_id.as_deref(),
                locale: val_ctx.locale_ctx.map(LocaleContext::access_locale),
            },
            wctx,
        )?;

        // Collection-level before_change
        self.run_hooks_with_conn(hooks, HookEvent::BeforeChange, ctx, val_ctx.conn, infra)
    }

    /// Run after-write hooks inside the transaction (with CRUD access).
    /// Field-level `after_change` hooks run first, then collection-level, then registered.
    /// Errors propagate up and cause the caller's transaction to roll back.
    /// The authenticated user and UI locale are extracted from `ctx`.
    ///
    /// # Errors
    ///
    /// Returns an error if any field hook or collection hook fails.
    pub fn run_after_write(
        &self,
        hooks: &Hooks,
        fields: &[FieldDefinition],
        event: HookEvent,
        mut ctx: HookContext,
        conn: &dyn DbConnection,
        infra: Option<LuaCrudInfra>,
    ) -> Result<HookContext> {
        // Run field-level after_change hooks (with CRUD access). Their
        // mutations are applied to `ctx.data` so the collection-level and
        // registered `after_change` hooks below see the transformed values —
        // matching the Lua-path adapter (`LuaWriteHooks::run_after_write`).
        if matches!(event, HookEvent::AfterChange) {
            let has_field_hooks = has_field_hooks_for_event(fields, &FieldHookEvent::AfterChange);

            if has_field_hooks {
                let wctx = FieldWriteCtx::builder(conn)
                    .user(ctx.user.as_ref())
                    .ui_locale(ctx.ui_locale.as_deref())
                    .infra(infra.clone())
                    .build();

                let mut data = ctx.data.clone();
                self.run_field_hooks_with_conn(
                    &mut data,
                    &FieldHooksCall {
                        fields,
                        event: FieldHookEvent::AfterChange,
                        collection: &ctx.collection,
                        operation: &ctx.operation,
                        id: ctx.document_id.as_deref(),
                        locale: ctx.locale.as_deref(),
                    },
                    wctx,
                )?;
                ctx.data = data;
            }
        }

        // Run collection-level + registered hooks (with CRUD access)
        self.run_hooks_with_conn(hooks, event, ctx, conn, infra)
    }

    /// Run `before_validate` hooks on richtext node attrs within field data,
    /// at any depth. The VM is acquired only when some field has such hooks;
    /// like field hooks, they run with the write's transaction, user and UI
    /// locale injected.
    ///
    /// # Errors
    ///
    /// Returns an error when no VM can be acquired or a hook fails — the write
    /// must not proceed with an attr value its hook never normalized.
    fn run_richtext_node_attr_before_validate(
        &self,
        fields: &[FieldDefinition],
        data: &mut DocumentFields,
        meta: &FieldHookMeta<'_>,
        wctx: FieldWriteCtx<'_>,
    ) -> Result<()> {
        if !has_node_attr_before_validate(fields, &self.registry) {
            return Ok(());
        }

        let lua = self.pool.acquire()?;

        let _guard = TxContextGuard::set(
            &lua,
            wctx.conn,
            wctx.user.cloned(),
            wctx.ui_locale.map(ToString::to_string),
            wctx.infra,
        );

        apply_node_attr_before_validate(&lua, fields, data, &self.registry, meta)
    }

    /// Validate field data against field definitions.
    /// Checks `required`, `unique`, and custom `validate` (Lua function ref).
    /// Runs inside the caller's transaction for unique checks.
    /// Automatically injects the registry for richtext node attr validation.
    ///
    /// # Errors
    ///
    /// Returns a `ValidationError` (collected per-field error messages) when
    /// any check fails. Lua VM acquisition failures are surfaced through a
    /// synthetic `_system` field error.
    pub fn validate_fields(
        &self,
        fields: &[FieldDefinition],
        data: &DocumentFields,
        ctx: &ValidationCtx,
    ) -> Result<(), ValidationError> {
        let lua = self
            .pool
            .acquire()
            .map_err(|_| ValidationError::new(vec![FieldError::new("_system", "VM pool error")]))?;

        // Validation runs on its own freshly-acquired pool VM (separate from the
        // write-hook VM), so custom `validate` functions would otherwise see
        // `ctx.user`/`ctx.ui_locale` as nil. Expose them via app-data (no CRUD).
        let _identity = TxContextGuard::set_identity(
            &lua,
            ctx.user.cloned(),
            ctx.ui_locale.map(std::string::ToString::to_string),
        );

        validate_write_fields(&lua, fields, data, ctx, &self.registry)
    }
}
