//! Queued bulk operations (`queue = true`): the `_system_bulk` job payload
//! round-trips, the scheduler executes it through the same op core as a
//! synchronous call, and run visibility is restricted to the queuer.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::items_after_statements,
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::unreadable_literal
)]

use std::sync::Arc;

use serde_json::json;

use crap_cms::config::CrapConfig;
use crap_cms::core::collection::CollectionDefinition;
use crap_cms::core::field::{FieldDefinition, FieldType};
use crap_cms::core::job::{JobDefinition, JobStatus, SYSTEM_BULK_JOB};
use crap_cms::core::{Document, DocumentFields, Registry};
use crap_cms::db::query::jobs as job_query;
use crap_cms::db::{migrate, pool, query};
use crap_cms::hooks::lifecycle::HookRunner;
use crap_cms::scheduler;
use crap_cms::service::jobs::bulk_queue::{self, BulkJobData, BulkOpKind, QueuedBy};
use crap_cms::service::{AppInfra, ServiceContext, StandaloneInfra};

struct Ctx {
    tmp: tempfile::TempDir,
    infra: Arc<AppInfra>,
    registry: Arc<Registry>,
    runner: HookRunner,
}

/// Create a real user row in the `users` auth collection and return its id.
fn create_user(ctx: &Ctx) -> String {
    let def = ctx.registry.get_collection("users").unwrap().clone();
    let conn = ctx.infra.pool.get().unwrap();
    let fields: DocumentFields = [("email".to_string(), json!("queuer@test.com"))]
        .into_iter()
        .collect();

    query::create(&conn, "users", &def, &fields, None)
        .expect("create user")
        .id
        .to_string()
}

fn posts_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text).build(),
        FieldDefinition::builder("status", FieldType::Text).build(),
        FieldDefinition::builder("stamped", FieldType::Text).build(),
        FieldDefinition::builder("by", FieldType::Text).build(),
    ];
    def
}

/// `posts` with a `before_change` hook that records that it ran and whether
/// it saw the acting user.
fn hooked_posts_def() -> CollectionDefinition {
    let mut def = posts_def();
    def.hooks = crap_cms::core::collection::Hooks {
        before_change: vec![crap_cms::core::HookRef::new("hooks.bulk.stamp")],
        ..Default::default()
    };
    def
}

fn setup() -> Ctx {
    setup_with(posts_def())
}

/// A minimal auth collection so a queued run can RE-LOAD its queuing user
/// (the executor no longer trusts a stored snapshot).
fn users_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("users");
    def.fields = vec![
        FieldDefinition::builder("email", FieldType::Email)
            .required(true)
            .build(),
    ];
    def.auth = Some(crap_cms::core::collection::Auth::enabled());
    def
}

fn setup_with(def: CollectionDefinition) -> Ctx {
    let tmp = tempfile::tempdir().expect("tempdir");

    std::fs::create_dir_all(tmp.path().join("hooks")).unwrap();
    std::fs::write(
        tmp.path().join("hooks/bulk.lua"),
        "local M = {}\n\
         function M.stamp(ctx)\n\
         ctx.data.stamped = \"yes\"\n\
         if ctx.user ~= nil then ctx.data.by = ctx.user.id end\n\
         return ctx\n\
         end\n\
         return M\n",
    )
    .unwrap();
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();

    let db_pool = pool::create_pool(tmp.path(), &config).expect("pool");

    let shared = Registry::shared();
    shared.write().unwrap().register_collection(def);
    shared.write().unwrap().register_collection(users_def());
    let registry = Registry::snapshot(&shared);
    migrate::sync_all(&db_pool, &registry, &config.locale).expect("sync");

    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .expect("hook runner");
    let storage = crap_cms::core::upload::create_storage(
        tmp.path(),
        &crap_cms::config::UploadConfig::default(),
    )
    .unwrap();

    let infra = AppInfra::standalone(StandaloneInfra {
        pool: db_pool,
        registry: Arc::clone(&registry),
        hook_runner: runner.clone(),
        storage,
        token_provider: None,
        event_transport: None,
        invalidation_transport: None,
        config: &config,
        config_dir: tmp.path(),
    })
    .expect("infra");

    Ctx {
        tmp,
        infra,
        registry,
        runner,
    }
}

