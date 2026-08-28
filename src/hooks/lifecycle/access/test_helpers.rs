//! Shared test fixtures for `access::collection` and `access::field` tests.

#![cfg(test)]

use std::collections::HashMap;

use mlua::Lua;
use serde_json::json;

use crate::core::{
    Document, FieldDefinition, FieldType, document::DocumentBuilder, field::FieldAccess,
};

pub(super) fn setup_lua() -> Lua {
    let lua = Lua::new();
    lua.load(
        r#"
        local access = {}

        function access.allow(ctx)

            return true
        end

        function access.deny(ctx)

            return false
        end

        function access.return_nil(ctx)

            return nil
        end

        function access.return_number(ctx)

            return 42
        end

        function access.constrained_string(ctx)

            return { status = "published" }
        end

        function access.constrained_integer(ctx)

            return { priority = 1 }
        end

        function access.constrained_number(ctx)

            return { score = 3.14 }
        end

        function access.constrained_ops(ctx)

            return { score = { greater_than = "50" } }
        end

        function access.constrained_multi_ops(ctx)

            return { score = { greater_than = "10", less_than = "100" } }
        end

        function access.constrained_ignore_bool(ctx)

            return { active = true }
        end

        function access.return_empty_table(ctx)

            return {}
        end

        function access.nil_keyed_constraint(ctx)

            -- The classic multi-tenant footgun: when ctx.user.tenant_id is nil
            -- the key is dropped and this is `{}`, NOT a tenant filter.
            return { tenant_id = ctx.user and ctx.user.tenant_id }
        end

        function access.empty_operator_table(ctx)

            -- Non-empty outer table, but the value parses to zero operators.
            return { score = {} }
        end

        function access.check_user(ctx)

            if ctx.user and ctx.user.role == "admin" then

                return true
            end

            return false
        end

        function access.check_id(ctx)

            if ctx.id == "doc-123" then

                return true
            end

            return false
        end

        function access.check_data(ctx)

            if ctx.data and ctx.data.title == "test" then

                return true
            end

            return false
        end

        function access.throw_error(ctx)
            error("access check failed!")
        end

        -- Data-aware field access: allow only when this field's OWN level
        -- (ctx.data, the array/blocks row at top level the document) has
        -- kind == "public". Proves per-row `ctx.data` wiring.
        function access.allow_if_kind_public(ctx)
            return ctx.data ~= nil and ctx.data.kind == "public"
        end

        -- Data-aware field access: allow only when the FULL document
        -- (ctx.document) is published — stable as the walk descends into rows.
        function access.allow_if_doc_published(ctx)
            return ctx.document ~= nil and ctx.document.status == "published"
        end

        function access.check_locale(ctx)
            if ctx.locale == "en" then
                return true
            end
            return false
        end

        -- Constrains on the system `_status` column — only legal when the
        -- operation itself injects `_status` (injecting_status = true).
        function access.constrained_status(ctx)
            return { _status = "published" }
        end

        package.loaded["test_access"] = access
    "#,
    )
    .exec()
    .unwrap();
    lua
}

pub(super) fn make_field(name: &str, access: FieldAccess) -> FieldDefinition {
    FieldDefinition::builder(name, FieldType::Text)
        .access(access)
        .build()
}

pub(super) fn make_user_doc(role: &str) -> Document {
    let mut fields = HashMap::new();
    fields.insert("role".to_string(), json!(role));
    fields.insert("email".to_string(), json!("user@test.com"));
    DocumentBuilder::new("user-1").fields(fields).build()
}
