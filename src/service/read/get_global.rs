//! Global document read with the full read lifecycle.

use serde_json::Value;

use crate::{
    core::{Document, HookRef, collection::GlobalDefinition},
    db::{DbConnection, LocaleContext, ops, query, query::helpers::global_table},
    hooks::{AccessCheckInput, lifecycle::AfterReadCtx},
    service::{
        GetGlobalInput, ReadHooks, ReadStripArgs, ServiceContext, ServiceError,
        global_access_allowed, helpers,
    },
};

type Result<T> = std::result::Result<T, ServiceError>;

/// What a global read may see and in which locale, for [`resolve_global_doc`].
#[derive(Clone, Copy)]
struct GlobalView<'a> {
    include_drafts: bool,
    published_visible: bool,
    locale_ctx: Option<&'a LocaleContext>,
}

/// Resolve the global document to serve, applying draft visibility identically
/// to the collection `find_by_id` path so the two behave the same:
///
/// - **No drafts configured** — read the single row as-is.
/// - **Drafts, reader opted in** (`include_drafts`) — overlay the latest draft
///   version snapshot when one exists (a pending draft *edit* lives in the
///   version table while the main row stays published), mirroring
///   `find_by_id_full(use_draft = true)`. Falls back to the main row otherwise.
/// - **Drafts, reader did not opt in** — an unpublished global (main row
///   `_status = 'draft'`) reads as EMPTY until it is published again: the
///   global twin of an unpublished collection document disappearing from
///   public reads. Serving the last published snapshot instead made
///   unpublishing a no-op for public readers, since every published write
///   records exactly the row it wrote.
fn resolve_global_doc(
    conn: &dyn DbConnection,
    slug: &str,
    def: &GlobalDefinition,
    view: GlobalView<'_>,
) -> anyhow::Result<Option<Document>> {
    let GlobalView {
        include_drafts,
        published_visible,
        locale_ctx,
    } = view;

    if !def.has_drafts() {
        return query::get_global(conn, slug, def, locale_ctx).map(Some);
    }

    let gtable = global_table(slug);

    if include_drafts {
        if let Some(version) = query::find_latest_version(conn, &gtable, "default")?
            && version.status == "draft"
            // A snapshot carries every locale's value: it is read for the reading
            // locale, as the collection overlay reads it.
            && let Some(mut doc) =
                ops::snapshot_read_document("default", &version.snapshot, &def.fields, locale_ctx)?
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

            return Ok(Some(doc));
        }

        // No pending draft edit, so the main row is what remains. It is draft
        // content only while the global is unpublished; otherwise it is the
        // published content, which a viewer granted only the draft view may
        // not read.
        let main = query::get_global(conn, slug, def, locale_ctx)?;
        let main_is_draft = main.fields.get("_status").and_then(Value::as_str) == Some("draft");

        return Ok((main_is_draft || published_visible).then_some(main));
    }

    let main = query::get_global(conn, slug, def, locale_ctx)?;

    if main.fields.get("_status").and_then(Value::as_str) == Some("draft") {
        return Ok(Some(unpublished_global()));
    }

    Ok(Some(main))
}

