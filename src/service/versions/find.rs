//! Find a specific version by ID.

use serde_json::{Map, Value};

use crate::{
    core::{Document, document::VersionSnapshot},
    db::{AccessResult, LocaleContext, ops, query},
    hooks::AccessCheckInput,
    service::{
        Def, ReadStripArgs, ServiceContext, ServiceError, helpers,
        hooks::ReadHooks,
        reject_global_filter,
        versions::gate::{check_versions_gate, draft_snapshots_visible},
    },
};

/// Look up a single version snapshot by its ID, returned in `locale_ctx`'s
/// locale (absent = the default locale).
///
/// Checks read access and strips read-denied fields from the snapshot.
/// Derives the version table from `ctx.slug` + `ctx.def`.
///
/// # Errors
///
/// Returns `AccessDenied` or `HookError`, or a backend error if the
/// SELECT fails.
pub fn find_version_by_id(
    ctx: &ServiceContext,
    version_id: &str,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Option<VersionSnapshot>, ServiceError> {
    let Some(mut version) = find_stored_version(ctx, version_id)? else {
        return Ok(None);
    };

    read_version_snapshot(ctx, ctx.read_hooks()?, &mut version, locale_ctx)?;

    Ok(Some(version))
}

/// The **stored** version row behind [`find_version_by_id`], with every access
/// gate that read applies already enforced — but without the read shaping
/// [`read_version_snapshot`] performs, which rewrites the snapshot into the
/// document a read returns.
///
/// For the caller that needs the stored snapshot itself (what a restore would
/// write), so it reads the row once instead of selecting it again behind the
/// gated read. Shape it with [`read_version_snapshot`] before handing it to a
/// caller as a version read.
///
/// # Errors
///
/// Returns `AccessDenied` or `HookError`, or a backend error if the SELECT
/// fails.
pub(crate) fn find_stored_version(
    ctx: &ServiceContext,
    version_id: &str,
) -> Result<Option<VersionSnapshot>, ServiceError> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let hooks = ctx.read_hooks()?;
    let table = ctx.version_table();

    // The `access.versions` toggle gates history access at all (unset → follows
    // `access.update`); the read/draft composite below then scopes *which*
    // snapshots are visible.
    check_versions_gate(ctx, hooks, None, "find_by_id")?;

    // Reading a version snapshot is gated by `access.read`; a DRAFT snapshot
    // additionally requires edit-level access (`access.draft ?? access.update`),
    // so a published-only reader can't fetch a draft snapshot by id. Mirrors the
    // version-list and document draft-visibility gates.
    let access = hooks.check_access(
        &AccessCheckInput::builder("find_by_id", ctx.slug)
            .access(ctx.read_access_ref())
            .user(ctx.user)
            .build(),
    )?;

    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Read access denied".into()));
    }

    let Some(version) = query::find_version_by_id(conn, &table, version_id)? else {
        return Ok(None);
    };

    let parent_id = version.parent.to_string();

    // Symmetry with `list_versions`: when version access falls back to a
    // `Constrained` update rule, the timeline is scoped to documents the caller
    // may edit. The pre-load gate above ran with no id (so a missing version id
    // can't be told apart from a denied one); now that the parent is known, re-run
    // the gate against it so a non-owner can't pull a single historical snapshot
    // by id that `list_versions` would withhold.
    check_versions_gate(ctx, hooks, Some(&parent_id), "find_by_id")?;

    // Hide a draft snapshot from a reader who lacks edit-level access — they may
    // see published version history, not work-in-progress. A constrained draft
    // rule is enforced against the parent document, so a non-match hides the
    // snapshot ("preview your own drafts" can't reveal another owner's).
    if version.status == "draft" {
        let draft_access = hooks.check_access(
            &AccessCheckInput::builder("find_by_id", ctx.slug)
                .access(ctx.draft_access_ref())
                .user(ctx.user)
                .build(),
        )?;

        if !draft_snapshots_visible(ctx, &draft_access, &parent_id)? {
            return Ok(None);
        }
    }

    // Constrained read: for collections enforce against the version's parent id;
    // for globals, the filter table is meaningless (single row) and is rejected.
    if matches!(access, AccessResult::Constrained(_)) {
        if let Def::Global(_) = &ctx.def {
            return Err(reject_global_filter(ctx.slug));
        }
        helpers::enforce_access_constraints(ctx, &parent_id, &access, "Read", false)?;
    }

    Ok(Some(version))
}

/// Replace a version's snapshot with the document a read returns for it:
/// shaped by [`ops::snapshot_read_document`] for `locale_ctx` — so its
/// per-locale keys never reach the caller — and stripped like any read
/// document (read-denied fields, the snapshot being its own `ctx.document`,
/// and hidden fields). Shared with `list_versions`.
///
/// The caller's locale decides which values come back: a single locale
/// resolves to that locale's values, an all-locales context to the per-locale
/// map shape. No locale reads the default locale, like an unqualified read of
/// the document.
pub(crate) fn read_version_snapshot(
    ctx: &ServiceContext,
    hooks: &dyn ReadHooks,
    version: &mut VersionSnapshot,
    locale_ctx: Option<&LocaleContext>,
) -> Result<(), ServiceError> {
    let fields = ctx.fields()?;
    let default_ctx = ctx.locale_config.and_then(LocaleContext::default_for);
    let locale_ctx = locale_ctx.or(default_ctx.as_ref());
    let parent = version.parent.to_string();

    let Some(mut doc) =
        ops::snapshot_read_document(&parent, &version.snapshot, fields, locale_ctx)?
    else {
        return Ok(());
    };

    let locale = locale_ctx.map(LocaleContext::access_locale);
    helpers::strip_unreadable(
        hooks,
        &ReadStripArgs::builder(fields, ctx.slug)
            .user(ctx.user)
            .locale(locale)
            .build(),
        &mut doc,
    );

    version.snapshot = snapshot_value(doc);

    Ok(())
}

