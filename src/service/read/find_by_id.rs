//! Single-document lookup by ID with the full read lifecycle.

use crate::{
    core::{CollectionDefinition, Document},
    db::{Filter, FilterClause, FilterOp, LocaleContext, ops},
    service::{
        FindByIdInput, ReadAccessCtx, ReadHooks, ServiceContext, ServiceError, requested_views,
        resolve_trash_scope, resolve_view_scope,
    },
};

use super::post_process::post_process_single;

type Result<T> = std::result::Result<T, ServiceError>;

/// The access-scoped outcome of a single-document lookup: the row constraints to
/// apply, and whether to overlay the latest draft snapshot.
struct ByIdScope {
    constraints: Vec<FilterClause>,
    /// Whether `find_by_id_full` should overlay the draft version snapshot. The
    /// caller's `use_draft` is downgraded to `false` when the draft view is not
    /// visible, so a reader without draft access never sees draft content
    /// overlaid onto a published row.
    use_draft_overlay: bool,
    /// Row constraints a returned draft snapshot must satisfy (the draft view's
    /// filter for a live read, the trash view's for a trash read). Enforced in
    /// memory since the snapshot bypasses the SQL `WHERE` path.
    snapshot_constraints: Vec<FilterClause>,
}

/// Resolve the live (non-trash) lookup scope via the published/draft view union.
fn resolve_live_by_id(
    hooks: &dyn ReadHooks,
    ctx: &ServiceContext,
    def: &CollectionDefinition,
    input: &FindByIdInput,
) -> Result<ByIdScope> {
    let read_ctx = ReadAccessCtx {
        def,
        slug: ctx.slug,
        user: ctx.user,
        id: Some(input.id),
        locale: input.locale_ctx.map(LocaleContext::access_locale),
        operation: "find_by_id",
        ui_locale: ctx.ui_locale.as_deref(),
    };

    let scope = resolve_view_scope(hooks, &read_ctx, requested_views(None, input.use_draft))?;

    if !scope.is_anything_visible() {
        return Err(ServiceError::AccessDenied("Read access denied".into()));
    }

    // Overlay the draft snapshot only when the draft view is actually visible —
    // a read-only caller opting into drafts is downgraded to the published row.
    let use_draft_overlay = input.use_draft && scope.draft_visible();
    // The draft view's row filter bounds the snapshot (a `Constrained` draft rule
    // must still hide other owners' drafts). Captured before `into_filters`
    // consumes the scope.
    let snapshot_constraints = scope.draft_filters().to_vec();

    Ok(ByIdScope {
        constraints: scope.into_filters(),
        use_draft_overlay,
        snapshot_constraints,
    })
}

/// Resolve the trash lookup scope: gated by `access.trash`, independent of the
/// published/draft status union.
fn resolve_trash_by_id(
    hooks: &dyn ReadHooks,
    ctx: &ServiceContext,
    input: &FindByIdInput,
) -> Result<ByIdScope> {
    let def = ctx.collection_def()?;
    let access_constraints = resolve_trash_scope(
        hooks,
        &ReadAccessCtx {
            def,
            slug: ctx.slug,
            user: ctx.user,
            id: Some(input.id),
            locale: input.locale_ctx.map(LocaleContext::access_locale),
            operation: "find_by_id",
            ui_locale: ctx.ui_locale.as_deref(),
        },
    )?;

    // Trash mode must surface only soft-deleted rows. The by-id SQL path runs
    // with `include_deleted = true` (which disables the default
    // `_deleted_at IS NULL` filter) and otherwise emits no lifecycle predicate,
    // so without this guard a LIVE row would be returned through the trash gate
    // (`access.trash ?? update`) — bypassing the `read` policy. Mirrors the list
    // path (`find.rs`) and `trash_branch` (`service::access`).
    let mut constraints = Vec::with_capacity(access_constraints.len() + 1);
    constraints.push(FilterClause::Single(Filter {
        field: "_deleted_at".to_string(),
        op: FilterOp::Exists,
    }));
    constraints.extend(access_constraints.iter().cloned());

    Ok(ByIdScope {
        // Trash never overlays a draft snapshot. The draft overlay is a
        // "pending unpublished edit" view of a *live* document; a draft snapshot
        // carries no `_deleted_at`, so honoring the overlay here would let the
        // snapshot early-return in `find_by_id_full` bypass the `_deleted_at`
        // lifecycle guard above and return a LIVE document's draft content
        // through the trash gate (`access.trash ?? update`) — distinct from, and
        // potentially broader than, the draft gate (`access.draft ?? update`).
        // A trash read returns the soft-deleted main-table row verbatim.
        use_draft_overlay: false,
        // Unused while `use_draft_overlay` is false (the snapshot path is never
        // taken), but kept consistent with the live path for clarity.
        snapshot_constraints: access_constraints,
        constraints,
    })
}

