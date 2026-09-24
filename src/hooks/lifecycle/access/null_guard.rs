//! Fail-closed guard for access rules that read a NULL identity attribute.
//!
//! A NULL field reaches Lua as `nil`, and a `nil`-valued key vanishes from a
//! table constructor — so `{ tenant_id = ctx.user.tenant_id, archived = false }`
//! for a user whose `tenant_id` is NULL is the constraint `{ archived = false }`:
//! it matches every tenant's rows. The empty-constraint rule only catches the
//! single-key form; this guard catches the rest.
//!
//! [`NullReadGuard::install`] puts an `__index` metamethod on the `ctx.user`
//! table (and each nested group table) that has NULL-valued fields. Those keys
//! are absent from the table, so reading one falls through to `__index`,
//! which records the field path and returns `nil` — the script sees exactly
//! what it saw before. When the rule then returns a **filter table**, the
//! access check fails closed (see `check_access_with_lua`). A boolean or `nil`
//! verdict is unaffected: the rule decided explicitly. `rawget(ctx.user, key)`
//! reads a field without recording it.

use std::{
    collections::{BTreeSet, HashMap},
    sync::{Arc, Mutex, PoisonError},
};

use mlua::{Lua, Result as LuaResult, Table, Value};
use serde_json::Value as JsonValue;

use crate::core::Document;

/// NULL-valued `ctx.user` field paths read during one access-rule call.
#[derive(Default)]
pub(super) struct NullReadGuard {
    reads: Arc<Mutex<BTreeSet<String>>>,
}

impl NullReadGuard {
    /// Install the read recorder on `ctx.user` inside `ctx` (the serialized
    /// access context). A no-op when there is no user or no NULL field.
    pub(super) fn install(lua: &Lua, ctx: &Value, user: Option<&Document>) -> LuaResult<Self> {
        let guard = Self::default();

        let (Some(user), Value::Table(ctx)) = (user, ctx) else {
            return Ok(guard);
        };

        let Value::Table(user_tbl) = ctx.raw_get::<Value>("user")? else {
            return Ok(guard);
        };

        guard.install_level(lua, &user_tbl, &user.fields, "")?;

        Ok(guard)
    }

    /// The NULL field paths the rule read, sorted; empty when none.
    pub(super) fn null_reads(&self) -> Vec<String> {
        self.reads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .cloned()
            .collect()
    }

