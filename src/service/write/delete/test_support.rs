//! Test fixtures shared by the delete tests.

use rusqlite::Connection;

use crate::{
    core::{
        CollectionDefinition, DocumentFields, FieldDefinition, FieldType, Hooks, ValidationError,
        collection::Auth,
    },
    db::{AccessResult, DbConnection},
    hooks::{AccessCheckInput, HookContext, HookEvent, ValidationCtx},
    service::{FieldReadStrip, hooks::WriteHooks},
};

/// Allow-all hooks that do not run any user-defined Lua.
pub(super) struct AllowAllWriteHooks;

impl WriteHooks for AllowAllWriteHooks {
    fn run_before_write(
        &self,
        _hooks: &Hooks,
        _fields: &[FieldDefinition],
        ctx: HookContext,
        _val_ctx: &ValidationCtx,
    ) -> anyhow::Result<HookContext> {
        Ok(ctx)
    }

    fn run_after_write(
        &self,
        _hooks: &Hooks,
        _fields: &[FieldDefinition],
        _event: HookEvent,
        ctx: HookContext,
        _conn: &dyn DbConnection,
    ) -> anyhow::Result<HookContext> {
        Ok(ctx)
    }

    fn run_hooks_with_conn(
        &self,
        _hooks: &Hooks,
        _event: HookEvent,
        ctx: HookContext,
        _conn: &dyn DbConnection,
    ) -> anyhow::Result<HookContext> {
        Ok(ctx)
    }

    fn check_access(&self, _input: &AccessCheckInput<'_>) -> anyhow::Result<AccessResult> {
        Ok(AccessResult::Allowed)
    }

    fn validate_fields(
        &self,
        _fields: &[FieldDefinition],
        _data: &DocumentFields,
        _ctx: &ValidationCtx,
    ) -> std::result::Result<(), ValidationError> {
        Ok(())
    }
}

impl FieldReadStrip for AllowAllWriteHooks {}

pub(super) fn setup_auth_collection() -> (Connection, CollectionDefinition) {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE users (
            id TEXT PRIMARY KEY,
            email TEXT,
            _ref_count INTEGER DEFAULT 0,
            _session_version INTEGER DEFAULT 0,
            created_at TEXT,
            updated_at TEXT
        );
        INSERT INTO users (id, email) VALUES ('u1', 'a@b.com');",
    )
    .unwrap();

    let mut def = CollectionDefinition::new("users");
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("email", FieldType::Email)
            .unique(true)
            .build(),
    ];
    def.auth = Some(Auth {
        enabled: true,
        ..Default::default()
    });

    (conn, def)
}