fn job_data(op: BulkOpKind, queued_by: QueuedBy) -> BulkJobData {
    BulkJobData {
        op,
        collection: "posts".to_string(),
        queued_by,
        locale: None,
        ui_locale: None,
        draft: false,
        hooks: true,
        events: false,
        max_documents: 0,
        documents: None,
        where_clause: None,
        data: None,
        force_hard_delete: false,
    }
}

/// Claim the pending job (the scheduler does this before executing — it
/// stamps the attempt the completion write matches) and run it.
fn execute(ctx: &Ctx, run: &crap_cms::core::job::JobRun) {
    let claimed = {
        let conn = ctx.infra.pool.get().unwrap();
        job_query::claim_pending_jobs(
            &conn,
            5,
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
            0,
        )
        .expect("claim")
    };
    let run = claimed
        .iter()
        .find(|c| c.id == run.id)
        .expect("our run must be claimable");

    execute_claimed(ctx, run);
}

/// Run one already-claimed `_system_bulk` job through the scheduler.
fn execute_claimed(ctx: &Ctx, run: &crap_cms::core::job::JobRun) {
    let job_def = JobDefinition::builder(SYSTEM_BULK_JOB, "system").build();
    let storage = crap_cms::core::upload::create_storage(
        ctx.tmp.path(),
        &crap_cms::config::UploadConfig::default(),
    )
    .unwrap();

    scheduler::execute_job(scheduler::ExecuteJobParams {
        pool: &ctx.infra.pool,
        hook_runner: &ctx.runner,
        job_def: &job_def,
        job_run: run,
        email_provider: None,
        storage: &storage,
        lua_infra: None,
        app_infra: Some(&ctx.infra),
    })
    .expect("execute_job");
}

fn find_titles(ctx: &Ctx) -> Vec<String> {
    let def = ctx.registry.get_collection("posts").unwrap().clone();
    let conn = ctx.infra.pool.get().unwrap();
    let docs = query::find(&conn, "posts", &def, &query::FindQuery::default(), None).unwrap();

    docs.iter()
        .filter_map(|d| d.fields.get("title").and_then(|v| v.as_str()))
        .map(String::from)
        .collect()
}

fn fetch_run(ctx: &Ctx, id: &str) -> crap_cms::core::job::JobRun {
    let conn = ctx.infra.pool.get().unwrap();
    job_query::get_job_run(&conn, id).unwrap().unwrap()
}

/// Queued `create_many` executes the real op and records the summary.
#[test]
fn queued_create_many_executes_and_summarizes() {
    let ctx = setup();

    let mut data = job_data(BulkOpKind::CreateMany, QueuedBy::System);
    data.documents = Some(vec![
        [("title".to_string(), json!("A"))].into_iter().collect(),
        [("title".to_string(), json!("B"))].into_iter().collect(),
    ]);

    let run = bulk_queue::queue_bulk(&ctx.infra.pool, &data).expect("queue");
    assert_eq!(run.slug, SYSTEM_BULK_JOB);
    assert!(
        find_titles(&ctx).is_empty(),
        "nothing written at queue time"
    );

    execute(&ctx, &run);

    let mut titles = find_titles(&ctx);
    titles.sort();
    assert_eq!(titles, vec!["A", "B"]);

    let finished = fetch_run(&ctx, &run.id);
    assert_eq!(finished.status, JobStatus::Completed);
    let summary: serde_json::Value =
        serde_json::from_str(finished.result.as_deref().unwrap()).unwrap();
    assert_eq!(summary["created"], json!(2));
}