    /// Guard one object level: record reads of its NULL keys, and descend
    /// into nested objects (group values) that have their own.
    fn install_level<'a>(
        &self,
        lua: &Lua,
        tbl: &Table,
        fields: impl IntoIterator<Item = (&'a String, &'a JsonValue)>,
        prefix: &str,
    ) -> LuaResult<()> {
        let mut null_paths: HashMap<String, String> = HashMap::new();

        for (key, value) in fields {
            match value {
                JsonValue::Null => {
                    null_paths.insert(key.clone(), format!("{prefix}{key}"));
                }
                JsonValue::Object(nested) => {
                    if let Value::Table(child) = tbl.raw_get::<Value>(key.as_str())? {
                        self.install_level(lua, &child, nested, &format!("{prefix}{key}."))?;
                    }
                }
                _ => {}
            }
        }

        if null_paths.is_empty() {
            return Ok(());
        }

        let reads = Arc::clone(&self.reads);

        let index = lua.create_function(move |_, (_tbl, key): (Table, Value)| {
            if let Value::String(key) = key
                && let Some(path) = null_paths.get(&*key.to_str()?)
            {
                reads
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(path.clone());
            }

            Ok(Value::Nil)
        })?;

        let meta = lua.create_table()?;
        meta.raw_set("__index", index)?;
        tbl.set_metatable(Some(meta))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{super::collection::check_access_with_lua, super::test_helpers::setup_lua, *};
    use crate::{
        core::{HookRef, document::DocumentBuilder},
        db::AccessResult,
        hooks::{lifecycle::AccessCheckInput, lua_api::to_lua_value},
    };

    /// A user document with the given fields.
    fn user(fields: &JsonValue) -> Document {
        let fields = fields
            .as_object()
            .expect("object")
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect::<HashMap<_, _>>();

        DocumentBuilder::new("u1").fields(fields).build()
    }

    /// Run the `test_access.<rule>` fixture for `user` as a `find` check.
    fn check(rule: &str, user: &Document) -> AccessResult {
        let lua = setup_lua();
        let hook = HookRef::new(format!("test_access.{rule}"));

        check_access_with_lua(
            &lua,
            &AccessCheckInput::builder("find", "posts")
                .access(Some(&hook))
                .user(Some(user))
                .build(),
        )
        .unwrap()
    }

    /// Regression: a multi-key constraint built from a NULL user field lost
    /// that key and widened to `{ archived = false }` — every tenant's rows.
    /// It must deny.
    #[test]
    fn multi_key_constraint_with_a_null_user_field_denies() {
        let tenantless = user(&json!({ "tenant_id": null }));

        let result = check("tenant_and_not_archived", &tenantless);

        assert!(matches!(result, AccessResult::Denied), "{result:?}");
    }

    #[test]
    fn multi_key_constraint_with_a_set_user_field_constrains() {
        let tenant = user(&json!({ "tenant_id": "t1" }));

        let result = check("tenant_and_not_archived", &tenant);

        let AccessResult::Constrained(clauses) = result else {
            panic!("expected Constrained, got {result:?}");
        };
        assert_eq!(clauses.len(), 2, "{clauses:?}");
    }

    /// A boolean rule reading a NULL flag sees `nil` and does not grant.
    #[test]
    fn boolean_rule_reading_a_null_field_is_not_granted() {
        let result = check("admin_flag", &user(&json!({ "is_admin": null })));

        assert!(matches!(result, AccessResult::Denied), "{result:?}");
    }

    /// An explicit `true` after reading a NULL field stands — the guard only
    /// fails constraint tables closed.
    #[test]
    fn explicit_true_after_a_null_read_stays_allowed() {
        let result = check("null_read_then_true", &user(&json!({ "tenant_id": null })));

        assert!(matches!(result, AccessResult::Allowed), "{result:?}");
    }

    /// `rawget` is the escape hatch for probing a possibly-NULL field in a
    /// rule that goes on to return an unrelated constraint.
    #[test]
    fn rawget_probe_keeps_the_constraint() {
        let result = check("rawget_role_then_owner", &user(&json!({ "role": null })));

        assert!(matches!(result, AccessResult::Constrained(_)), "{result:?}");
    }

    /// Serialize `{ user = <doc> }`, install the guard, run `script` with the
    /// table as `ctx`, and return the recorded NULL reads.
    fn reads_after(fields: &JsonValue, script: &str) -> Vec<String> {
        let lua = Lua::new();
        let user = user(fields);

        let ctx = to_lua_value(&lua, &json!({ "user": &user })).unwrap();
        let guard = NullReadGuard::install(&lua, &ctx, Some(&user)).unwrap();

        lua.globals().set("ctx", ctx).unwrap();
        lua.load(script).exec().unwrap();

        guard.null_reads()
    }

    #[test]
    fn reading_a_null_field_is_recorded_and_still_nil() {
        let reads = reads_after(
            &json!({ "tenant_id": null, "role": "editor" }),
            "assert(ctx.user.tenant_id == nil); assert(ctx.user.role == 'editor')",
        );

        assert_eq!(reads, vec!["tenant_id".to_string()]);
    }

    #[test]
    fn set_fields_and_unknown_keys_are_not_recorded() {
        let reads = reads_after(
            &json!({ "tenant_id": "t1", "other": null }),
            "local _ = ctx.user.tenant_id; local _ = ctx.user.no_such_field",
        );

        assert!(reads.is_empty(), "{reads:?}");
    }

    #[test]
    fn rawget_reads_without_recording() {
        let reads = reads_after(
            &json!({ "tenant_id": null }),
            "assert(rawget(ctx.user, 'tenant_id') == nil)",
        );

        assert!(reads.is_empty(), "{reads:?}");
    }

    #[test]
    fn a_null_field_inside_a_group_is_recorded_with_its_path() {
        let reads = reads_after(
            &json!({ "org": { "tenant_id": null, "name": "x" } }),
            "local _ = ctx.user.org.tenant_id",
        );

        assert_eq!(reads, vec!["org.tenant_id".to_string()]);
    }

    #[test]
    fn no_user_installs_nothing() {
        let lua = Lua::new();
        let ctx = to_lua_value(&lua, &json!({ "operation": "find" })).unwrap();

        let guard = NullReadGuard::install(&lua, &ctx, None).unwrap();

        assert!(guard.null_reads().is_empty());
    }
}
