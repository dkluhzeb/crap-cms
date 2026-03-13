//! HookRunner methods for CRUD lifecycle orchestration.

use std::collections::HashMap;

use anyhow::Result;

use crate::core::Document;
use crate::core::collection::Hooks;
use crate::core::field::FieldDefinition;
use crate::core::validate::{FieldError, ValidationError};
use crate::db::query::LocaleContext;
use crate::hooks::lifecycle::context::HookContext;
use crate::hooks::lifecycle::execution::{AfterReadCtx, apply_after_read_inner};
use crate::hooks::lifecycle::types::{FieldHookEvent, HookEvent};
use crate::hooks::lifecycle::validation::{ValidationCtx, validate_fields_inner};

use super::HookRunner;

impl HookRunner {
    /// Fire before_read hooks. Returns error to abort the read.
    /// Runs collection-level hook refs, then global registered hooks.
    /// No CRUD access — uses `run_hooks` (no connection).
    pub fn fire_before_read(
        &self,
        hooks: &Hooks,
        collection: &str,
        operation: &str,
        data: HashMap<String, serde_json::Value>,
    ) -> Result<()> {
        let ctx = HookContext::builder(collection, operation)
            .data(data)
            .build();
        self.run_hooks(hooks, HookEvent::BeforeRead, ctx)?;
        Ok(())
    }

    /// Fire after_read hooks on a single document. Returns transformed doc.
    /// Field-level after_read hooks run first, then collection-level, then global registered.
    /// On error: logs warning, returns original doc unmodified.
    pub fn apply_after_read(&self, ctx: &AfterReadCtx, doc: Document) -> Document {
        let lua = match self.pool.acquire() {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!("VM pool error in apply_after_read: {}", e);
                return doc;
            }
        };
        apply_after_read_inner(&lua, ctx, doc)
    }

    /// Fire after_read hooks on a list of documents.
    /// Acquires a single VM for the entire batch instead of one per document.
    pub fn apply_after_read_many(&self, ctx: &AfterReadCtx, docs: Vec<Document>) -> Vec<Document> {
        let has_field_hooks = ctx.fields.iter().any(|f| !f.hooks.after_read.is_empty());
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

        docs.into_iter()
            .map(|doc| apply_after_read_inner(&lua, ctx, doc))
            .collect()
    }

    /// Run the full before-write lifecycle:
    ///   field BeforeValidate → collection BeforeValidate → validate_fields →
    ///   field BeforeChange → collection BeforeChange.
    /// Returns the final hook context with validated, hook-processed data.
    /// Callers use `HookContext::to_string_map()` on the result to get the data for query functions.
    ///
    /// Field hooks in before-write get full CRUD access (same transaction).
    /// The authenticated user, draft flag, and UI locale are extracted from `ctx`.
    #[allow(clippy::too_many_arguments)]
    pub fn run_before_write(
        &self,
        hooks: &Hooks,
        fields: &[FieldDefinition],
        mut ctx: HookContext,
        conn: &rusqlite::Connection,
        table: &str,
        exclude_id: Option<&str>,
        locale_ctx: Option<&LocaleContext>,
    ) -> Result<HookContext> {
        let is_draft = ctx.draft.unwrap_or(false);

        // Field-level before_validate (normalize inputs, CRUD available)
        self.run_field_hooks_with_conn(
            fields,
            FieldHookEvent::BeforeValidate,
            &mut ctx.data,
            &ctx.collection,
            &ctx.operation,
            conn,
            ctx.user.as_ref(),
            ctx.ui_locale.as_deref(),
        )?;
        // Collection-level before_validate
        let ctx = self.run_hooks_with_conn(hooks, HookEvent::BeforeValidate, ctx, conn)?;
        // Validation (skip required checks for drafts)
        let val_ctx = ValidationCtx {
            conn,
            table,
            exclude_id,
            is_draft,
            locale_ctx,
        };
        self.validate_fields(fields, &ctx.data, &val_ctx)?;
        // Field-level before_change (post-validation transforms, CRUD available)
        let mut ctx = ctx;
        self.run_field_hooks_with_conn(
            fields,
            FieldHookEvent::BeforeChange,
            &mut ctx.data,
            &ctx.collection,
            &ctx.operation,
            conn,
            ctx.user.as_ref(),
            ctx.ui_locale.as_deref(),
        )?;
        // Collection-level before_change
        self.run_hooks_with_conn(hooks, HookEvent::BeforeChange, ctx, conn)
    }

    /// Run after-write hooks inside the transaction (with CRUD access).
    /// Field-level after_change hooks run first, then collection-level, then registered.
    /// Errors propagate up and cause the caller's transaction to roll back.
    /// The authenticated user and UI locale are extracted from `ctx`.
    pub fn run_after_write(
        &self,
        hooks: &Hooks,
        fields: &[FieldDefinition],
        event: HookEvent,
        ctx: HookContext,
        conn: &rusqlite::Connection,
    ) -> Result<HookContext> {
        // Run field-level after_change hooks (with CRUD access)
        if matches!(event, HookEvent::AfterChange) {
            let has_field_hooks = fields.iter().any(|f| !f.hooks.after_change.is_empty());
            if has_field_hooks {
                let mut data = ctx.data.clone();
                self.run_field_hooks_with_conn(
                    fields,
                    FieldHookEvent::AfterChange,
                    &mut data,
                    &ctx.collection,
                    &ctx.operation,
                    conn,
                    ctx.user.as_ref(),
                    ctx.ui_locale.as_deref(),
                )?;
            }
        }

        // Run collection-level + registered hooks (with CRUD access)
        self.run_hooks_with_conn(hooks, event, ctx, conn)
    }

    /// Validate field data against field definitions.
    /// Checks `required`, `unique`, and custom `validate` (Lua function ref).
    /// Runs inside the caller's transaction for unique checks.
    pub fn validate_fields(
        &self,
        fields: &[FieldDefinition],
        data: &HashMap<String, serde_json::Value>,
        ctx: &ValidationCtx,
    ) -> Result<(), ValidationError> {
        let lua = self
            .pool
            .acquire()
            .map_err(|_| ValidationError::new(vec![FieldError::new("_system", "VM pool error")]))?;
        validate_fields_inner(&lua, fields, data, ctx)
    }
}
