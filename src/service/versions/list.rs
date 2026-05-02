//! Version list operation with pagination.

use crate::{
    core::document::VersionSnapshot,
    db::{AccessResult, query},
    service::{
        Def, ListVersionsInput, PaginatedResult, ServiceContext, ServiceError,
        helpers::enforce_access_constraints,
    },
};

/// List version snapshots for a document/global with pagination and access control.
///
/// Checks read access before listing. Returns paginated result with page metadata.
/// Derives the version table name from `ctx.slug` + `ctx.def`.
pub fn list_versions(
    ctx: &ServiceContext,
    input: &ListVersionsInput,
) -> Result<PaginatedResult<VersionSnapshot>, ServiceError> {
    let resolved = ctx.resolve_conn()?;
    let conn = resolved.as_ref();
    let hooks = ctx.read_hooks()?;
    let table = ctx.version_table();

    let access =
        hooks.check_access(ctx.read_access_ref(), ctx.user, Some(input.parent_id), None)?;

    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Read access denied".into()));
    }

    // Constrained handling depends on the target: for collections we enforce
    // the filters against the parent document id (reusing the count-based
    // helper); for globals the filter table is meaningless (single row) and
    // is rejected with a clear operator-facing error.
    if matches!(access, AccessResult::Constrained(_)) {
        match &ctx.def {
            Def::Global(_) => {
                return Err(ServiceError::HookError(format!(
                    "Access hook for global '{}' returned a filter table; globals don't support filter-based access — return true/false based on ctx.user fields instead.",
                    ctx.slug
                )));
            }
            _ => {
                enforce_access_constraints(ctx, input.parent_id, &access, "Read", false)?;
            }
        }
    }

    let total = query::count_versions(conn, &table, input.parent_id)?;
    let versions = query::list_versions(conn, &table, input.parent_id, input.limit, input.offset)?;

    let limit = input.limit.unwrap_or(total);
    let offset = input.offset.unwrap_or(0);
    let page = if limit > 0 { offset / limit + 1 } else { 1 };

    let pagination = query::PaginationResult::builder(&[], total, limit).page(page, offset);

    Ok(PaginatedResult {
        docs: versions,
        total,
        pagination,
    })
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
            validate::ValidationError,
        },
        db::{AccessResult, DbConnection},
        hooks::{HookContext, HookEvent, ValidationCtx, lifecycle::AfterReadCtx},
        service::{
            ServiceContext,
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

        fn validate_fields(
            &self,
            _fields: &[FieldDefinition],
            _data: &HashMap<String, Value>,
            _ctx: &ValidationCtx,
        ) -> std::result::Result<(), ValidationError> {
            Ok(())
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
        let (conn, def) = setup_versioned_collection();
        let rh = NoopReadHooks;
        let ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .read_hooks(&rh)
            .build();

        let input = ListVersionsInput::builder("p1").build();

        let result = list_versions(&ctx, &input).unwrap();
        assert_eq!(result.total, 0);
        assert!(result.docs.is_empty());
    }

    #[test]
    fn list_versions_with_data() {
        let (conn, def) = setup_versioned_collection();
        conn.execute_batch(
            "INSERT INTO _versions_posts (id, _parent, _version, _status, _latest, snapshot) \
             VALUES ('v1', 'p1', 1, 'published', 0, '{\"title\": \"V1\"}'),
                    ('v2', 'p1', 2, 'published', 1, '{\"title\": \"V2\"}');",
        )
        .unwrap();

        let rh = NoopReadHooks;
        let ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .read_hooks(&rh)
            .build();

        let input = ListVersionsInput::builder("p1").build();

        let result = list_versions(&ctx, &input).unwrap();
        assert_eq!(result.total, 2);
        assert_eq!(result.docs.len(), 2);
        assert_eq!(result.docs[0].version, 2, "should be newest first");
    }

    #[test]
    fn list_versions_pagination() {
        let (conn, def) = setup_versioned_collection();
        conn.execute_batch(
            "INSERT INTO _versions_posts (id, _parent, _version, _status, _latest, snapshot) \
             VALUES ('v1', 'p1', 1, 'published', 0, '{}'),
                    ('v2', 'p1', 2, 'published', 0, '{}'),
                    ('v3', 'p1', 3, 'published', 1, '{}');",
        )
        .unwrap();

        let rh = NoopReadHooks;
        let ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .read_hooks(&rh)
            .build();

        let input = ListVersionsInput::builder("p1")
            .limit(Some(2))
            .offset(Some(0))
            .build();

        let result = list_versions(&ctx, &input).unwrap();
        assert_eq!(result.total, 3);
        assert_eq!(result.docs.len(), 2);
    }

    #[test]
    fn restore_collection_version_not_found() {
        let (conn, def) = setup_versioned_collection();
        let lc = LocaleConfig::default();
        let wh = NoopWriteHooks;
        let ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .write_hooks(&wh)
            .build();
        let result = restore_collection_version_core(&ctx, "p1", "nonexistent", &lc);
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
        let ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .write_hooks(&wh)
            .build();
        let doc = restore_collection_version_core(&ctx, "p1", "v1", &lc).unwrap();
        assert_eq!(doc.get_str("title"), Some("Restored Title"));
    }
}