/// Queued `delete_many` re-decodes its stored `where` through the shared
/// decoder and reports the same counts a synchronous call would.
#[test]
fn queued_delete_many_applies_where_clause() {
    let ctx = setup();

    let def = ctx.registry.get_collection("posts").unwrap().clone();
    {
        let conn = ctx.infra.pool.get().unwrap();
        for (title, status) in [("keep", "live"), ("drop", "stale")] {
            let fields: DocumentFields = [
                ("title".to_string(), json!(title)),
                ("status".to_string(), json!(status)),
            ]
            .into_iter()
            .collect();
            query::create(&conn, "posts", &def, &fields, None).unwrap();
        }
    }

    let mut data = job_data(BulkOpKind::DeleteMany, QueuedBy::System);
    data.where_clause = Some(json!({"status": {"equals": "stale"}}).to_string());

    let run = bulk_queue::queue_bulk(&ctx.infra.pool, &data).expect("queue");
    execute(&ctx, &run);

    assert_eq!(find_titles(&ctx), vec!["keep"]);

    let finished = fetch_run(&ctx, &run.id);
    assert_eq!(finished.status, JobStatus::Completed);
    let summary: serde_json::Value =
        serde_json::from_str(finished.result.as_deref().unwrap()).unwrap();
    assert_eq!(summary["deleted"], json!(1));
}

/// A malformed payload fails PERMANENTLY (the bulk queue has no retries —
/// re-running a partially-committed batch would duplicate it).
#[test]
fn invalid_payload_fails_permanently() {
    let ctx = setup();

    let run = {
        let conn = ctx.infra.pool.get().unwrap();
        job_query::insert_job(&conn, SYSTEM_BULK_JOB, "{not json", "api", 3, "bulk", 0).unwrap()
    };

    execute(&ctx, &run);

    let finished = fetch_run(&ctx, &run.id);
    assert_eq!(finished.status, JobStatus::Failed);
    assert!(
        finished
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("invalid bulk job data"),
        "error: {:?}",
        finished.error
    );
}

/// Lifecycle hooks run inside a QUEUED bulk op exactly as they would
/// inline, and they see the queuing user as `ctx.user` — the run executes
/// under the actor snapshotted at queue time, not anonymously.
#[test]
fn queued_run_fires_hooks_with_the_queuer_as_ctx_user() {
    let ctx = setup_with(hooked_posts_def());
    let owner_id = create_user(&ctx);

    let mut data = job_data(
        BulkOpKind::CreateMany,
        QueuedBy::User {
            id: owner_id.clone(),
            collection: "users".to_string(),
            session_version: 0,
        },
    );
    data.documents = Some(vec![
        [("title".to_string(), json!("hooked"))]
            .into_iter()
            .collect(),
    ]);

    let run = bulk_queue::queue_bulk(&ctx.infra.pool, &data).expect("queue");
    execute(&ctx, &run);

    let def = ctx.registry.get_collection("posts").unwrap().clone();
    let conn = ctx.infra.pool.get().unwrap();
    let docs = query::find(&conn, "posts", &def, &query::FindQuery::default(), None).unwrap();
    let doc = docs.first().expect("the queued create ran");

    assert_eq!(
        doc.fields.get("stamped").and_then(|v| v.as_str()),
        Some("yes"),
        "before_change must run inside a queued bulk op"
    );
    assert_eq!(
        doc.fields.get("by").and_then(|v| v.as_str()),
        Some(owner_id.as_str()),
        "the hook must see the queuing user as ctx.user"
    );
}

/// `hooks = false` is honored through the queue exactly as inline.
#[test]
fn queued_run_honors_hooks_false() {
    let ctx = setup_with(hooked_posts_def());

    let mut data = job_data(BulkOpKind::CreateMany, QueuedBy::System);
    data.hooks = false;
    data.documents = Some(vec![
        [("title".to_string(), json!("quiet"))]
            .into_iter()
            .collect(),
    ]);

    let run = bulk_queue::queue_bulk(&ctx.infra.pool, &data).expect("queue");
    execute(&ctx, &run);

    let def = ctx.registry.get_collection("posts").unwrap().clone();
    let conn = ctx.infra.pool.get().unwrap();
    let docs = query::find(&conn, "posts", &def, &query::FindQuery::default(), None).unwrap();

    assert!(
        docs[0]
            .fields
            .get("stamped")
            .and_then(|v| v.as_str())
            .is_none(),
        "hooks = false must skip the hook in a queued run too"
    );
}