/// A read document back in snapshot form: its fields, with the timestamps the
/// snapshot carried.
fn snapshot_value(doc: Document) -> Value {
    let mut map: Map<String, Value> = doc.fields.into_iter().collect();

    for (key, ts) in [
        ("created_at", doc.created_at),
        ("updated_at", doc.updated_at),
    ] {
        if let Some(ts) = ts {
            map.insert(key.to_string(), Value::String(ts));
        }
    }

    Value::Object(map)
}

#[cfg(test)]
mod tests {
    use anyhow::Result as AnyResult;
    use rusqlite::Connection;

    use super::*;
    use crate::{
        core::{
            CollectionDefinition, Document, FieldDefinition, FieldType, HookRef, Hooks, ReqContext,
            collection::VersionsConfig,
        },
        db::{Filter, FilterClause, FilterOp},
        hooks::lifecycle::AfterReadCtx,
        service::{FieldReadStrip, hooks::ReadHooks},
    };

    /// `read` → Allowed (public read); `update` → Constrained to `author = "me"`
    /// (edit your own). `versions` is unset, so version access falls back to
    /// `update`.
    struct OwnDocsConstrainedUpdate;

    impl ReadHooks for OwnDocsConstrainedUpdate {
        fn before_read(
            &self,
            _: &Hooks,
            _: &str,
            _: &str,
            _: Option<&str>,
        ) -> AnyResult<ReqContext> {
            Ok(ReqContext::new())
        }

        fn after_read_one(&self, _: &AfterReadCtx, doc: Document) -> Document {
            doc
        }

        fn check_access(&self, input: &AccessCheckInput<'_>) -> AnyResult<AccessResult> {
            match input.access.map(HookRef::reference) {
                Some("update_fn") => Ok(AccessResult::Constrained(vec![FilterClause::Single(
                    Filter {
                        field: "author".to_string(),
                        op: FilterOp::Equals("me".to_string()),
                    },
                )])),
                _ => Ok(AccessResult::Allowed),
            }
        }
    }

    impl FieldReadStrip for OwnDocsConstrainedUpdate {}

    fn versioned_collection() -> (Connection, CollectionDefinition) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY, title TEXT, author TEXT,
                _status TEXT DEFAULT 'published', created_at TEXT, updated_at TEXT
            );
            CREATE TABLE _versions_posts (
                id TEXT PRIMARY KEY, _parent TEXT, _version INTEGER,
                _status TEXT, _latest INTEGER DEFAULT 0, snapshot TEXT, created_at TEXT
            );
            -- Another owner's published doc + a published version snapshot.
            INSERT INTO posts (id, title, author, _status) VALUES ('d1', 'x', 'other', 'published');
            INSERT INTO _versions_posts (id, _parent, _version, _status, _latest, snapshot)
                VALUES ('v1', 'd1', 1, 'published', 1, '{\"title\":\"Old Public Title\"}');
            -- The viewer's OWN published doc + snapshot.
            INSERT INTO posts (id, title, author, _status) VALUES ('d2', 'x', 'me', 'published');
            INSERT INTO _versions_posts (id, _parent, _version, _status, _latest, snapshot)
                VALUES ('v2', 'd2', 1, 'published', 1, '{\"title\":\"My Old Title\"}');",
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
        def.access.update = Some(HookRef::new("update_fn"));

        (conn, def)
    }

    /// Regression: `find_version_by_id` must enforce the `versions ?? update`
    /// constraint against the snapshot's parent, exactly like `list_versions`.
    /// With `versions` unset and `update` constrained to your own docs, a public
    /// `read` previously let a non-owner pull another owner's *published*
    /// historical snapshot by id — content `list_versions` would withhold. The
    /// pre-load gate runs with no id, so the scope is enforced after the parent
    /// is known.
    #[test]
    fn find_version_by_id_enforces_constrained_update_scope() {
        let (conn, def) = versioned_collection();
        let rh = OwnDocsConstrainedUpdate;
        let ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .read_hooks(&rh)
            .build();

        // Another owner's snapshot: denied (parent not editable by the caller).
        let other = find_version_by_id(&ctx, "v1", None);
        assert!(
            matches!(other, Err(ServiceError::AccessDenied(_))),
            "fetching another owner's version by id must be denied, got {other:?}"
        );

        // The caller's own snapshot: visible.
        let own = find_version_by_id(&ctx, "v2", None)
            .unwrap()
            .expect("own version snapshot must be reachable");
        assert_eq!(
            own.snapshot.get("title").and_then(|v| v.as_str()),
            Some("My Old Title")
        );
    }
}
