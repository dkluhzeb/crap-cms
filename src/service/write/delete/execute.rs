//! Removing the row — trash (soft delete) or purge (hard delete) — and
//! reading the delete's live event on the delete's own connection.

use crate::{
    config::LocaleConfig,
    db::{DbConnection, LocaleContext, query},
    service::{DeleteEvent, ServiceContext, ServiceError, purge_document, read_delete_event},
};

type Result<T> = std::result::Result<T, ServiceError>;

/// The delete's live event, read on the delete's own connection (inside its
/// transaction, on the row locked at the start of the delete, after the
/// before-hooks) so it is exactly the row the delete acts on. `None` when the delete
/// publishes no event — nothing is read then.
fn read_event(
    ctx: &ServiceContext,
    conn: &dyn DbConnection,
    id: &str,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Option<DeleteEvent>> {
    if !ctx.publishes_events() {
        return Ok(None);
    }

    read_delete_event(conn, ctx.collection_def()?, id, locale_ctx)
}

/// Remove the row — trash it (soft delete) or purge it (hard delete) — and
/// return the delete's live event: a hard-deleted row as read just before it
/// went, a trashed row as it now sits in the trash.
pub(super) fn execute_delete(
    ctx: &ServiceContext,
    conn: &dyn DbConnection,
    id: &str,
    locale_cfg: &LocaleConfig,
) -> Result<Option<DeleteEvent>> {
    let def = ctx.collection_def()?;
    let locale_ctx = LocaleContext::default_for(locale_cfg);

    if !def.soft_delete {
        let removed = read_event(ctx, conn, id, locale_ctx.as_ref())?;

        if !purge_document(conn, def, id, locale_cfg)? {
            return Err(ServiceError::NotFound(format!(
                "Document '{id}' not found in '{}'",
                ctx.slug
            )));
        }

        return Ok(removed);
    }

    if !query::soft_delete(conn, ctx.slug, id)? {
        return Err(ServiceError::NotFound(format!(
            "Document '{id}' not found in '{}' (or already deleted)",
            ctx.slug
        )));
    }

    // Trashing a user retires every token issued to it. The trashed row no
    // longer resolves, but a restore would otherwise bring each of those
    // tokens back to life. In the delete's transaction; the stream teardown is
    // published after commit by the delete wrappers.
    if def.is_auth_collection() {
        query::bump_session_version(conn, ctx.slug, id)?;
    }

    // A soft-deleted row keeps its FTS entry so the trash view stays
    // searchable (the normal view is filtered by `_deleted_at` before the FTS
    // membership clause), and its queued image conversions: a restore brings
    // the upload back, and nothing re-queues them. A conversion that runs
    // while the row is trashed writes its URL onto the trashed row and
    // publishes nothing (the report reads live rows only); only a hard
    // delete cancels them.
    let trashed = read_event(ctx, conn, id, locale_ctx.as_ref())?;

    Ok(trashed.map(DeleteEvent::moved_to_trash))
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use std::{cell::RefCell, rc::Rc, sync::Arc};

    use rusqlite::Connection;

    use super::*;
    use crate::{
        config::CrapConfig,
        core::{
            CollectionDefinition, EventViewPlacement, FieldDefinition, FieldType, JobStatus,
            Registry, SharedEventTransport, VersionsConfig,
            event::{EventOperation, InProcessEventBus},
            upload::{
                CollectionUpload, ImageConvertJobData, SYSTEM_IMAGE_CONVERT_JOB,
                queue_image_conversion,
            },
        },
        db::{Filter, FilterClause, FilterOp, migrate, pool},
        service::{
            DeleteManyOptions, EventQueue, delete_document, delete_document_in_conn, delete_many,
            write::delete::test_support::{AllowAllWriteHooks, setup_auth_collection},
        },
    };

    /// Regression: trashing an auth user left its session version alone. While
    /// trashed its tokens were refused (the user no longer resolves), but a
    /// restore brought every pre-trash token back to life — an operator who
    /// trashed a compromised account to kick the attacker out handed the
    /// attacker's stolen token back on restore. The trash now retires every
    /// issued token, and the restore does not bring the old version back.
    #[test]
    fn trashing_an_auth_user_retires_its_issued_tokens() {
        let (conn, mut def) = setup_auth_collection();
        conn.execute_batch("ALTER TABLE users ADD COLUMN _deleted_at TEXT;")
            .unwrap();
        def.soft_delete = true;

        let hooks = AllowAllWriteHooks;
        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .write_hooks(&hooks)
            .override_access(true)
            .build();

        let issued_under = query::get_session_version(&conn, "users", "u1").unwrap();

        delete_document_in_conn(&ctx, "u1", None).expect("trash");
        assert!(query::restore(&conn, "users", "u1").unwrap(), "restored");

        let current = query::get_session_version(&conn, "users", "u1").unwrap();
        assert_ne!(
            current, issued_under,
            "a token issued before the trash must stay stale after the restore"
        );
    }

    /// Regression: a soft delete cancelled the upload's queued image
    /// conversions and a restore never re-queued them, so an upload trashed
    /// before its conversions ran came back without its variant URLs.
    #[test]
    fn a_soft_delete_keeps_queued_image_conversions() {
        let mut media = CollectionDefinition::new("media");
        media.soft_delete = true;
        media.upload = Some(CollectionUpload {
            enabled: true,
            ..Default::default()
        });

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut config = CrapConfig::test_default();
        config.database.path = "test.db".to_string();
        let db_pool = pool::create_pool(tmp.path(), &config).expect("pool");
        let shared = Registry::shared();
        shared.write().unwrap().register_collection(media.clone());
        migrate::sync_all(&db_pool, &Registry::snapshot(&shared), &config.locale).expect("sync");

        let conn = db_pool.get().unwrap();
        conn.execute("INSERT INTO media (id) VALUES ('m1')", &[])
            .unwrap();
        let job = ImageConvertJobData {
            collection: "media".to_string(),
            document_id: "m1".to_string(),
            source_path: "a.png".to_string(),
            target_path: "a.webp".to_string(),
            format: "webp".to_string(),
            quality: 80,
            url_column: "thumbnail_webp_url".to_string(),
            url_value: "/uploads/a.webp".to_string(),
        };
        queue_image_conversion(&conn, &job, 1).unwrap();

        let hooks = AllowAllWriteHooks;
        let ctx = ServiceContext::collection("media", &media)
            .conn(&conn)
            .write_hooks(&hooks)
            .override_access(true)
            .build();

        delete_document_in_conn(&ctx, "m1", None).expect("soft delete");

        let pending = query::jobs::count_job_runs(
            &conn,
            Some(SYSTEM_IMAGE_CONVERT_JOB),
            Some(JobStatus::Pending),
        )
        .unwrap();
        assert_eq!(pending, 1, "a trashed upload keeps its queued conversion");
    }

    /// A `posts` table holding `p1` (owned by `u1`) and `p2` (owned by `u2`);
    /// soft-deleting when `soft_delete`.
    fn owned_posts(soft_delete: bool) -> (Connection, CollectionDefinition) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                owner TEXT,
                _ref_count INTEGER DEFAULT 0,
                _deleted_at TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO posts (id, owner) VALUES ('p1', 'u1'), ('p2', 'u2');",
        )
        .unwrap();

        let mut def = CollectionDefinition::new("posts");
        def.timestamps = true;
        def.soft_delete = soft_delete;
        def.fields = vec![FieldDefinition::builder("owner", FieldType::Text).build()];

        (conn, def)
    }

    fn owner_is(owner: &str) -> [FilterClause; 1] {
        [FilterClause::Single(Filter {
            field: "owner".to_string(),
            op: FilterOp::Equals(owner.to_string()),
        })]
    }

    /// A conn-mode context that publishes its events into `queue`.
    fn publishing_ctx<'a>(
        conn: &'a Connection,
        def: &'a CollectionDefinition,
        hooks: &'a AllowAllWriteHooks,
        queue: &EventQueue,
    ) -> ServiceContext<'a> {
        let transport: SharedEventTransport = Arc::new(InProcessEventBus::new(16));

        ServiceContext::collection("posts", def)
            .conn(conn)
            .write_hooks(hooks)
            .override_access(true)
            .event_transport(Some(transport))
            .event_queue(queue.clone())
            .build()
    }

    /// Regression: a delete event carried no document, so a subscriber with a
    /// row-constrained view never learned that a row it could see was deleted.
    /// A hard delete now carries the row as read just before it was removed.
    #[test]
    fn hard_delete_event_carries_the_removed_row() {
        let (conn, def) = owned_posts(false);
        let hooks = AllowAllWriteHooks;
        let queue: EventQueue = Rc::new(RefCell::new(Vec::new()));
        let ctx = publishing_ctx(&conn, &def, &hooks, &queue);

        delete_document(&ctx, "p1", None, None).expect("delete");

        let queued = queue.borrow();
        let event = queued.first().expect("delete event queued");
        let gate = event.gate.as_ref().expect("the removed row rides along");

        assert_eq!(event.operation, EventOperation::Delete);
        assert!(event.data.is_empty(), "a delete delivers no document");
        assert!(!event.view.trashed);
        assert!(gate.matches(&owner_is("u1"), &def.fields));
        assert!(!gate.matches(&owner_is("u2"), &def.fields));
    }

    /// A soft delete carries the row as it now sits in the trash — the view the
    /// event is gated by — so a trash-view constraint is judged like SQL's.
    #[test]
    fn soft_delete_event_carries_the_trashed_row() {
        let (conn, def) = owned_posts(true);
        let hooks = AllowAllWriteHooks;
        let queue: EventQueue = Rc::new(RefCell::new(Vec::new()));
        let ctx = publishing_ctx(&conn, &def, &hooks, &queue);

        delete_document(&ctx, "p1", None, None).expect("soft delete");

        let queued = queue.borrow();
        let event = queued.first().expect("delete event queued");
        let gate = event.gate.as_ref().expect("the trashed row rides along");
        let trashed = [FilterClause::Single(Filter {
            field: "_deleted_at".to_string(),
            op: FilterOp::Exists,
        })];

        assert!(event.view.trashed);
        assert!(gate.matches(&owner_is("u1"), &def.fields));
        assert!(gate.matches(&trashed, &def.fields));
    }

    /// Regression: trashing a live document was gated by the trash view
    /// alone, so a subscriber that could see it only in its status view kept
    /// showing it. The event records the status view the row left — and that
    /// it left the published view only when it was published — while a hard
    /// delete records no move: the row's own view admits it.
    #[test]
    fn trashing_a_published_row_leaves_the_published_view() {
        let (conn, mut def) = owned_posts(true);
        conn.execute_batch(
            "ALTER TABLE posts ADD COLUMN _status TEXT;
            UPDATE posts SET _status = 'published' WHERE id = 'p1';
            UPDATE posts SET _status = 'draft' WHERE id = 'p2';",
        )
        .unwrap();
        def.versions = Some(VersionsConfig::new(true, 0));

        let hooks = AllowAllWriteHooks;
        let queue: EventQueue = Rc::new(RefCell::new(Vec::new()));
        let ctx = publishing_ctx(&conn, &def, &hooks, &queue);

        delete_document(&ctx, "p1", None, None).expect("trash the published row");
        delete_document(&ctx, "p2", None, None).expect("trash the draft");

        let queued = queue.borrow();
        let published = queued.first().expect("the published row's event");
        let draft = queued.get(1).expect("the draft's event");

        let live = |status: &str| {
            Some(EventViewPlacement {
                status: Some(status.into()),
                trashed: false,
            })
        };

        assert!(published.view.trashed);
        assert_eq!(published.view.prior, live("published"));
        assert!(published.view.left_published);
        assert!(draft.view.trashed);
        assert_eq!(draft.view.prior, live("draft"), "it left the draft view");
        assert!(!draft.view.left_published, "a draft never was published");
    }

    /// A hard delete never records a removal: the row's own view admits it.
    #[test]
    fn a_hard_delete_does_not_leave_the_published_view() {
        let (conn, def) = owned_posts(false);
        let hooks = AllowAllWriteHooks;
        let queue: EventQueue = Rc::new(RefCell::new(Vec::new()));
        let ctx = publishing_ctx(&conn, &def, &hooks, &queue);

        delete_document(&ctx, "p1", None, None).expect("delete");

        let queued = queue.borrow();
        let event = queued.first().expect("delete event queued");

        assert!(event.view.prior.is_none());
        assert!(!event.view.left_published);
    }

    /// A delete that publishes no event reads nothing for it.
    #[test]
    fn delete_without_events_reads_no_event_row() {
        let (conn, def) = owned_posts(false);
        let hooks = AllowAllWriteHooks;
        let ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .write_hooks(&hooks)
            .override_access(true)
            .build();

        let result = delete_document_in_conn(&ctx, "p1", None).expect("delete");

        assert!(result.event.is_none());
    }

    /// Regression: a hard delete of a trashed row — a forced delete, or a
    /// purge of the trash — runs on the collection's hard-delete variant,
    /// which reads no trash column, so its event was gated by the row's status
    /// view: a subscriber without trash access learned of a document it could
    /// no longer see. It is gated by the trash, the view the row was last in,
    /// and carries the trash timestamp for a trash-view constraint.
    #[test]
    fn a_hard_delete_of_a_trashed_row_is_gated_by_the_trash() {
        let (conn, soft) = owned_posts(true);
        query::soft_delete(&conn, "posts", "p1").unwrap();

        let mut hard = soft.clone();
        hard.make_hard_delete();

        let hooks = AllowAllWriteHooks;
        let queue: EventQueue = Rc::new(RefCell::new(Vec::new()));
        let ctx = publishing_ctx(&conn, &hard, &hooks, &queue);

        delete_document(&ctx, "p1", None, None).expect("delete the trashed row");
        delete_document(&ctx, "p2", None, None).expect("delete the live row");

        let queued = queue.borrow();
        let trashed = [FilterClause::Single(Filter {
            field: "_deleted_at".to_string(),
            op: FilterOp::Exists,
        })];

        let purged = queued.first().expect("the trashed row's event");
        let gate = purged.gate.as_ref().expect("the removed row rides along");
        assert!(purged.view.trashed, "a trashed row is gated by the trash");
        assert!(gate.matches(&trashed, &hard.fields));
        assert!(gate.matches(&owner_is("u1"), &hard.fields));

        let live = queued.get(1).expect("the live row's event");
        assert!(!live.view.trashed, "a live row keeps its status view");
    }

    /// Every document a bulk delete removes carries its own row.
    #[test]
    fn bulk_delete_events_carry_each_removed_row() {
        let (conn, def) = owned_posts(false);
        let hooks = AllowAllWriteHooks;
        let queue: EventQueue = Rc::new(RefCell::new(Vec::new()));
        let ctx = publishing_ctx(&conn, &def, &hooks, &queue);
        let both = [FilterClause::Single(Filter {
            field: "owner".to_string(),
            op: FilterOp::In(vec!["u1".to_string(), "u2".to_string()]),
        })];

        delete_many(
            &ctx,
            &both,
            &LocaleConfig::default(),
            &DeleteManyOptions::default(),
        )
        .expect("bulk delete");

        let queued = queue.borrow();
        assert_eq!(queued.len(), 2);

        for event in queued.iter() {
            let owner = if event.document_id == "p1" {
                "u1"
            } else {
                "u2"
            };
            let gate = event.gate.as_ref().expect("each removed row rides along");

            assert!(
                gate.matches(&owner_is(owner), &def.fields),
                "{}",
                event.document_id
            );
        }
    }
}