/// What a non-draft reader gets for an unpublished global: no field content,
/// `_status = "draft"` — every field reads as null until the global is
/// published again. Also the document the live streams announce to a
/// published-only subscriber when the global is unpublished.
pub(crate) fn unpublished_global() -> Document {
    let mut doc = Document::builder("default").build();

    doc.fields
        .insert("_status".to_string(), Value::String("draft".to_string()));

    doc
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

    global_access_allowed(&access, ctx.slug)
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
    let view = GlobalView {
        include_drafts: draft_visible,
        published_visible,
        locale_ctx: input.locale_ctx,
    };
    let Some(mut doc) = resolve_global_doc(conn, ctx.slug, def, view)? else {
        return Err(ServiceError::AccessDenied("Read access denied".into()));
    };

    let access_locale = input.locale_ctx.map(LocaleContext::access_locale);

    helpers::strip_unreadable(
        hooks,
        &ReadStripArgs::builder(&def.fields, ctx.slug)
            .user(ctx.user)
            .locale(access_locale)
            .build(),
        &mut doc,
    );

    let ar_ctx = AfterReadCtx {
        hooks: &def.hooks,
        fields: &def.fields,
        collection: ctx.slug,
        operation: "get",
        // `hook_locale`, as for collections: in All-locale mode the values are
        // per-locale maps, so `ctx.locale` is None rather than a misleading
        // single locale.
        locale: input.locale_ctx.and_then(LocaleContext::hook_locale),
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
        config::LocaleConfig,
        core::{
            Document, FieldDefinition, FieldType, GlobalDefinition, HookRef, Hooks, ReqContext,
            collection::VersionsConfig,
        },
        db::{AccessResult, LocaleMode},
        hooks::lifecycle::AfterReadCtx,
        service::{FieldReadStrip, hooks::ReadHooks},
    };

    struct NoopReadHooks;

    /// Denies the published view (`access.read = "deny_read"`), allows the rest.
    struct DraftOnlyReadHooks;

    impl ReadHooks for DraftOnlyReadHooks {
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
            Ok(match input.access.map(HookRef::reference) {
                Some("deny_read") => AccessResult::Denied,
                _ => AccessResult::Allowed,
            })
        }
    }

    impl FieldReadStrip for DraftOnlyReadHooks {}

    /// A viewer granted only the draft view, reading a published global with
    /// no pending draft, must not be handed the published content.
    #[test]
    fn draft_only_viewer_gets_no_published_content_without_a_pending_draft() {
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
                snapshot TEXT,
                created_at TEXT
            );
            INSERT INTO _global_settings (id, title, _status)
                VALUES ('default', 'Published Main', 'published');",
        )
        .unwrap();

        let mut def = GlobalDefinition::new("settings");
        def.fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];
        def.versions = Some(VersionsConfig::new(true, 0));
        def.access.read = Some("deny_read".into());
        def.access.draft = Some("allow_draft".into());

        let rh = DraftOnlyReadHooks;
        let ctx = ServiceContext::global("settings", &def)
            .conn(&conn)
            .read_hooks(&rh)
            .build();

        let result =
            get_global_document(&ctx, &GetGlobalInput::new(None, None).include_drafts(true));

        assert!(matches!(result, Err(ServiceError::AccessDenied(_))));
    }

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

    impl FieldReadStrip for NoopReadHooks {}

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
                snapshot TEXT,
                created_at TEXT
            );
            INSERT INTO _global_settings (id, title, _status)
                VALUES ('default', 'UNPUBLISHED DRAFT', 'draft');",
        )
        .unwrap();

        if with_published_version {
            conn.execute_batch(
                "INSERT INTO _versions__global_settings
                    (id, _parent, _version, _status, _latest, snapshot)
                 VALUES ('v1', 'default', 1, 'published', 0, '{\"title\": \"UNPUBLISHED DRAFT\"}'),
                        ('v2', 'default', 2, 'draft', 1, '{\"title\": \"UNPUBLISHED DRAFT\"}');",
            )
            .unwrap();
        }

        let mut def = GlobalDefinition::new("settings");
        def.fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];
        def.versions = Some(VersionsConfig::new(true, 0));

        (conn, def)
    }

    /// An unpublished global reads as empty to a non-draft reader — even with
    /// a published version on record whose content equals the unpublished row,
    /// which is exactly the state unpublishing a published global leaves. The
    /// draft view is unchanged.
    #[test]
    fn unpublished_global_reads_empty_to_public_readers() {
        let (conn, def) = unpublished_global(true);
        let rh = NoopReadHooks;
        let ctx = ServiceContext::global("settings", &def)
            .conn(&conn)
            .read_hooks(&rh)
            .build();

        let public = get_global_document(&ctx, &GetGlobalInput::new(None, None)).unwrap();
        assert_eq!(
            public.fields.get("title"),
            None,
            "an unpublished global serves no content to public readers"
        );
        assert_eq!(
            public.fields.get("_status").and_then(Value::as_str),
            Some("draft")
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
                snapshot TEXT,
                created_at TEXT
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

    /// Regression: a global's draft snapshot was served without resolving the
    /// reading locale, so a reader got the last save's value next to every
    /// locale's key.
    #[test]
    fn a_draft_global_reads_in_the_reading_locale() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE _global_settings (
                id TEXT PRIMARY KEY,
                title__en TEXT,
                title__de TEXT,
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
                snapshot TEXT,
                created_at TEXT
            );
            INSERT INTO _global_settings (id, title__en, title__de)
                VALUES ('default', 'Hello', 'Hallo');
            INSERT INTO _versions__global_settings
                (id, _parent, _version, _status, _latest, snapshot)
             VALUES ('v1', 'default', 1, 'draft', 1,
                '{\"title\": \"Hallo neu\", \"title__en\": \"Hello new\", \"title__de\": \"Hallo neu\"}');",
        )
        .unwrap();

        let mut def = GlobalDefinition::new("settings");
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text)
                .localized(true)
                .build(),
        ];
        def.versions = Some(VersionsConfig::new(true, 0));

        let locale_ctx = LocaleContext {
            mode: LocaleMode::Single("en".to_string()),
            config: LocaleConfig {
                default_locale: "en".to_string(),
                locales: vec!["en".into(), "de".into()],
                fallback: false,
            },
        };
        let rh = NoopReadHooks;
        let ctx = ServiceContext::global("settings", &def)
            .conn(&conn)
            .read_hooks(&rh)
            .build();

        let input = GetGlobalInput::new(Some(&locale_ctx), None).include_drafts(true);
        let doc = get_global_document(&ctx, &input).unwrap();

        assert_eq!(
            doc.fields.get("title").and_then(Value::as_str),
            Some("Hello new")
        );
        assert!(!doc.fields.contains_key("title__de"), "{:?}", doc.fields);
    }
}
