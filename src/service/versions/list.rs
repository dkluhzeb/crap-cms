//! Version list operation with pagination.

use crate::{
    core::document::VersionSnapshot,
    db::{AccessResult, query},
    hooks::AccessCheckInput,
    service::{
        Def, ListVersionsInput, PaginatedResult, ServiceContext, ServiceError,
        helpers::enforce_access_constraints,
        versions::gate::{check_versions_gate, draft_snapshots_visible, reject_global_filter},
    },
};

/// List version snapshots for a document/global with pagination and access control.
///
/// Checks read access before listing. Returns paginated result with page metadata.
/// Derives the version table name from `ctx.slug` + `ctx.def`.
///
/// # Errors
///
/// Returns `AccessDenied` or `HookError`, or a backend error if the SELECT fails.
pub fn list_versions(
    ctx: &ServiceContext,
    input: &ListVersionsInput,
) -> Result<PaginatedResult<VersionSnapshot>, ServiceError> {
    let resolved = ctx.resolve_conn()?;
    let conn = resolved.as_ref();
    let hooks = ctx.read_hooks()?;
    let table = ctx.version_table();

    // The `access.versions` toggle gates history access at all (default allow);
    // the read/draft composite below then scopes *which* snapshots are visible.
    check_versions_gate(ctx, hooks, Some(input.parent_id), "find")?;

    // Listing a document's version history is gated by `access.read` — you can
    // see the published version history if you can read the document. DRAFT
    // version snapshots are additionally gated at edit level
    // (`access.draft ?? access.update`): a reader without that access sees only
    // published versions, mirroring document draft visibility.
    let access = hooks.check_access(&AccessCheckInput {
        access: ctx.read_access_ref(),
        user: ctx.user,
        id: Some(input.parent_id),
        data: None,
        locale: None,
        operation: "find",
        collection: ctx.slug,
        ui_locale: None,
    })?;

    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Read access denied".into()));
    }

    // Draft versions need read AND edit-level access. A denied draft check
    // restricts the listing to published snapshots; a *constrained* draft rule
    // ("preview only your own drafts") is enforced against the parent document,
    // so a non-match downgrades to published-only instead of leaking another
    // owner's draft snapshots.
    let draft_access = hooks.check_access(&AccessCheckInput {
        access: ctx.draft_access_ref(),
        user: ctx.user,
        id: Some(input.parent_id),
        data: None,
        locale: None,
        operation: "find",
        collection: ctx.slug,
        ui_locale: None,
    })?;
    let published_only = !draft_snapshots_visible(ctx, &draft_access, input.parent_id)?;

    // Constrained read handling: for collections we enforce the filters against
    // the parent document id (reusing the count-based helper); for globals the
    // filter table is meaningless (single row) and is rejected.
    if matches!(access, AccessResult::Constrained(_)) {
        match &ctx.def {
            Def::Global(_) => return Err(reject_global_filter(ctx.slug)),
            _ => enforce_access_constraints(ctx, input.parent_id, &access, "Read", false)?,
        }
    }

    let total = query::count_versions(conn, &table, input.parent_id, published_only)?;
    let mut versions = query::list_versions(
        conn,
        &table,
        input.parent_id,
        published_only,
        input.limit,
        input.offset,
    )?;

    // Strip read-denied + API-hidden fields from every snapshot — parity with
    // `find_version_by_id`; otherwise denied fields leak through the list.
    let mut denied = hooks.field_read_denied(ctx.fields()?, ctx.user, None);
    denied.extend(crate::service::helpers::collect_api_hidden_field_names(
        ctx.fields()?,
        "",
    ));
    if !denied.is_empty() {
        for version in &mut versions {
            super::find::strip_snapshot_fields(&mut version.snapshot, &denied);
        }
    }

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

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;

    use anyhow::Result;
    use rusqlite::Connection;
    use serde_json::json;

    use crate::{
        config::LocaleConfig,
        core::{
            CollectionDefinition, Document, DocumentFields, FieldDefinition, FieldDenial,
            FieldType, Hooks, ValidationError, VersionsConfig,
        },
        db::{AccessResult, DbConnection},
        hooks::{HookContext, HookEvent, ValidationCtx, lifecycle::AfterReadCtx},
        service::{
            ServiceContext,
            hooks::{ReadHooks, WriteHooks},
            versions::restore_collection_version_core,
        },
    };

    /// Noop implementation of `ReadHooks` for unit tests — always allows access.
    struct NoopReadHooks;

    impl ReadHooks for NoopReadHooks {
        fn before_read(
            &self,
            _hooks: &Hooks,
            _slug: &str,
            _op: &str,
            _locale: Option<&str>,
        ) -> Result<()> {
            Ok(())
        }

        fn after_read_one(&self, _ctx: &AfterReadCtx, doc: Document) -> Document {
            doc
        }

        fn check_access(&self, _input: &AccessCheckInput<'_>) -> Result<AccessResult> {
            Ok(AccessResult::Allowed)
        }

        fn field_read_denied(
            &self,
            _fields: &[FieldDefinition],
            _user: Option<&Document>,
            _locale: Option<&str>,
        ) -> Vec<FieldDenial> {
            Vec::new()
        }
    }

    /// Read hooks that allow access only when the access ref is the `read`
    /// function — so a version read (which resolves to the edit-level
    /// `draft`/`update` ref) is denied.
    struct OnlyReadFnAllowed;

    impl ReadHooks for OnlyReadFnAllowed {
        fn before_read(
            &self,
            _hooks: &Hooks,
            _slug: &str,
            _op: &str,
            _locale: Option<&str>,
        ) -> Result<()> {
            Ok(())
        }

        fn after_read_one(&self, _ctx: &AfterReadCtx, doc: Document) -> Document {
            doc
        }

        fn check_access(&self, input: &AccessCheckInput<'_>) -> Result<AccessResult> {
            let is_read_fn = input.access.map(crate::core::HookRef::reference) == Some("read_fn");
            Ok(if is_read_fn {
                AccessResult::Allowed
            } else {
                AccessResult::Denied
            })
        }

        fn field_read_denied(
            &self,
            _fields: &[FieldDefinition],
            _user: Option<&Document>,
            _locale: Option<&str>,
        ) -> Vec<FieldDenial> {
            Vec::new()
        }
    }

    /// Noop implementation of `WriteHooks` for unit tests — always allows access.
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
            _locale: Option<&str>,
        ) -> Vec<FieldDenial> {
            Vec::new()
        }

        fn check_access(&self, _input: &AccessCheckInput<'_>) -> Result<AccessResult> {
            Ok(AccessResult::Allowed)
        }

        fn field_write_denied(
            &self,
            _fields: &[FieldDefinition],
            _user: Option<&Document>,
            _locale: Option<&str>,
            _operation: &str,
        ) -> Vec<FieldDenial> {
            Vec::new()
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

    /// Listing versions is gated by `read`; a draft version additionally needs
    /// edit-level access. A reader with `read` but not edit access sees only
    /// published versions, while an editor sees draft versions too.
    #[test]
    fn version_list_filters_drafts_for_readers_without_edit_access() {
        let (conn, mut def) = setup_versioned_collection();
        def.access.read = Some(crate::core::HookRef::new("read_fn"));
        def.access.update = Some(crate::core::HookRef::new("update_fn"));
        conn.execute_batch(
            "INSERT INTO _versions_posts (id, _parent, _version, _status, _latest, snapshot) \
             VALUES ('v1', 'p1', 1, 'published', 0, '{}'),
                    ('v2', 'p1', 2, 'draft', 1, '{}');",
        )
        .unwrap();

        // Reader: `read_fn` allowed, edit-level (`update_fn`) denied → published only.
        let reader = OnlyReadFnAllowed;
        let reader_ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .read_hooks(&reader)
            .build();
        let res = list_versions(&reader_ctx, &ListVersionsInput::builder("p1").build()).unwrap();
        assert_eq!(res.total, 1, "reader sees only published versions");
        assert_eq!(res.docs.len(), 1);
        assert_eq!(res.docs[0].status, "published");

        // Editor (allow-all hooks pass the edit-level check too) → all versions.
        let editor = NoopReadHooks;
        let editor_ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .read_hooks(&editor)
            .build();
        let res2 = list_versions(&editor_ctx, &ListVersionsInput::builder("p1").build()).unwrap();
        assert_eq!(res2.total, 2, "editor sees draft versions too");
    }

    /// Read hooks where `read` is `Allowed` (published readable by anyone) but
    /// `draft` returns `Constrained(author = "me")` — the canonical
    /// "preview only your own drafts" rule.
    struct DraftConstrainedToMe;

    impl ReadHooks for DraftConstrainedToMe {
        fn before_read(&self, _: &Hooks, _: &str, _: &str, _: Option<&str>) -> Result<()> {
            Ok(())
        }

        fn after_read_one(&self, _: &AfterReadCtx, doc: Document) -> Document {
            doc
        }

        fn check_access(&self, input: &AccessCheckInput<'_>) -> Result<AccessResult> {
            if input.access.map(crate::core::HookRef::reference) == Some("read_fn") {
                return Ok(AccessResult::Allowed);
            }
            Ok(AccessResult::Constrained(vec![
                crate::db::FilterClause::Single(crate::db::Filter {
                    field: "author".into(),
                    op: crate::db::FilterOp::Equals("me".into()),
                }),
            ]))
        }

        fn field_read_denied(
            &self,
            _: &[FieldDefinition],
            _: Option<&Document>,
            _: Option<&str>,
        ) -> Vec<FieldDenial> {
            Vec::new()
        }
    }

    /// Regression: a `Constrained` draft rule must be enforced against the
    /// parent document on BOTH version surfaces. Treating `Constrained` as full
    /// draft access (the old `matches!(.., Denied)` check) leaked another
    /// owner's draft version snapshots to anyone who could read published
    /// content — the version-surface sibling of the `find_by_id`
    /// constrained-draft snapshot leak.
    #[test]
    fn constrained_draft_does_not_leak_other_owners_draft_versions() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                author TEXT,
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
            INSERT INTO posts (id, title, author) VALUES ('p1', 'Theirs', 'other');
            INSERT INTO posts (id, title, author) VALUES ('p2', 'Mine', 'me');
            INSERT INTO _versions_posts (id, _parent, _version, _status, _latest, snapshot)
            VALUES ('p1v1', 'p1', 1, 'published', 0, '{}'),
                   ('p1v2', 'p1', 2, 'draft', 1, '{}'),
                   ('p2v1', 'p2', 1, 'published', 0, '{}'),
                   ('p2v2', 'p2', 2, 'draft', 1, '{}');",
        )
        .unwrap();

        let mut def = CollectionDefinition::new("posts");
        def.timestamps = true;
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("author", FieldType::Text).build(),
        ];
        def.versions = Some(VersionsConfig {
            drafts: true,
            max_versions: 0,
        });
        def.access.read = Some(crate::core::HookRef::new("read_fn"));
        def.access.draft = Some(crate::core::HookRef::new("draft_fn"));

        let hooks = DraftConstrainedToMe;

        // p1 authored by "other": the draft constraint does NOT match → the
        // viewer sees only the published version; the draft snapshot is withheld.
        let ctx1 = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .read_hooks(&hooks)
            .build();
        let listed = list_versions(&ctx1, &ListVersionsInput::builder("p1").build()).unwrap();
        assert_eq!(
            listed.total, 1,
            "another owner's draft version must not leak"
        );
        assert_eq!(listed.docs[0].status, "published");
        assert!(
            crate::service::versions::find::find_version_by_id(&ctx1, "p1v2")
                .unwrap()
                .is_none(),
            "fetching another owner's draft version by id must be hidden"
        );

        // p2 authored by "me": the draft constraint matches → drafts visible.
        let ctx2 = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .read_hooks(&hooks)
            .build();
        let mine = list_versions(&ctx2, &ListVersionsInput::builder("p2").build()).unwrap();
        assert_eq!(mine.total, 2, "own draft version is still visible");
        assert!(
            crate::service::versions::find::find_version_by_id(&ctx2, "p2v2")
                .unwrap()
                .is_some(),
            "fetching own draft version by id still works"
        );
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