/// A queued run refuses to execute if its queuing user was LOCKED after
/// queueing — the executor re-loads the user rather than trusting a
/// snapshot, so revocation is observed.
#[test]
fn locked_queuing_user_abandons_the_run() {
    let ctx = setup();
    let owner_id = create_user(&ctx);

    {
        let conn = ctx.infra.pool.get().unwrap();
        let svc = ServiceContext::slug_only("users").conn(&conn).build();
        crap_cms::service::auth::lock_user(&svc, &owner_id).expect("lock");
    }

    let mut data = job_data(
        BulkOpKind::CreateMany,
        QueuedBy::User {
            id: owner_id,
            collection: "users".to_string(),
            session_version: 0,
        },
    );
    data.documents = Some(vec![
        [("title".to_string(), json!("should not exist"))]
            .into_iter()
            .collect(),
    ]);

    let run = bulk_queue::queue_bulk(&ctx.infra.pool, &data).expect("queue");
    execute(&ctx, &run);

    let finished = fetch_run(&ctx, &run.id);
    assert_eq!(finished.status, JobStatus::Failed);
    assert!(
        finished
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("locked"),
        "error: {:?}",
        finished.error
    );
    assert!(
        find_titles(&ctx).is_empty(),
        "a revoked queuer's batch must not run"
    );
}

/// A session-version bump (force-logout / password reset / unverify —
/// none of which lock the account) also abandons a pending run.
#[test]
fn revoked_session_abandons_the_run() {
    let ctx = setup();
    let owner_id = create_user(&ctx);

    let mut data = job_data(
        BulkOpKind::CreateMany,
        QueuedBy::User {
            id: owner_id.clone(),
            collection: "users".to_string(),
            // Queued at version 0…
            session_version: 0,
        },
    );
    data.documents = Some(vec![
        [("title".to_string(), json!("revoked"))]
            .into_iter()
            .collect(),
    ]);

    let run = bulk_queue::queue_bulk(&ctx.infra.pool, &data).expect("queue");

    // …then every live session is revoked without locking the account.
    {
        let conn = ctx.infra.pool.get().unwrap();
        let svc = ServiceContext::slug_only("users").conn(&conn).build();
        crap_cms::service::auth::bump_session_version(&svc, &owner_id).expect("bump");
    }

    execute(&ctx, &run);

    let finished = fetch_run(&ctx, &run.id);
    assert_eq!(finished.status, JobStatus::Failed);
    assert!(
        finished
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("revoked"),
        "error: {:?}",
        finished.error
    );
    assert!(find_titles(&ctx).is_empty());
}

/// The single-attempt pin is enforced at the insert chokepoint, not by the
/// queue's `retries` default — a retry could re-apply a committed batch.
#[test]
fn queued_runs_are_pinned_to_one_attempt() {
    let ctx = setup();
    let data = job_data(BulkOpKind::CreateMany, QueuedBy::System);

    let run = bulk_queue::queue_bulk(&ctx.infra.pool, &data).expect("queue");

    assert_eq!(run.max_attempts, 1, "a queued bulk run must never retry");
}

/// A finished run keeps its identity (so visibility still resolves) but no
/// longer stores the caller's request body.
#[test]
fn finished_run_payload_is_stripped() {
    let ctx = setup();

    let mut data = job_data(BulkOpKind::CreateMany, QueuedBy::System);
    data.documents = Some(vec![
        [("title".to_string(), json!("secret-ish"))]
            .into_iter()
            .collect(),
    ]);

    let run = bulk_queue::queue_bulk(&ctx.infra.pool, &data).expect("queue");
    assert!(
        fetch_run(&ctx, &run.id).data.contains("secret-ish"),
        "the payload is stored while the run is pending"
    );

    execute(&ctx, &run);

    let finished = fetch_run(&ctx, &run.id);
    assert_eq!(finished.status, JobStatus::Completed);
    assert!(
        !finished.data.contains("secret-ish"),
        "a finished run must not keep the submitted payload: {}",
        finished.data
    );
    assert!(
        finished.data.contains("queued_by"),
        "the identity must survive so visibility still resolves: {}",
        finished.data
    );
}