/// Look up a single document by ID with the full read lifecycle.
///
/// Steps: access check -> `before_read` -> `find_by_id` -> post-process.
///
/// # Errors
///
/// Returns service-layer errors (access denied, hook errors) or a backend
/// error if the SELECT or hydration fails.
pub fn find_document_by_id(
    ctx: &ServiceContext,
    input: &FindByIdInput,
) -> Result<Option<Document>> {
    let resolved = ctx.resolve_conn()?;
    let conn = resolved.as_ref();
    let hooks = ctx.read_hooks()?;
    let def = ctx.collection_def()?;

    let scope = if input.include_deleted {
        resolve_trash_by_id(hooks, ctx, input)?
    } else {
        resolve_live_by_id(hooks, ctx, def, input)?
    };

    let constraints = (!scope.constraints.is_empty()).then_some(scope.constraints);

    let req_context = hooks.before_read(
        &def.hooks,
        ctx.slug,
        "find_by_id",
        input.locale_ctx.map(LocaleContext::access_locale),
    )?;

    let Some(mut doc) = ops::find_by_id_full(ops::FindByIdFullParams {
        conn,
        slug: ctx.slug,
        def,
        id: input.id,
        locale_ctx: input.locale_ctx,
        constraints,
        snapshot_constraints: scope.snapshot_constraints,
        use_draft: scope.use_draft_overlay,
        include_deleted: input.include_deleted,
    })?
    else {
        return Ok(None);
    };

    post_process_single(ctx, conn, &mut doc, input, "find_by_id", req_context);

    Ok(Some(doc))
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use anyhow::Result;
    use rusqlite::Connection;

    use super::*;
    use crate::{
        core::{
            CollectionDefinition, Document, FieldDefinition, FieldType, HookRef, Hooks, ReqContext,
            collection::VersionsConfig,
        },
        db::{AccessResult, Filter, FilterClause, FilterOp},
        hooks::{AccessCheckInput, lifecycle::AfterReadCtx},
        service::{ServiceContext, hooks::ReadHooks},
    };

    /// Read hooks that allow access only when the configured access ref is the
    /// collection's `read` function — so a draft read (which resolves to the
    /// `update`/`draft` ref) is denied. Used to verify the access *gate* a read
    /// uses, independent of the row content.
    struct OnlyReadFnAllowed;

    impl ReadHooks for OnlyReadFnAllowed {
        fn before_read(
            &self,
            _hooks: &Hooks,
            _slug: &str,
            _op: &str,
            _locale: Option<&str>,
        ) -> Result<ReqContext> {
            Ok(ReqContext::new())
        }

        fn after_read_one(&self, _ctx: &AfterReadCtx, doc: Document) -> Document {
            doc
        }

        fn check_access(&self, input: &AccessCheckInput<'_>) -> Result<AccessResult> {
            let is_read_fn = input.access.map(HookRef::reference) == Some("read_fn");
            Ok(if is_read_fn {
                AccessResult::Allowed
            } else {
                AccessResult::Denied
            })
        }
    }

    /// Always-allow read hooks so the test exercises the draft-visibility
    /// filter rather than access control.
    struct NoopReadHooks;

    impl ReadHooks for NoopReadHooks {
        fn before_read(
            &self,
            _hooks: &Hooks,
            _slug: &str,
            _op: &str,
            _locale: Option<&str>,
        ) -> Result<ReqContext> {
            Ok(ReqContext::new())
        }

        fn after_read_one(&self, _ctx: &AfterReadCtx, doc: Document) -> Document {
            doc
        }

        fn check_access(&self, _input: &AccessCheckInput<'_>) -> Result<AccessResult> {
            Ok(AccessResult::Allowed)
        }
    }

    fn drafts_collection_with_rows() -> (Connection, CollectionDefinition) {
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
            INSERT INTO posts (id, title, _status) VALUES ('pub1', 'Published', 'published');
            INSERT INTO posts (id, title, _status) VALUES ('draft1', 'Secret Draft', 'draft');",
        )
        .unwrap();

        let mut def = CollectionDefinition::new("posts");
        def.timestamps = true;
        def.fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];
        def.versions = Some(VersionsConfig::new(true, 0));

        (conn, def)
    }

    /// Regression: a non-draft read (`use_draft = false`) of a drafts-enabled
    /// collection must NOT return a never-published draft whose content lives
    /// in the main row with `_status = 'draft'`. Without the injected
    /// `_status = 'published'` filter this leaked unpublished content through
    /// the public `GET /{collection}/{id}` surface — a sibling to the populate
    /// draft leak.
    #[test]
    fn find_by_id_hides_unpublished_draft_on_non_draft_read() {
        let (conn, def) = drafts_collection_with_rows();
        let rh = NoopReadHooks;
        let ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .read_hooks(&rh)
            .build();

        // Published doc is visible.
        let published = find_document_by_id(&ctx, &FindByIdInput::builder("pub1").build()).unwrap();
        assert!(published.is_some(), "published doc must be readable");

        // Never-published draft must be hidden on a default (non-draft) read.
        let leaked = find_document_by_id(&ctx, &FindByIdInput::builder("draft1").build()).unwrap();
        assert!(
            leaked.is_none(),
            "unpublished draft must not leak through find_by_id"
        );

        // The draft is still reachable when the caller explicitly opts in.
        let as_draft = find_document_by_id(
            &ctx,
            &FindByIdInput::builder("draft1").use_draft(true).build(),
        )
        .unwrap();
        assert!(
            as_draft.is_some(),
            "draft opt-in (use_draft) must still surface the draft"
        );
    }

    /// A draft read is gated at edit level (`access.draft ?? access.update`),
    /// not by `access.read`. A reader who passes `access.read` but not the
    /// edit-level gate, opting into drafts, downgrades to the published view:
    /// the never-published draft is hidden (returns `None`), never leaked. The
    /// published view stays available — reads downgrade, they don't error.
    #[test]
    fn draft_read_without_draft_access_downgrades_to_published() {
        let (conn, mut def) = drafts_collection_with_rows();
        def.access.read = Some(HookRef::new("read_fn"));
        def.access.update = Some(HookRef::new("update_fn"));

        let rh = OnlyReadFnAllowed;
        let ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .read_hooks(&rh)
            .build();

        // A normal (published) read uses `access.read` → allowed.
        let published = find_document_by_id(&ctx, &FindByIdInput::builder("pub1").build()).unwrap();
        assert!(published.is_some(), "published read uses access.read");

        // Opting into drafts without the edit-level gate downgrades: the draft
        // view is dropped, so the never-published draft is hidden — not errored.
        let downgraded = find_document_by_id(
            &ctx,
            &FindByIdInput::builder("draft1").use_draft(true).build(),
        )
        .unwrap();
        assert!(
            downgraded.is_none(),
            "draft read without draft access must downgrade (draft hidden), not leak"
        );

        // The published view is still reachable for the same reader.
        let still_published = find_document_by_id(
            &ctx,
            &FindByIdInput::builder("pub1").use_draft(true).build(),
        )
        .unwrap();
        assert!(
            still_published.is_some(),
            "published content stays visible under draft downgrade"
        );
    }

    /// read → Allowed; draft (`draft_fn`) → Constrained to `author = "me"`
    /// ("preview your own drafts").
    struct OwnDraftsConstrained;

    impl ReadHooks for OwnDraftsConstrained {
        fn before_read(&self, _: &Hooks, _: &str, _: &str, _: Option<&str>) -> Result<ReqContext> {
            Ok(ReqContext::new())
        }

        fn after_read_one(&self, _: &AfterReadCtx, doc: Document) -> Document {
            doc
        }

        fn check_access(&self, input: &AccessCheckInput<'_>) -> Result<AccessResult> {
            match input.access.map(HookRef::reference) {
                Some("draft_fn") => Ok(AccessResult::Constrained(vec![FilterClause::Single(
                    Filter {
                        field: "author".to_string(),
                        op: FilterOp::Equals("me".to_string()),
                    },
                )])),
                _ => Ok(AccessResult::Allowed),
            }
        }
    }

    /// THE constrained-draft leak (regression): when `access.draft` returns a row
    /// FILTER (e.g. "preview your OWN drafts"), the draft snapshot returned by
    /// `find_by_id(use_draft=true)` must still be bounded by that filter. The
    /// snapshot early-return previously skipped the access constraints, so any
    /// caller with a constrained draft rule could fetch ANY draft by id —
    /// cross-owner unpublished disclosure on the public surface.
    #[test]
    fn constrained_draft_access_does_not_leak_other_owners_draft_snapshot() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY, title TEXT, author TEXT,
                _status TEXT DEFAULT 'published', created_at TEXT, updated_at TEXT
            );
            CREATE TABLE _versions_posts (
                id TEXT PRIMARY KEY, _parent TEXT, _version INTEGER,
                _status TEXT, _latest INTEGER DEFAULT 0, snapshot TEXT
            );
            -- d1: another owner's never-published draft (content in the snapshot).
            INSERT INTO posts (id, title, author, _status) VALUES ('d1', 'x', 'other', 'draft');
            INSERT INTO _versions_posts (id, _parent, _version, _status, _latest, snapshot)
                VALUES ('v1', 'd1', 1, 'draft', 1, '{\"title\":\"Others Secret\",\"author\":\"other\"}');
            -- d2: the viewer's OWN draft.
            INSERT INTO posts (id, title, author, _status) VALUES ('d2', 'x', 'me', 'draft');
            INSERT INTO _versions_posts (id, _parent, _version, _status, _latest, snapshot)
                VALUES ('v2', 'd2', 1, 'draft', 1, '{\"title\":\"My Secret\",\"author\":\"me\"}');",
        )
        .unwrap();

        let mut def = CollectionDefinition::new("posts");
        def.timestamps = true;
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("author", FieldType::Text).build(),
        ];
        def.versions = Some(VersionsConfig::new(true, 0));
        def.access.read = Some(HookRef::new("read_fn"));
        def.access.draft = Some(HookRef::new("draft_fn"));

        let rh = OwnDraftsConstrained;
        let ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .read_hooks(&rh)
            .build();

        // Another owner's draft must NOT leak, even with use_draft=true.
        let leaked =
            find_document_by_id(&ctx, &FindByIdInput::builder("d1").use_draft(true).build())
                .unwrap();
        assert!(
            leaked.is_none(),
            "constrained draft access must not surface another owner's draft snapshot"
        );

        // The viewer's OWN draft is still reachable — the constraint matches.
        let own = find_document_by_id(&ctx, &FindByIdInput::builder("d2").use_draft(true).build())
            .unwrap()
            .expect("own draft must remain visible under a constrained draft rule");
        assert_eq!(
            own.fields.get("title").and_then(|v| v.as_str()),
            Some("My Secret")
        );
    }

    fn soft_delete_collection_with_rows() -> (Connection, CollectionDefinition) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                _status TEXT DEFAULT 'published',
                _deleted_at TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO posts (id, title, _deleted_at) VALUES ('live1', 'Live', NULL);
            INSERT INTO posts (id, title, _deleted_at)
                VALUES ('gone1', 'Trashed', '2026-01-01T00:00:00Z');",
        )
        .unwrap();

        let mut def = CollectionDefinition::new("posts");
        def.timestamps = true;
        def.soft_delete = true;
        def.fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];

        (conn, def)
    }

    /// Regression: a trash-mode by-id lookup (`include_deleted = true`) must
    /// surface ONLY soft-deleted rows. The by-id SQL path runs with the default
    /// `_deleted_at IS NULL` filter disabled, so without an explicit
    /// `_deleted_at IS NOT NULL` guard a LIVE row leaks through the trash gate
    /// (`access.trash ?? update`) — bypassing the published-`read` policy and
    /// violating the lifecycle boundary (trash must show trashed rows only). The
    /// list path (`find.rs`) already guards this; the by-id sibling did not.
    #[test]
    fn trash_by_id_does_not_return_a_live_row() {
        let (conn, def) = soft_delete_collection_with_rows();
        let rh = NoopReadHooks;
        let ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .read_hooks(&rh)
            .build();

        // A live (non-deleted) row must NOT be returned through the trash view,
        // even though trash access (here unconstrained) authorizes the gate.
        let live = find_document_by_id(
            &ctx,
            &FindByIdInput::builder("live1")
                .include_deleted(true)
                .build(),
        )
        .unwrap();
        assert!(
            live.is_none(),
            "trash-mode by-id must not return a live (non-deleted) row"
        );

        // The actually-trashed row is reachable through the trash view.
        let trashed = find_document_by_id(
            &ctx,
            &FindByIdInput::builder("gone1")
                .include_deleted(true)
                .build(),
        )
        .unwrap();
        assert!(
            trashed.is_some(),
            "trash-mode by-id must surface a soft-deleted row"
        );
    }

    /// Regression: a collection with BOTH `soft_delete` and `versions.drafts` must
    /// not let a trash read overlay a *live* document's draft snapshot. The draft
    /// snapshot bypasses the SQL `WHERE` path (so it never sees the trash view's
    /// `_deleted_at IS NOT NULL` guard); honoring `use_draft` in trash mode let
    /// `find_by_id_full`'s snapshot early-return return a LIVE doc's draft content
    /// through the trash gate (`access.trash ?? update`) — bypassing the lifecycle
    /// boundary AND the separate draft gate (`access.draft ?? update`). Trash now
    /// pins `use_draft_overlay = false`; the draft remains reachable only via the
    /// live draft view.
    #[test]
    fn trash_with_drafts_does_not_overlay_a_live_drafts_snapshot() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY, title TEXT,
                _status TEXT DEFAULT 'published', _deleted_at TEXT,
                created_at TEXT, updated_at TEXT
            );
            CREATE TABLE _versions_posts (
                id TEXT PRIMARY KEY, _parent TEXT, _version INTEGER,
                _status TEXT, _latest INTEGER DEFAULT 0, snapshot TEXT
            );
            -- A LIVE (non-deleted) document carrying a pending draft snapshot.
            INSERT INTO posts (id, title, _deleted_at) VALUES ('live1', 'Live', NULL);
            INSERT INTO _versions_posts (id, _parent, _version, _status, _latest, snapshot)
                VALUES ('v1', 'live1', 1, 'draft', 1, '{\"title\":\"Pending Draft Edit\"}');",
        )
        .unwrap();

        let mut def = CollectionDefinition::new("posts");
        def.timestamps = true;
        def.soft_delete = true;
        def.fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];
        def.versions = Some(VersionsConfig::new(true, 0));

        let rh = NoopReadHooks;
        let ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .read_hooks(&rh)
            .build();

        // Trash + draft on a LIVE doc must NOT surface the draft snapshot through
        // the trash gate — the doc is not soft-deleted, so trash sees nothing.
        let via_trash = find_document_by_id(
            &ctx,
            &FindByIdInput::builder("live1")
                .include_deleted(true)
                .use_draft(true)
                .build(),
        )
        .unwrap();
        assert!(
            via_trash.is_none(),
            "trash read must not overlay a live document's draft snapshot"
        );

        // The draft content is still reachable through the live draft view.
        let via_draft = find_document_by_id(
            &ctx,
            &FindByIdInput::builder("live1").use_draft(true).build(),
        )
        .unwrap()
        .expect("live draft view must still surface the pending draft");
        assert_eq!(
            via_draft.fields.get("title").and_then(|v| v.as_str()),
            Some("Pending Draft Edit")
        );
    }
}
