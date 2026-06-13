//! Single-document lookup by ID with the full read lifecycle.

use crate::{
    core::Document,
    db::{AccessResult, LocaleContext, ops},
    hooks::AccessCheckInput,
    service::{FindByIdInput, ServiceContext, ServiceError},
};

use super::draft_visibility::draft_visibility_filter;
use super::post_process::post_process_single;
use super::validate_filters::validate_access_constraints;

type Result<T> = std::result::Result<T, ServiceError>;

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

    // A draft read (opting into unpublished content) is gated at edit level
    // (`access.draft ?? access.update`), not by `access.read` — a plain reader
    // must not be able to pull unpublished content via the `draft` opt-in.
    // Mirrors the stricter `resolve_trash` gate used for the trash view.
    let wants_draft = input.use_draft && def.has_drafts();

    let access_ref = if input.include_deleted {
        def.access.resolve_trash()
    } else if wants_draft {
        def.access.resolve_draft()
    } else {
        def.access.read.as_ref()
    };

    let access = hooks.check_access(&AccessCheckInput {
        access: access_ref,
        user: ctx.user,
        id: Some(input.id),
        data: None,
        locale: input.locale_ctx.map(LocaleContext::access_locale),
        operation: "find_by_id",
        collection: ctx.slug,
        ui_locale: None,
    })?;

    if matches!(access, AccessResult::Denied) {
        let msg = if input.include_deleted {
            "Trash access denied"
        } else if wants_draft {
            "Draft access denied"
        } else {
            "Read access denied"
        };
        return Err(ServiceError::AccessDenied(msg.into()));
    }

    // Hide never-published drafts on a non-draft read. The `find`/`search`
    // list paths share `draft_visibility_filter`; `find_by_id` must apply the
    // identical rule or a document created as a draft and never published —
    // whose content lives in the main row with `_status = 'draft'` — would leak
    // here (e.g. via the public `GET /{collection}/{id}` surface). When
    // `use_draft` is true the caller opted into drafts and `find_by_id_full`'s
    // snapshot overlay handles them, so no filter is injected. Because the
    // service now actually injects `_status = 'published'`, access-hook filters
    // that mention `_status` are legitimately allowed (`injecting_status`).
    let draft_filter = draft_visibility_filter(def, input.use_draft);
    let injecting_status = draft_filter.is_some();

    let mut constraints = input.access_constraints.clone().unwrap_or_default();
    if let Some(f) = draft_filter {
        constraints.push(f);
    }

    if let AccessResult::Constrained(extra) = access {
        validate_access_constraints(&extra, input.include_deleted, injecting_status, ctx.slug)?;
        constraints.extend(extra);
    }

    let constraints = (!constraints.is_empty()).then_some(constraints);

    hooks.before_read(
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
        use_draft: input.use_draft,
        include_deleted: input.include_deleted,
    })?
    else {
        return Ok(None);
    };

    post_process_single(ctx, conn, &mut doc, input, "find_by_id");

    Ok(Some(doc))
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use anyhow::Result;
    use rusqlite::Connection;

    use super::*;
    use crate::{
        core::{
            CollectionDefinition, Document, FieldDefinition, FieldDenial, FieldType, HookRef,
            Hooks, collection::VersionsConfig,
        },
        db::AccessResult,
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
        ) -> Result<()> {
            Ok(())
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

        fn field_read_denied(
            &self,
            _fields: &[FieldDefinition],
            _user: Option<&Document>,
            _locale: Option<&str>,
        ) -> Vec<FieldDenial> {
            Vec::new()
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
    /// edit-level gate is denied when opting into drafts — they cannot pull
    /// unpublished content via `use_draft`.
    #[test]
    fn draft_read_is_gated_by_edit_access_not_read() {
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

        // A draft read resolves to the edit-level gate (`update_fn`) → denied.
        let denied = find_document_by_id(
            &ctx,
            &FindByIdInput::builder("draft1").use_draft(true).build(),
        );
        assert!(
            matches!(denied, Err(ServiceError::AccessDenied(_))),
            "draft read must be gated by edit-level access, not read"
        );
    }
}
