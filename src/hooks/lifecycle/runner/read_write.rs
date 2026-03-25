//! HookRunner methods for CRUD lifecycle orchestration.

use std::collections::HashMap;

use anyhow::Result;
use serde_json::Value;

use crate::{
    core::{
        Document, FieldDefinition,
        collection::Hooks,
        validate::{FieldError, ValidationError},
    },
    hooks::{
        HookContext, HookEvent, HookRunner, ValidationCtx,
        lifecycle::{
            execution::{AfterReadCtx, apply_after_read_inner},
            types::FieldHookEvent,
            validation::validate_fields_inner,
        },
    },
};

impl HookRunner {
    /// Fire before_read hooks. Returns error to abort the read.
    /// Runs collection-level hook refs, then global registered hooks.
    /// No CRUD access — uses `run_hooks` (no connection).
    pub fn fire_before_read(
        &self,
        hooks: &Hooks,
        collection: &str,
        operation: &str,
        data: HashMap<String, Value>,
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
    pub fn run_before_write(
        &self,
        hooks: &Hooks,
        fields: &[FieldDefinition],
        mut ctx: HookContext,
        val_ctx: &ValidationCtx,
    ) -> Result<HookContext> {
        // Field-level before_validate (normalize inputs, CRUD available)
        let wctx = super::run::FieldWriteCtx::builder(val_ctx.conn)
            .user(ctx.user.as_ref())
            .ui_locale(ctx.ui_locale.as_deref())
            .build();

        self.run_field_hooks_with_conn(
            fields,
            FieldHookEvent::BeforeValidate,
            &mut ctx.data,
            &ctx.collection,
            &ctx.operation,
            &wctx,
        )?;

        // Run before_validate hooks on richtext node attrs (normalize attr values)
        self.run_richtext_node_attr_before_validate(fields, &mut ctx.data, &ctx.collection);

        // Collection-level before_validate
        let ctx = self.run_hooks_with_conn(hooks, HookEvent::BeforeValidate, ctx, val_ctx.conn)?;

        // Validation (skip required checks for drafts)
        self.validate_fields(fields, &ctx.data, val_ctx)?;

        // Field-level before_change (post-validation transforms, CRUD available)
        let mut ctx = ctx;
        let wctx = super::run::FieldWriteCtx::builder(val_ctx.conn)
            .user(ctx.user.as_ref())
            .ui_locale(ctx.ui_locale.as_deref())
            .build();

        self.run_field_hooks_with_conn(
            fields,
            FieldHookEvent::BeforeChange,
            &mut ctx.data,
            &ctx.collection,
            &ctx.operation,
            &wctx,
        )?;

        // Collection-level before_change
        self.run_hooks_with_conn(hooks, HookEvent::BeforeChange, ctx, val_ctx.conn)
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
        conn: &dyn crate::db::DbConnection,
    ) -> Result<HookContext> {
        // Run field-level after_change hooks (with CRUD access)
        if matches!(event, HookEvent::AfterChange) {
            let has_field_hooks = fields.iter().any(|f| !f.hooks.after_change.is_empty());

            if has_field_hooks {
                let mut data = ctx.data.clone();
                let wctx = super::run::FieldWriteCtx::builder(conn)
                    .user(ctx.user.as_ref())
                    .ui_locale(ctx.ui_locale.as_deref())
                    .build();

                self.run_field_hooks_with_conn(
                    fields,
                    FieldHookEvent::AfterChange,
                    &mut data,
                    &ctx.collection,
                    &ctx.operation,
                    &wctx,
                )?;
            }
        }

        // Run collection-level + registered hooks (with CRUD access)
        self.run_hooks_with_conn(hooks, event, ctx, conn)
    }

    /// Run `before_validate` hooks on richtext node attrs within field data.
    ///
    /// For each Richtext field with custom nodes that have `before_validate` hooks,
    /// extracts node attrs, runs the hooks, and writes the modified content back.
    fn run_richtext_node_attr_before_validate(
        &self,
        fields: &[FieldDefinition],
        data: &mut HashMap<String, Value>,
        collection: &str,
    ) {
        use super::super::validation::richtext_attrs::run_before_validate_on_node_attrs;
        use crate::core::FieldType;

        // Check if any richtext field has node attr before_validate hooks
        let richtext_fields: Vec<&FieldDefinition> = fields
            .iter()
            .filter(|f| f.field_type == FieldType::Richtext && !f.admin.nodes.is_empty())
            .collect();

        if richtext_fields.is_empty() {
            return;
        }

        let has_any_hooks = richtext_fields.iter().any(|f| {
            f.admin.nodes.iter().any(|node_name| {
                self.registry
                    .get_richtext_node(node_name)
                    .map(|nd| nd.attrs.iter().any(|a| !a.hooks.before_validate.is_empty()))
                    .unwrap_or(false)
            })
        });

        if !has_any_hooks {
            return;
        }

        let lua = match self.pool.acquire() {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!("VM pool error in richtext node attr before_validate: {}", e);
                return;
            }
        };

        for field in richtext_fields {
            if let Some(Value::String(content)) = data.get(&field.name) {
                let new_content = run_before_validate_on_node_attrs(
                    &lua,
                    content,
                    field,
                    &self.registry,
                    collection,
                );
                if new_content != *content {
                    data.insert(field.name.clone(), Value::String(new_content));
                }
            }
        }
    }

    /// Validate field data against field definitions.
    /// Checks `required`, `unique`, and custom `validate` (Lua function ref).
    /// Runs inside the caller's transaction for unique checks.
    /// Automatically injects the registry for richtext node attr validation.
    pub fn validate_fields(
        &self,
        fields: &[FieldDefinition],
        data: &HashMap<String, Value>,
        ctx: &ValidationCtx,
    ) -> Result<(), ValidationError> {
        let lua = self
            .pool
            .acquire()
            .map_err(|_| ValidationError::new(vec![FieldError::new("_system", "VM pool error")]))?;

        // Inject registry for richtext node attr validation if not already set
        if ctx.registry.is_some() {
            return validate_fields_inner(&lua, fields, data, ctx);
        }
        let enriched_ctx = ValidationCtx {
            conn: ctx.conn,
            table: ctx.table,
            exclude_id: ctx.exclude_id,
            is_draft: ctx.is_draft,
            locale_ctx: ctx.locale_ctx,
            registry: Some(&self.registry),
        };
        validate_fields_inner(&lua, fields, data, &enriched_ctx)
    }
}
