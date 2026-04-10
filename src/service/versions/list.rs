//! Version list operation with pagination.

use crate::{
    core::{Document, document::VersionSnapshot},
    db::{AccessResult, DbConnection, query},
    service::{ServiceError, hooks::ReadHooks},
};

/// List version snapshots for a document/global with pagination and access control.
///
/// Checks read access before listing. Returns `(versions, total_count)`.
/// The `table` parameter is the collection slug for collections or
/// the global table name for globals.
#[allow(clippy::too_many_arguments)]
pub fn list_versions(
    conn: &dyn DbConnection,
    hooks: &dyn ReadHooks,
    table: &str,
    parent_id: &str,
    access_ref: Option<&str>,
    user: Option<&Document>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<(Vec<VersionSnapshot>, i64), ServiceError> {
    let access = hooks.check_access(access_ref, user, Some(parent_id), None)?;
    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Read access denied".into()));
    }

    let total = query::count_versions(conn, table, parent_id)?;
    let versions = query::list_versions(conn, table, parent_id, limit, offset)?;

    Ok((versions, total))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    use anyhow::Result;
    use rusqlite::Connection;
    use serde_json::{Value, json};

    use crate::{
        config::LocaleConfig,
        core::{
            CollectionDefinition, Document, FieldDefinition,
            collection::{Hooks, VersionsConfig},
            field::FieldType,
        },
        db::AccessResult,
        hooks::{HookContext, HookEvent, ValidationCtx, lifecycle::AfterReadCtx},
        service::{
            hooks::{ReadHooks, WriteHooks},
            versions::restore_collection_version_core,
        },
    };

    /// Noop implementation of ReadHooks for unit tests — always allows access.
    struct NoopReadHooks;

    impl ReadHooks for NoopReadHooks {
        fn before_read(&self, _hooks: &Hooks, _slug: &str, _op: &str) -> Result<()> {
            Ok(())
        }

        fn after_read_one(&self, _ctx: &AfterReadCtx, doc: Document) -> Document {
            doc
        }

        fn check_access(
            &self,
            _access_ref: Option<&str>,
            _user: Option<&Document>,
            _id: Option<&str>,
            _data: Option<&HashMap<String, Value>>,
        ) -> Result<AccessResult> {
            Ok(AccessResult::Allowed)
        }

        fn field_read_denied(
            &self,
            _fields: &[FieldDefinition],
            _user: Option<&Document>,
        ) -> Vec<String> {
            Vec::new()
        }
    }

    /// Noop implementation of WriteHooks for unit tests — always allows access.
    struct NoopWriteHooks;

    impl WriteHooks for NoopWriteHooks {
        fn run_before_write(
            &self,
            _hooks: &Hooks,
            _fields: &[FieldDefinition],
            ctx: HookContext,
            _val_ctx: &ValidationCtx,
        ) -> Result<HookContext> {
            Ok(ctx)
        }

        fn run_after_write(
            &self,
            _hooks: &Hooks,
            _fields: &[FieldDefinition],
            _event: HookEvent,
            ctx: HookContext,
            _conn: &dyn DbConnection,
        ) -> Result<HookContext> {
            Ok(ctx)
        }

        fn run_hooks_with_conn(
            &self,
            _hooks: &Hooks,
            _event: HookEvent,
            ctx: HookContext,
            _conn: &dyn DbConnection,
        ) -> Result<HookContext> {
            Ok(ctx)
        }

        fn field_read_denied(
            &self,
            _fields: &[FieldDefinition],
            _user: Option<&Document>,
        ) -> Vec<String> {
            Vec::new()
        }

        fn check_access(
            &self,
            _access_ref: Option<&str>,
            _user: Option<&Document>,
            _id: Option<&str>,
            _data: Option<&HashMap<String, Value>>,
        ) -> Result<AccessResult> {
            Ok(AccessResult::Allowed)
        }

        fn field_write_denied(
            &self,
            _fields: &[FieldDefinition],
            _user: Option<&Document>,
            _operation: &str,
        ) -> Vec<String> {
            Vec::new()
        }
    }

    fn setup_versioned_collection() -> (Connection, CollectionDefinition) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                _status TEXT DEFAULT 'published',
                created_at TEXT,
                updated_at TEXT
            );
            CREATE TABLE _versions_posts (
                id TEXT PRIMARY KEY,
                _parent TEXT,
                _version INTEGER,
                _status TEXT,
                _latest INTEGER DEFAULT 0,
                snapshot TEXT
            );
            INSERT INTO posts (id, title) VALUES ('p1', 'Original Title');",
        )
        .unwrap();

        let mut def = CollectionDefinition::new("posts");
        def.timestamps = true;
        def.fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];
        def.versions = Some(VersionsConfig {
            drafts: false,
            max_versions: 0,
        });

        (conn, def)
    }

    #[test]
    fn list_versions_empty() {
        let (conn, _def) = setup_versioned_collection();
        let (versions, total) =
            list_versions(&conn, &NoopReadHooks, "posts", "p1", None, None, None, None).unwrap();
        assert_eq!(total, 0);
        assert!(versions.is_empty());
    }

    #[test]
    fn list_versions_with_data() {
        let (conn, _def) = setup_versioned_collection();
        conn.execute_batch(
            "INSERT INTO _versions_posts (id, _parent, _version, _status, _latest, snapshot) \
             VALUES ('v1', 'p1', 1, 'published', 0, '{\"title\": \"V1\"}'),
                    ('v2', 'p1', 2, 'published', 1, '{\"title\": \"V2\"}');",
        )
        .unwrap();

        let (versions, total) =
            list_versions(&conn, &NoopReadHooks, "posts", "p1", None, None, None, None).unwrap();
        assert_eq!(total, 2);
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[0].version, 2, "should be newest first");
    }

    #[test]
    fn list_versions_pagination() {
        let (conn, _def) = setup_versioned_collection();
        conn.execute_batch(
            "INSERT INTO _versions_posts (id, _parent, _version, _status, _latest, snapshot) \
             VALUES ('v1', 'p1', 1, 'published', 0, '{}'),
                    ('v2', 'p1', 2, 'published', 0, '{}'),
                    ('v3', 'p1', 3, 'published', 1, '{}');",
        )
        .unwrap();

        let (versions, total) = list_versions(
            &conn,
            &NoopReadHooks,
            "posts",
            "p1",
            None,
            None,
            Some(2),
            Some(0),
        )
        .unwrap();
        assert_eq!(total, 3);
        assert_eq!(versions.len(), 2);
    }

    #[test]
    fn restore_collection_version_not_found() {
        let (conn, def) = setup_versioned_collection();
        let lc = LocaleConfig::default();
        let wh = NoopWriteHooks;
        let result = restore_collection_version_core(
            &conn,
            &wh,
            "posts",
            &def,
            "p1",
            "nonexistent",
            &lc,
            None,
        );
        assert!(matches!(result, Err(ServiceError::NotFound(_))));
    }

    #[test]
    fn restore_collection_version_success() {
        let (conn, def) = setup_versioned_collection();
        let snapshot = json!({"title": "Restored Title"}).to_string();
        conn.execute(
            "INSERT INTO _versions_posts (id, _parent, _version, _status, _latest, snapshot) \
             VALUES ('v1', 'p1', 1, 'published', 1, ?1)",
            [&snapshot],
        )
        .unwrap();

        let lc = LocaleConfig::default();
        let wh = NoopWriteHooks;
        let doc = restore_collection_version_core(&conn, &wh, "posts", &def, "p1", "v1", &lc, None)
            .unwrap();
        assert_eq!(doc.get_str("title"), Some("Restored Title"));
    }
}
