//! Inline-VM [`WriteHooks`] for Lua CRUD calls made from hooks.

use anyhow::Result;
use mlua::Lua;
use serde_json::{Map, Value};

use crate::{
    core::{Builder, Document, DocumentFields, FieldDefinition, Hooks, Registry},
    db::{AccessResult, DbConnection, LocaleContext},
    hooks::{
        HookContext, HookEvent, ValidationCtx,
        lifecycle::{
            AccessCheckInput, FieldHookEvent, FieldHookMeta, FieldHooksCall,
            access::{
                ReadStripInput, WriteStripInput, check_collection_access,
                strip_read_access_with_lua, strip_write_access_with_lua,
            },
            apply_node_attr_before_validate, run_field_hooks_inner, run_hooks_inner,
            validate_write_fields,
        },
    },
    service::hooks::FieldReadStrip,
};

use super::{ValidateResult, WriteHooks};

/// Inline Lua VM write hook execution for Lua CRUD hooks.
///
/// No `user`/`ui_locale` here: hook contexts get both from the service
/// context (`ctx.user` / the write input), and access checks receive the user
/// via [`AccessCheckInput`] — the fields existed once, were never read, and
/// every codec dutifully filled them for no effect.
#[derive(Builder)]
pub struct LuaWriteHooks<'a> {
    #[builder(required)]
    pub lua: &'a Lua,
    /// Richtext node definitions — required so node-attr `before_validate`
    /// hooks and node-attr validation run on every Lua CRUD write.
    #[builder(required)]
    pub registry: &'a Registry,
    pub override_access: bool,
    /// Whether hooks are enabled (false when hook depth exceeded or `hooks: false` option).
    #[builder(default = true)]
    pub hooks_enabled: bool,
    /// Whether validation should run (`hooks` option from Lua API).
    #[builder(default = true)]
    pub run_validation: bool,
}

impl WriteHooks for LuaWriteHooks<'_> {
    fn runs_delete_hooks(&self, hooks: &Hooks) -> bool {
        // Inline nested-Lua delete path: gate on the hooks-enabled flag and the
        // collection's own delete hooks. Globally-registered delete hooks in
        // the current VM are not probed here (the common case is collection
        // refs); a registered-only delete hook still gets the `id`.
        self.hooks_enabled && (!hooks.before_delete.is_empty() || !hooks.after_delete.is_empty())
    }

    fn registry(&self) -> Option<&Registry> {
        Some(self.registry)
    }

    fn run_before_write(
        &self,
        hooks: &Hooks,
        fields: &[FieldDefinition],
        mut ctx: HookContext,
        val_ctx: &ValidationCtx,
    ) -> Result<HookContext> {
        if self.hooks_enabled {
            run_field_hooks_inner(
                self.lua,
                &mut ctx.data,
                &FieldHooksCall {
                    fields,
                    event: FieldHookEvent::BeforeValidate,
                    collection: &ctx.collection,
                    operation: &ctx.operation,
                    id: ctx.document_id.as_deref(),
                    locale: val_ctx.locale_ctx.map(LocaleContext::access_locale),
                },
            )?;

            apply_node_attr_before_validate(
                self.lua,
                fields,
                &mut ctx.data,
                self.registry,
                &FieldHookMeta {
                    collection: &ctx.collection,
                    operation: &ctx.operation,
                    id: ctx.document_id.as_deref(),
                    locale: val_ctx.locale_ctx.map(LocaleContext::access_locale),
                },
            )?;

            ctx = run_hooks_inner(self.lua, hooks, HookEvent::BeforeValidate, ctx)?;
        }

        if self.run_validation {
            validate_write_fields(self.lua, fields, &ctx.data, val_ctx, self.registry)?;
        }

        if self.hooks_enabled {
            run_field_hooks_inner(
                self.lua,
                &mut ctx.data,
                &FieldHooksCall {
                    fields,
                    event: FieldHookEvent::BeforeChange,
                    collection: &ctx.collection,
                    operation: &ctx.operation,
                    id: ctx.document_id.as_deref(),
                    locale: val_ctx.locale_ctx.map(LocaleContext::access_locale),
                },
            )?;

            ctx = run_hooks_inner(self.lua, hooks, HookEvent::BeforeChange, ctx)?;
        }

        Ok(ctx)
    }

    fn run_after_write(
        &self,
        hooks: &Hooks,
        fields: &[FieldDefinition],
        event: HookEvent,
        mut ctx: HookContext,
        _conn: &dyn DbConnection,
    ) -> Result<HookContext> {
        if !self.hooks_enabled {
            return Ok(ctx);
        }

        if matches!(event, HookEvent::AfterChange) {
            run_field_hooks_inner(
                self.lua,
                &mut ctx.data,
                &FieldHooksCall {
                    fields,
                    event: FieldHookEvent::AfterChange,
                    collection: &ctx.collection,
                    operation: &ctx.operation,
                    id: ctx.document_id.as_deref(),
                    locale: ctx.locale.as_deref(),
                },
            )?;
        }

        run_hooks_inner(self.lua, hooks, event, ctx)
    }

    fn run_hooks_with_conn(
        &self,
        hooks: &Hooks,
        event: HookEvent,
        ctx: HookContext,
        _conn: &dyn DbConnection,
    ) -> Result<HookContext> {
        if !self.hooks_enabled {
            return Ok(ctx);
        }
        run_hooks_inner(self.lua, hooks, event, ctx)
    }

    fn check_access(&self, input: &AccessCheckInput<'_>) -> Result<AccessResult> {
        if self.override_access {
            return Ok(AccessResult::Allowed);
        }
        // Route through the chokepoint (not the bare evaluator) so a write
        // hook's row constraints get the same operator / dotted / locale
        // validation as every other surface.
        check_collection_access(self.lua, input)
    }

    fn strip_write_access_map(
        &self,
        fields: &[FieldDefinition],
        level: &mut Map<String, Value>,
        input: &WriteStripInput<'_>,
    ) {
        if self.override_access {
            return;
        }
        strip_write_access_with_lua(self.lua, fields, level, input);
    }

    fn validate_fields(
        &self,
        fields: &[FieldDefinition],
        data: &DocumentFields,
        ctx: &ValidationCtx,
    ) -> ValidateResult {
        validate_write_fields(self.lua, fields, data, ctx, self.registry)
    }
}

