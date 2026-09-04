//! Global document read with the full read lifecycle.

use serde_json::Value;

use crate::{
    core::{Document, HookRef, collection::GlobalDefinition},
    db::{AccessResult, DbConnection, LocaleContext, ops, query, query::helpers::global_table},
    hooks::{AccessCheckInput, lifecycle::AfterReadCtx},
    service::{GetGlobalInput, ReadHooks, ServiceContext, ServiceError, helpers},
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Resolve the global document to serve, applying draft visibility identically
/// to the collection `find_by_id` path so the two behave the same:
///
/// - **No drafts configured** — read the single row as-is.
/// - **Drafts, reader opted in** (`include_drafts`) — overlay the latest draft
///   version snapshot when one exists (a pending draft *edit* lives in the
///   version table while the main row stays published), mirroring
///   `find_by_id_full(use_draft = true)`. Falls back to the main row otherwise.
/// - **Drafts, reader did not opt in** — hide an unpublished global (main row
///   `_status = 'draft'`) by serving the last published snapshot, or empty
///   content when nothing was ever published.
fn resolve_global_doc(
    conn: &dyn DbConnection,
    slug: &str,
    def: &GlobalDefinition,
    include_drafts: bool,
    locale_ctx: Option<&LocaleContext>,
) -> anyhow::Result<Document> {
    if !def.has_drafts() {
        return query::get_global(conn, slug, def, locale_ctx);
    }

    let gtable = global_table(slug);

    if include_drafts {
        if let Some(version) = query::find_latest_version(conn, &gtable, "default")?
            && version.status == "draft"
            && let Some(mut doc) = ops::document_from_snapshot("default", &version.snapshot)
        {
            // The row is the authority on `_status` — snapshots can carry a
            // stale value (see the collection overlay in `db::ops`). A
            // draft-only global must read as "draft".
            if let Some(row_status) =
                query::versions::get_document_status(conn, &gtable, "default")?
            {
                doc.fields
                    .insert("_status".to_string(), Value::String(row_status));
            }

            return Ok(doc);
        }

        return query::get_global(conn, slug, def, locale_ctx);
    }

    let main = query::get_global(conn, slug, def, locale_ctx)?;

    if main.fields.get("_status").and_then(Value::as_str) == Some("draft") {
        return published_global_or_empty(conn, &gtable);
    }

    Ok(main)
}

/// The published content to serve when an unpublished global is read without a
/// draft opt-in: the most recent `published` version snapshot, or an empty
/// global when nothing was ever published. `gtable` is the `_global_{slug}`
/// table name (the version-table base for globals).
fn published_global_or_empty(conn: &dyn DbConnection, gtable: &str) -> anyhow::Result<Document> {
    if let Some(version) = query::find_latest_published_version(conn, gtable, "default")?
        && let Some(doc) = ops::document_from_snapshot("default", &version.snapshot)
    {
        return Ok(doc);
    }

    Ok(Document::builder("default").build())
}

/// Resolve a single global view (published or draft) to a boolean visibility.
/// Globals are single-row and so do not support filter-based access: a
/// `Constrained` result is a configuration error, not a row filter.
///
/// # Errors
///
/// Returns [`ServiceError::HookError`] if the access hook returns a filter
/// table, or propagates a hook execution error.
fn global_view_visible(
    hooks: &dyn ReadHooks,
    ctx: &ServiceContext,
    access_ref: Option<&HookRef>,
    input: &GetGlobalInput,
) -> Result<bool> {
    // Match this global-read's own `before_read` / `after_read` hooks, which
    // report `"get"` — a global has no collection-style `find`.
    let access = hooks.check_access(
        &AccessCheckInput::builder("get", ctx.slug)
            .access(access_ref)
            .user(ctx.user)
            .locale(input.locale_ctx.map(LocaleContext::access_locale))
            .ui_locale(input.ui_locale)
            .build(),
    )?;

    match access {
        AccessResult::Allowed => Ok(true),
        AccessResult::Denied => Ok(false),
        AccessResult::Constrained(_) => Err(ServiceError::HookError(format!(
            "Access hook for global '{}' returned a filter table; globals don't support filter-based access — return true/false based on ctx.user fields instead.",
            ctx.slug
        ))),
    }
}

/// Read a global document with the full read lifecycle.
///
/// Steps: `before_read` -> `get_global` -> field-level read strip -> `after_read`.
///
/// # Errors
///
/// Returns service-layer errors (access denied, hook errors) or a backend
/// error if the SELECT or hydration fails.
pub fn get_global_document(ctx: &ServiceContext, input: &GetGlobalInput) -> Result<Document> {
    let resolved = ctx.resolve_conn()?;
    let conn = resolved.as_ref();
    let hooks = ctx.read_hooks()?;
    let def = ctx.global_def()?;

    // Two independent views, exactly as for collections: published content gated
    // by `access.read`, draft content gated by `access.draft ?? access.update`.
    // Reads downgrade — a reader opting into drafts without the edit-level gate
    // still sees the published global rather than an error.
    let published_visible = global_view_visible(hooks, ctx, def.access.read.as_ref(), input)?;

    let draft_visible = input.include_drafts
        && def.has_drafts()
        && global_view_visible(hooks, ctx, def.access.resolve_draft(), input)?;

    if !published_visible && !draft_visible {
        return Err(ServiceError::AccessDenied("Read access denied".into()));
    }

    let req_context = hooks.before_read(
        &def.hooks,
        ctx.slug,
        "get",
        input.locale_ctx.map(LocaleContext::access_locale),
    )?;

    // Resolve the document with draft visibility applied identically to the
    // collection `find_by_id` path (see `resolve_global_doc`). `draft_visible`
    // is the downgraded opt-in: a denied draft view falls back to published.
    let mut doc = resolve_global_doc(conn, ctx.slug, def, draft_visible, input.locale_ctx)?;

    let access_locale = input.locale_ctx.map(LocaleContext::access_locale);

    hooks.strip_read_access_doc(&def.fields, &mut doc, ctx.slug, ctx.user, access_locale);
    doc.strip_fields(&helpers::collect_api_hidden_field_names(&def.fields, ""));

    let ar_ctx = AfterReadCtx {
        hooks: &def.hooks,
        fields: &def.fields,
        collection: ctx.slug,
        operation: "get",
        locale: input.locale_ctx.map(LocaleContext::access_locale),
        user: ctx.user,
        ui_locale: input.ui_locale,
        context: req_context,
    };

    Ok(hooks.after_read_one(&ar_ctx, doc))
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use anyhow::Result;
    use rusqlite::Connection;

    use super::*;
    use crate::{
        core::{
            Document, FieldDefinition, FieldType, GlobalDefinition, Hooks, ReqContext,
            collection::VersionsConfig,
        },
        hooks::lifecycle::AfterReadCtx,
        service::hooks::ReadHooks,
    };

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

    /// Build a drafts-enabled global whose main row is unpublished
    /// (`_status = 'draft'`), optionally with a prior published version snapshot.
    fn unpublished_global(with_published_version: bool) -> (Connection, GlobalDefinition) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE _global_settings (
                id TEXT PRIMARY KEY,
                title TEXT,
                _status TEXT DEFAULT 'published',
                created_at TEXT,
                updated_at TEXT
            );
            CREATE TABLE _versions__global_settings (
                id TEXT PRIMARY KEY,
                _parent TEXT,
                _version INTEGER,
                _status TEXT,
                _latest INTEGER DEFAULT 0,
                snapshot TEXT
            );
            INSERT INTO _global_settings (id, title, _status)
                VALUES ('default', 'UNPUBLISHED DRAFT', 'draft');",
        )
        .unwrap();

        if with_published_version {
            conn.execute_batch(
                "INSERT INTO _versions__global_settings
                    (id, _parent, _version, _status, _latest, snapshot)
                 VALUES ('v1', 'default', 1, 'published', 0, '{\"title\": \"Published\"}'),
                        ('v2', 'default', 2, 'draft', 1, '{\"title\": \"UNPUBLISHED DRAFT\"}');",
            )
            .unwrap();
        }

        let mut def = GlobalDefinition::new("settings");
        def.fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];
        def.versions = Some(VersionsConfig::new(true, 0));

        (conn, def)
    }

    /// Regression: a non-draft read of an unpublished global must not leak the
    /// draft sitting in the main row. It serves the last published snapshot.
    #[test]
    fn unpublished_global_hidden_from_public_read() {
        let (conn, def) = unpublished_global(true);
        let rh = NoopReadHooks;
        let ctx = ServiceContext::global("settings", &def)
            .conn(&conn)
            .read_hooks(&rh)
            .build();

        let public = get_global_document(&ctx, &GetGlobalInput::new(None, None)).unwrap();
        assert_eq!(
            public.fields.get("title").and_then(Value::as_str),
            Some("Published"),
            "public read must serve the last published snapshot, not the draft"
        );

        // An editor opting into drafts still sees the unpublished content.
        let editor =
            get_global_document(&ctx, &GetGlobalInput::new(None, None).include_drafts(true))
                .unwrap();
        assert_eq!(
            editor.fields.get("title").and_then(Value::as_str),
            Some("UNPUBLISHED DRAFT"),
            "draft opt-in must surface the unpublished content"
        );
    }

    /// When nothing was ever published, a non-draft read yields empty content
    /// rather than leaking the draft.
    #[test]
    fn unpublished_global_with_no_published_version_reads_empty() {
        let (conn, def) = unpublished_global(false);
        let rh = NoopReadHooks;
        let ctx = ServiceContext::global("settings", &def)
            .conn(&conn)
            .read_hooks(&rh)
            .build();

        let public = get_global_document(&ctx, &GetGlobalInput::new(None, None)).unwrap();
        assert_ne!(
            public.fields.get("title").and_then(Value::as_str),
            Some("UNPUBLISHED DRAFT"),
            "draft content must not leak when no published version exists"
        );
    }

    /// Parity with collection `find_by_id`: a *published* global with a pending
    /// draft edit (saved to the version table while the main row stays
    /// published) surfaces the draft edit when drafts are opted into, and the
    /// published main row otherwise.
    #[test]
    fn published_global_with_pending_draft_edit_overlays_on_opt_in() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE _global_settings (
                id TEXT PRIMARY KEY,
                title TEXT,
                _status TEXT DEFAULT 'published',
                created_at TEXT,
                updated_at TEXT
            );
            CREATE TABLE _versions__global_settings (
                id TEXT PRIMARY KEY,
                _parent TEXT,
                _version INTEGER,
                _status TEXT,
                _latest INTEGER DEFAULT 0,
                snapshot TEXT
            );
            INSERT INTO _global_settings (id, title, _status)
                VALUES ('default', 'Published Main', 'published');
            INSERT INTO _versions__global_settings
                (id, _parent, _version, _status, _latest, snapshot)
             VALUES ('v1', 'default', 1, 'published', 0, '{\"title\": \"Published Main\"}'),
                    ('v2', 'default', 2, 'draft', 1, '{\"title\": \"Pending Draft Edit\"}');",
        )
        .unwrap();

        let mut def = GlobalDefinition::new("settings");
        def.fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];
        def.versions = Some(VersionsConfig::new(true, 0));

        let rh = NoopReadHooks;
        let ctx = ServiceContext::global("settings", &def)
            .conn(&conn)
            .read_hooks(&rh)
            .build();

        // Opt-in surfaces the pending draft edit (matches find_by_id).
        let editor =
            get_global_document(&ctx, &GetGlobalInput::new(None, None).include_drafts(true))
                .unwrap();
        assert_eq!(
            editor.fields.get("title").and_then(Value::as_str),
            Some("Pending Draft Edit"),
            "draft opt-in must overlay the pending draft version"
        );

        // A normal read still serves the published main row.
        let public = get_global_document(&ctx, &GetGlobalInput::new(None, None)).unwrap();
        assert_eq!(
            public.fields.get("title").and_then(Value::as_str),
            Some("Published Main"),
            "a published global serves its main row to non-draft readers"
        );
    }
}
