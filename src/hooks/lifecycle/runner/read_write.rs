//! `HookRunner` methods for CRUD lifecycle orchestration.

use anyhow::Result;
use serde_json::Value;

use super::run::{FieldHooksCall, FieldWriteCtx};
use crate::{
    core::{
        Document, DocumentFields, FieldDefinition, FieldError, FieldType, Hooks, ValidationError,
    },
    db::{DbConnection, LocaleContext, query::helpers::prefixed_name},
    hooks::{
        HookContext, HookEvent, HookRunner, ValidationCtx,
        lifecycle::{
            LuaCrudInfra,
            execution::{AfterReadCtx, apply_after_read_inner, has_field_hooks_for_event},
            types::{FieldHookEvent, TxContextGuard},
            validation::{
                richtext_attrs::run_before_validate_on_node_attrs, validate_fields_inner,
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
    pub fn fire_before_read(&self, hooks: &Hooks, ctx: HookContext) -> Result<()> {
        self.run_hooks(hooks, HookEvent::BeforeRead, ctx)?;

        Ok(())
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
            .map(|doc| apply_after_read_inner(&lua, ctx, doc))
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
        self.run_richtext_node_attr_before_validate(fields, &mut ctx.data, &ctx.collection);

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
        ctx: HookContext,
        conn: &dyn DbConnection,
        infra: Option<LuaCrudInfra>,
    ) -> Result<HookContext> {
        // Run field-level after_change hooks (with CRUD access)
        if matches!(event, HookEvent::AfterChange) {
            let has_field_hooks = has_field_hooks_for_event(fields, &FieldHookEvent::AfterChange);

            if has_field_hooks {
                let mut data = ctx.data.clone();
                let wctx = FieldWriteCtx::builder(conn)
                    .user(ctx.user.as_ref())
                    .ui_locale(ctx.ui_locale.as_deref())
                    .infra(infra.clone())
                    .build();

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
            }
        }

        // Run collection-level + registered hooks (with CRUD access)
        self.run_hooks_with_conn(hooks, event, ctx, conn, infra)
    }

    /// Run `before_validate` hooks on richtext node attrs within field data.
    ///
    /// Walks the entire field tree (Groups with `__` prefix, Row/Collapsible transparent,
    /// Tabs transparent) to find all Richtext fields with custom nodes.
    fn run_richtext_node_attr_before_validate(
        &self,
        fields: &[FieldDefinition],
        data: &mut DocumentFields,
        collection: &str,
    ) {
        let richtext_fields = collect_richtext_fields_recursive(fields, "");

        if richtext_fields.is_empty() {
            return;
        }

        let has_any_hooks = richtext_fields.iter().any(|(f, _)| {
            f.admin.nodes.iter().any(|node_name| {
                self.registry
                    .get_richtext_node(node_name)
                    .is_some_and(|nd| nd.attrs.iter().any(|a| !a.hooks.before_validate.is_empty()))
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

        for (field, data_key) in &richtext_fields {
            if let Some(Value::String(content)) = data.get(data_key.as_str()) {
                let new_content = run_before_validate_on_node_attrs(
                    &lua,
                    content,
                    field,
                    &self.registry,
                    collection,
                );
                if new_content != *content {
                    data.insert(data_key.clone(), Value::String(new_content));
                }
            }
        }
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
            soft_delete: ctx.soft_delete,
            collection_required_locales: ctx.collection_required_locales,
            user: ctx.user,
            ui_locale: ctx.ui_locale,
        };
        validate_fields_inner(&lua, fields, data, &enriched_ctx)
    }
}

/// Walk the field tree recursively and collect all Richtext fields that have
/// custom nodes configured, along with their data key (the `__`-separated
/// column name used in the flat data map).
///
/// - **Group**: adds `group__` prefix to children
/// - **Row / Collapsible**: transparent — passes through unchanged
/// - **Tabs**: transparent — iterates each tab's fields
fn collect_richtext_fields_recursive<'a>(
    fields: &'a [FieldDefinition],
    prefix: &str,
) -> Vec<(&'a FieldDefinition, String)> {
    let mut out = Vec::new();
    for field in fields {
        match field.field_type {
            FieldType::Group => {
                let new_prefix = prefixed_name(prefix, &field.name);
                out.extend(collect_richtext_fields_recursive(
                    &field.fields,
                    &new_prefix,
                ));
            }
            FieldType::Row | FieldType::Collapsible => {
                out.extend(collect_richtext_fields_recursive(&field.fields, prefix));
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    out.extend(collect_richtext_fields_recursive(&tab.fields, prefix));
                }
            }
            FieldType::Richtext if !field.admin.nodes.is_empty() => {
                out.push((field, prefixed_name(prefix, &field.name)));
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{FieldAdmin, FieldTab};
    fn rt_field(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, crate::core::FieldType::Richtext)
            .admin(FieldAdmin::builder().nodes(vec!["cta".to_string()]).build())
            .build()
    }

    fn text_field(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, crate::core::FieldType::Text).build()
    }

    #[test]
    fn collect_top_level_richtext() {
        let fields = vec![rt_field("content"), text_field("title")];
        let result = collect_richtext_fields_recursive(&fields, "");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].1, "content");
    }

    #[test]
    fn collect_richtext_inside_group() {
        let fields = vec![
            FieldDefinition::builder("seo", crate::core::FieldType::Group)
                .fields(vec![rt_field("body")])
                .build(),
        ];
        let result = collect_richtext_fields_recursive(&fields, "");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].1, "seo__body");
    }

    #[test]
    fn collect_richtext_inside_tabs() {
        let fields = vec![
            FieldDefinition::builder("layout", crate::core::FieldType::Tabs)
                .tabs(vec![FieldTab::new("Tab1", vec![rt_field("content")])])
                .build(),
        ];
        let result = collect_richtext_fields_recursive(&fields, "");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].1, "content");
    }

    #[test]
    fn collect_richtext_group_inside_tabs() {
        let fields = vec![
            FieldDefinition::builder("layout", crate::core::FieldType::Tabs)
                .tabs(vec![FieldTab::new(
                    "SEO",
                    vec![
                        FieldDefinition::builder("seo", crate::core::FieldType::Group)
                            .fields(vec![rt_field("desc")])
                            .build(),
                    ],
                )])
                .build(),
        ];
        let result = collect_richtext_fields_recursive(&fields, "");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].1, "seo__desc");
    }

    #[test]
    fn collect_skips_richtext_without_nodes() {
        let fields =
            vec![FieldDefinition::builder("body", crate::core::FieldType::Richtext).build()];
        let result = collect_richtext_fields_recursive(&fields, "");
        assert!(
            result.is_empty(),
            "richtext without nodes should be skipped"
        );
    }
}