/// A pending run is cancellable by the identity that queued it, invisible
/// (and therefore not cancellable) to anyone else.
#[test]
fn pending_run_is_cancellable_by_its_queuer_only() {
    let ctx = setup();
    let owner_id = create_user(&ctx);

    let mut data = job_data(
        BulkOpKind::CreateMany,
        QueuedBy::User {
            id: owner_id.clone(),
            collection: "users".to_string(),
            session_version: 0,
        },
    );
    data.documents = Some(vec![
        [("title".to_string(), json!("cancel me"))]
            .into_iter()
            .collect(),
    ]);

    let run = bulk_queue::queue_bulk(&ctx.infra.pool, &data).expect("queue");

    let conn = ctx.infra.pool.get().unwrap();
    let cancel_as = |user: Option<&Document>| {
        let svc = ServiceContext::slug_only("")
            .conn(&conn)
            .runner(&ctx.runner)
            .user(user)
            .build();

        crap_cms::service::jobs::cancel_job_run(&svc, ctx.registry.as_ref(), &run.id).unwrap()
    };

    let stranger = Document::new("someone-else");
    assert!(!cancel_as(Some(&stranger)), "another user cannot cancel it");
    assert!(!cancel_as(None), "anonymous cannot cancel it");

    let owner = Document::new(owner_id.as_str());
    assert!(cancel_as(Some(&owner)), "the queuer cancels their own run");
    assert!(!cancel_as(Some(&owner)), "a second cancel finds nothing");
}

/// Queued `update_many` rebuilds its patch + filter and reports the frozen
/// `{"modified":N}` summary.
#[test]
fn queued_update_many_applies_patch() {
    let ctx = setup();

    let def = ctx.registry.get_collection("posts").unwrap().clone();
    {
        let conn = ctx.infra.pool.get().unwrap();
        for title in ["a", "b"] {
            let fields: DocumentFields = [
                ("title".to_string(), json!(title)),
                ("status".to_string(), json!("draft")),
            ]
            .into_iter()
            .collect();
            query::create(&conn, "posts", &def, &fields, None).unwrap();
        }
    }

    let mut data = job_data(BulkOpKind::UpdateMany, QueuedBy::System);
    data.where_clause = Some(json!({"status": {"equals": "draft"}}).to_string());
    data.data = Some(
        [("status".to_string(), json!("published"))]
            .into_iter()
            .collect(),
    );

    let run = bulk_queue::queue_bulk(&ctx.infra.pool, &data).expect("queue");
    execute(&ctx, &run);

    let finished = fetch_run(&ctx, &run.id);
    assert_eq!(finished.status, JobStatus::Completed);
    let summary: serde_json::Value =
        serde_json::from_str(finished.result.as_deref().unwrap()).unwrap();
    assert_eq!(summary["modified"], json!(2));

    let conn = ctx.infra.pool.get().unwrap();
    let docs = query::find(&conn, "posts", &def, &query::FindQuery::default(), None).unwrap();
    assert!(
        docs.iter()
            .all(|d| d.fields.get("status").and_then(|v| v.as_str()) == Some("published")),
        "every matched document must be patched"
    );
}

/// Visibility: a queued run is readable by the user who queued it, hidden
/// from everyone else, and always readable by an override caller.
#[test]
fn queued_run_visibility_is_queuer_only() {
    let ctx = setup();

    let owner = Document::new("u1");
    let other = Document::new("u2");

    let mut data = job_data(
        BulkOpKind::CreateMany,
        QueuedBy::User {
            id: "u1".to_string(),
            collection: "users".to_string(),
            session_version: 0,
        },
    );
    data.documents = Some(vec![
        [("title".to_string(), json!("X"))].into_iter().collect(),
    ]);

    let run = bulk_queue::queue_bulk(&ctx.infra.pool, &data).expect("queue");

    let conn = ctx.infra.pool.get().unwrap();

    let visible = |user: Option<&Document>, override_access: bool| {
        let ctx_ = ServiceContext::slug_only("")
            .conn(&conn)
            .runner(&ctx.runner)
            .user(user)
            .override_access(override_access)
            .build();

        crap_cms::service::jobs::get_job_run(&ctx_, ctx.registry.as_ref(), &run.id)
            .unwrap()
            .is_some()
    };

    assert!(visible(Some(&owner), false), "the queuer sees their run");
    assert!(!visible(Some(&other), false), "another user must not");
    assert!(!visible(None, false), "anonymous must not");
    assert!(visible(None, true), "override sees every run");
}