impl FieldReadStrip for LuaWriteHooks<'_> {
    fn strip_read_access_map(
        &self,
        fields: &[FieldDefinition],
        level: &mut Map<String, Value>,
        document: &DocumentFields,
        collection: &str,
        user: Option<&Document>,
        locale: Option<&str>,
    ) {
        if self.override_access {
            return;
        }
        let input = ReadStripInput {
            document,
            collection,
            user,
            locale,
        };
        strip_read_access_with_lua(self.lua, fields, level, &input);
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        core::{FieldAdmin, FieldType, RichtextNodeDef, ValidationError},
        db::InMemoryConn,
    };

    /// Regression: the in-VM Lua CRUD write path validated without the
    /// registry, so a registered richtext node's required attr was never
    /// checked on a `crap.collections.*` write.
    #[test]
    fn lua_write_hooks_check_richtext_node_attrs() {
        let lua = Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE pages (id TEXT PRIMARY KEY, content TEXT)");

        let mut registry = Registry::new();
        registry.register_richtext_node(
            RichtextNodeDef::builder("cta", "CTA")
                .attrs(vec![
                    FieldDefinition::builder("text", FieldType::Text)
                        .required(true)
                        .build(),
                ])
                .build(),
        );

        let fields = vec![
            FieldDefinition::builder("content", FieldType::Richtext)
                .admin(
                    FieldAdmin::builder()
                        .nodes(vec!["cta".to_string()])
                        .richtext_format("json")
                        .build(),
                )
                .build(),
        ];

        let content = r#"{"type":"doc","content":[{"type":"cta","attrs":{"text":""}}]}"#;
        let ctx = HookContext::builder("pages", "create")
            .data(DocumentFields::from_iter([(
                "content".to_string(),
                json!(content),
            )]))
            .build();

        let hooks = LuaWriteHooks::builder(&lua, &registry)
            .hooks_enabled(false)
            .build();
        let val_ctx = ValidationCtx::builder(&conn, "pages").build();

        let err = hooks
            .run_before_write(&Hooks::default(), &fields, ctx, &val_ctx)
            .expect_err("an empty required node attr must fail validation");

        let validation = err
            .downcast_ref::<ValidationError>()
            .expect("a validation error");
        assert_eq!(validation.errors[0].field, "content[cta#0].text");
    }
}
