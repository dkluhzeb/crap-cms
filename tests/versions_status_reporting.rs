//! The draft/publish status a write reports.
//!
//! The document a create/update/publish hands its `after_change` hooks, its
//! caller and its live event must carry the `_status` the row ends with — the
//! row is read back before the status moves, so these pin the stamp. Also
//! covers the status lifecycle on a collection without timestamps, what an
//! unpublished global serves to non-draft readers, and a global's first
//! partial draft save.

#![allow(clippy::missing_panics_doc, clippy::too_many_lines)]

use std::{collections::HashMap, fs, path::PathBuf, sync::Arc, time::Duration};

use tokio_stream::StreamExt;
use tonic::Request;

use crap_cms::{
    api::{
        content,
        content::content_api_server::ContentApi,
        handlers::{ContentService, ContentServiceDeps},
    },
    config::CrapConfig,
    core::{
        Registry,
        auth::{Argon2PasswordProvider, JwtTokenProvider},
        cache::NoneCache,
        collection::{CollectionDefinition, GlobalDefinition, VersionsConfig},
        email::EmailRenderer,
        event::{InProcessEventBus, SharedEventTransport},
        field::{FieldDefinition, FieldType},
        rate_limit::LoginRateLimiter,
        upload::create_storage,
    },
    db::{DbPool, migrate, pool, query},
    hooks::{self, lifecycle::HookRunner},
    service::{RunnerReadHooks, ServiceContext, collection_stats},
};

// ── Lua harness ───────────────────────────────────────────────────────────

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/status_tests")
}

/// A synced database for the `status_tests` fixture and a runner over it.
fn setup_lua() -> (tempfile::TempDir, DbPool, Arc<Registry>, HookRunner) {
    let config_dir = fixture_dir();
    let config = CrapConfig::test_default();
    let registry = hooks::init_lua(&config_dir, &config).expect("init_lua");

    let tmp = tempfile::tempdir().expect("tempdir");
    let mut db_config = CrapConfig::test_default();
    db_config.database.path = "test.db".to_string();
    let db_pool = pool::create_pool(tmp.path(), &db_config).expect("pool");
    migrate::sync_all(&db_pool, &registry, &config.locale).expect("sync");

    let runner = HookRunner::builder()
        .config_dir(&config_dir)
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .expect("runner");

    (tmp, db_pool, registry, runner)
}

fn eval(runner: &HookRunner, db_pool: &DbPool, code: &str) -> String {
    let conn = db_pool.get().expect("conn");

    runner
        .eval_lua_with_conn(code, &conn, None)
        .expect("eval failed")
}

// ── Reported status: Lua surface + after_change ───────────────────────────

/// A draft create is read back while the row still carries the column
/// default; publishing a draft row is read back before the status flips.
/// Both used to report — and hand `after_change` — the wrong status.
#[test]
fn create_and_publish_report_the_status_the_row_ends_with() {
    let (_tmp, db_pool, _registry, runner) = setup_lua();

    let result = eval(
        &runner,
        &db_pool,
        r#"
        _G._after_change_status = nil
        local d = crap.collections.create("journal", { title = "T" }, { draft = true })
        local created = tostring(d._status) .. "/" .. tostring(_G._after_change_status)

        local p = crap.collections.update("journal", d.id, { title = "T" })
        local published = tostring(p._status) .. "/" .. tostring(_G._after_change_status)

        crap.collections.journal.unpublish(d.id)
        local r = crap.collections.update("journal", d.id, { title = "T2" })
        local republished = tostring(r._status) .. "/" .. tostring(_G._after_change_status)

        return created .. "," .. published .. "," .. republished
        "#,
    );

    assert_eq!(
        result, "draft/draft,published/published,published/published",
        "returned status / after_change status for create, publish, re-publish"
    );
}

/// The bulk publish path stamps the published status through the same
/// snapshot step as the single one. (A bulk update targets published rows
/// only, so the row it reads back already says `published`; this pins that the
/// shared stamp keeps it so.)
#[test]
fn bulk_publish_hands_after_change_the_published_status() {
    let (_tmp, db_pool, _registry, runner) = setup_lua();

    let result = eval(
        &runner,
        &db_pool,
        r#"
        local d = crap.collections.create("journal", { title = "Bulk" })
        crap.collections.update("journal", d.id, { title = "Bulk" }, { draft = true })

        _G._after_change_status = nil
        local r = crap.collections.update_many("journal",
            { where = { title = "Bulk" } },
            { title = "Bulk" }
        )
        return tostring(r.modified) .. "/" .. tostring(_G._after_change_status)
        "#,
    );

    assert_eq!(result, "1/published");
}

/// Publishing an unpublished global reads the row back while it still says
/// `draft`; the write must report — and hand `after_change` — `published`.
#[test]
fn global_publish_after_unpublish_reports_published() {
    let (_tmp, db_pool, _registry, runner) = setup_lua();

    let result = eval(
        &runner,
        &db_pool,
        r#"
        crap.globals.update("notice", { headline = "Live" })
        crap.globals.unpublish("notice")

        _G._after_change_status = nil
        local g = crap.globals.update("notice", { headline = "Again" })
        return tostring(g._status) .. "/" .. tostring(_G._after_change_status)
        "#,
    );

    assert_eq!(result, "published/published");
}

// ── Unpublish needs drafts ────────────────────────────────────────────────

/// Unpublish moves `_status`, which a collection versioned without drafts does
/// not have. It used to succeed as a no-op that reported `draft` and recorded
/// a spurious draft version while the document stayed public; it is refused.
#[test]
fn unpublish_is_refused_without_drafts() {
    let (_tmp, db_pool, _registry, runner) = setup_lua();

    let result = eval(
        &runner,
        &db_pool,
        r#"
        local d = crap.collections.create("audit_log", { title = "entry" })
        local ok, err = pcall(crap.collections.audit_log.unpublish, d.id)
        local versions = crap.collections.list_versions("audit_log", d.id).documents
        return tostring(ok) .. "/" .. tostring(#versions) .. "/" .. tostring(err)
        "#,
    );

    assert!(
        result.starts_with("false/1/") && result.contains("drafts"),
        "unpublish must be refused and record nothing, got: {result}"
    );
}

// ── Unpublished global: empty to non-draft readers ────────────────────────

/// Unpublishing a global hides its content from non-draft readers until it
/// is published again; the draft view keeps it. Every published write records
/// exactly the row it wrote, so serving the last published snapshot instead
/// made unpublishing a no-op for public readers.
#[test]
fn an_unpublished_global_reads_empty_until_published_again() {
    let (_tmp, db_pool, _registry, runner) = setup_lua();

    let result = eval(
        &runner,
        &db_pool,
        r#"
        crap.globals.update("notice", { headline = "Live" })
        crap.globals.unpublish("notice")

        local public = crap.globals.get("notice")
        local draft = crap.globals.get("notice", { draft = true })
        local hidden = tostring(public.headline) .. "/" .. tostring(public._status)
        local drafted = tostring(draft.headline)

        crap.globals.update("notice", { headline = "Back" })
        local back = tostring(crap.globals.get("notice").headline)

        return hidden .. "," .. drafted .. "," .. back
        "#,
    );

    assert_eq!(result, "nil/draft,Live,Back");
}

// ── A global's first partial draft save ───────────────────────────────────

/// The first draft of a global starts from the stored global, which reads
/// with its groups nested. A partial group edit used to rebuild the group
/// from the edited sub-field alone, dropping its siblings from the draft.
#[test]
fn a_globals_first_partial_group_draft_keeps_sibling_sub_fields() {
    let (_tmp, db_pool, _registry, runner) = setup_lua();

    let result = eval(
        &runner,
        &db_pool,
        r#"
        crap.globals.update("notice", { seo = { title = "Old", desc = "Kept" } })
        crap.globals.update("notice", { seo = { title = "New" } }, { draft = true })

        local g = crap.globals.get("notice", { draft = true })
        return tostring(g.seo.title) .. "/" .. tostring(g.seo.desc)
        "#,
    );

    assert_eq!(result, "New/Kept");
}

// ── Drafts on a collection without timestamps ─────────────────────────────

/// The status write bumped `updated_at` unconditionally, so on a collection
/// with `timestamps = false` every drafts write failed with a missing-column
/// error: create, publish, unpublish and restore.
#[test]
fn a_collection_without_timestamps_runs_the_whole_status_lifecycle() {
    let (_tmp, db_pool, _registry, runner) = setup_lua();

    let result = eval(
        &runner,
        &db_pool,
        r#"
        local d = crap.collections.create("logbook", { title = "one" }, { draft = true })
        local p = crap.collections.update("logbook", d.id, { title = "two" })
        local u = crap.collections.logbook.unpublish(d.id)

        local versions = crap.collections.list_versions("logbook", d.id).documents
        local oldest = versions[#versions]
        local r = crap.collections.restore_version("logbook", d.id, oldest.id)

        return table.concat({
            tostring(d._status), tostring(p._status), tostring(u._status),
            tostring(r._status), tostring(r.title),
        }, ",")
        "#,
    );

    assert_eq!(result, "draft,published,draft,draft,one");
}

/// The dashboard card's "last updated" read selected `updated_at` from a
/// table without one and failed.
#[test]
fn stats_of_a_collection_without_timestamps_have_no_last_updated() {
    let (_tmp, db_pool, registry, runner) = setup_lua();

    eval(
        &runner,
        &db_pool,
        r#"
        crap.collections.create("logbook", { title = "one" })
        return "ok"
        "#,
    );

    let def = registry.get_collection("logbook").expect("logbook").clone();
    let conn = db_pool.get().expect("conn");
    let read_hooks = RunnerReadHooks::new(&runner, &conn, None, None);
    let ctx = ServiceContext::collection("logbook", &def)
        .conn(&conn)
        .read_hooks(&read_hooks)
        .build();

    let stats = collection_stats(&ctx, true).expect("stats");

    assert_eq!(stats.count, 1);
    assert_eq!(stats.last_updated, None);
}

// ── Live events: routed by the status the row ends with ───────────────────

struct GrpcSetup {
    _tmp: tempfile::TempDir,
    service: ContentService,
    db_pool: DbPool,
}

fn make_struct(pairs: &[(&str, &str)]) -> content::DataMap {
    let fields = pairs
        .iter()
        .map(|(k, v)| {
            (
                (*k).to_string(),
                content::FieldValue {
                    kind: Some(content::field_value::Kind::StringValue((*v).to_string())),
                },
            )
        })
        .collect::<HashMap<_, _>>();

    content::DataMap { fields }
}

/// Drafts-enabled definitions whose draft view nobody may see: an anonymous
/// subscriber can only ever be sent published content.
fn drafts_hidden_defs() -> (CollectionDefinition, GlobalDefinition) {
    let title = || FieldDefinition::builder("title", FieldType::Text).build();

    let mut posts = CollectionDefinition::new("posts");
    posts.fields = vec![title()];
    posts.versions = Some(VersionsConfig::new(true, 0));
    posts.access.draft = Some("hooks.access.deny_all".into());

    let mut notice = GlobalDefinition::new("notice");
    notice.fields = vec![title()];
    notice.versions = Some(VersionsConfig::new(true, 0));
    notice.access.draft = Some("hooks.access.deny_all".into());

    (posts, notice)
}

fn setup_grpc() -> GrpcSetup {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();

    let hooks_dir = tmp.path().join("hooks");
    fs::create_dir_all(&hooks_dir).unwrap();
    fs::write(
        hooks_dir.join("access.lua"),
        "local M = {}\nfunction M.deny_all(ctx)\n    return false\nend\nreturn M\n",
    )
    .unwrap();

    let db_pool = pool::create_pool(tmp.path(), &config).expect("pool");

    let (posts, notice) = drafts_hidden_defs();
    let shared = Registry::shared();
    {
        let mut reg = shared.write().unwrap();
        reg.register_collection(posts);
        reg.register_global(notice);
    }
    let registry = Registry::snapshot(&shared);
    migrate::sync_all(&db_pool, &registry, &config.locale).expect("sync");

    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .expect("runner");

    let transport: SharedEventTransport = Arc::new(InProcessEventBus::new(64));

    let deps = ContentServiceDeps::builder()
        .pool(db_pool.clone())
        .registry(registry)
        .hook_runner(runner)
        .config(config.clone())
        .config_dir(tmp.path().to_path_buf())
        .storage(create_storage(tmp.path(), &config.upload).unwrap())
        .email_renderer(Arc::new(EmailRenderer::new(tmp.path()).expect("email")))
        .login_limiter(Arc::new(LoginRateLimiter::new(5, 300)))
        .ip_login_limiter(Arc::new(LoginRateLimiter::new(20, 300)))
        .forgot_password_limiter(Arc::new(LoginRateLimiter::new(3, 900)))
        .ip_forgot_password_limiter(Arc::new(LoginRateLimiter::new(20, 900)))
        .cache(Arc::new(NoneCache))
        .token_provider(Arc::new(JwtTokenProvider::new("test-jwt-secret")))
        .password_provider(Arc::new(Argon2PasswordProvider))
        .event_transport(Some(transport))
        .build();

    GrpcSetup {
        _tmp: tmp,
        service: ContentService::new(deps),
        db_pool,
    }
}

type EventStream = <ContentService as ContentApi>::SubscribeStream;

async fn subscribe(gs: &GrpcSetup, request: content::SubscribeRequest) -> EventStream {
    gs.service
        .subscribe(Request::new(request))
        .await
        .expect("subscribe")
        .into_inner()
}

/// The next event within `wait`, or `None` when none arrives.
async fn next_event(stream: &mut EventStream, wait: Duration) -> Option<content::MutationEvent> {
    tokio::time::timeout(wait, stream.next())
        .await
        .ok()
        .flatten()
        .map(|event| event.expect("event"))
}

/// A draft create's event carries draft content: it must not reach a
/// subscriber without draft access. The publish that makes the document
/// visible must.
#[tokio::test]
async fn a_draft_create_is_withheld_from_published_only_subscribers() {
    let gs = setup_grpc();
    let mut stream = subscribe(
        &gs,
        content::SubscribeRequest {
            collections: vec!["posts".to_string()],
            ..Default::default()
        },
    )
    .await;

    let created = gs
        .service
        .create(Request::new(content::CreateRequest {
            events: None,
            collection: "posts".to_string(),
            data: Some(make_struct(&[("title", "Secret draft")])),
            locale: None,
            draft: Some(true),
        }))
        .await
        .expect("create")
        .into_inner()
        .document
        .expect("document");

    assert!(
        next_event(&mut stream, Duration::from_millis(500))
            .await
            .is_none(),
        "the draft create must not be delivered to a published-only subscriber"
    );

    gs.service
        .update(Request::new(content::UpdateRequest {
            collection: "posts".to_string(),
            id: created.id.clone(),
            data: Some(make_struct(&[("title", "Now public")])),
            ..Default::default()
        }))
        .await
        .expect("publish");

    let event = next_event(&mut stream, Duration::from_secs(2))
        .await
        .expect("the publish must reach a published-only subscriber");

    assert_eq!(event.operation(), content::MutationOperation::Update);
    assert_eq!(event.document_id, created.id);
}

/// Publishing an unpublished global makes it visible again: the event must
/// reach a subscriber without draft access.
#[tokio::test]
async fn publishing_an_unpublished_global_reaches_published_only_subscribers() {
    let gs = setup_grpc();

    {
        let conn = gs.db_pool.get().expect("conn");
        query::set_document_status(
            &conn,
            query::StatusTable::new("_global_notice", true),
            "default",
            "draft",
        )
        .unwrap();
    }

    let mut stream = subscribe(
        &gs,
        content::SubscribeRequest {
            globals: vec!["notice".to_string()],
            ..Default::default()
        },
    )
    .await;

    gs.service
        .update_global(Request::new(content::UpdateGlobalRequest {
            events: None,
            slug: "notice".to_string(),
            data: Some(make_struct(&[("title", "Back online")])),
            locale: None,
            draft: None,
            expected_revision: None,
        }))
        .await
        .expect("publish");

    let event = next_event(&mut stream, Duration::from_secs(2))
        .await
        .expect("the publish must reach a published-only subscriber");

    assert_eq!(event.target(), content::MutationTarget::Global);
    assert_eq!(event.collection, "notice");
}

/// Unpublishing moves a document out of the published view. A subscriber
/// that can see only that view must be told it is gone — as a `delete` with
/// no data — not left showing a document its own reads now hide.
#[tokio::test]
async fn unpublishing_is_a_delete_for_published_only_subscribers() {
    let gs = setup_grpc();

    let created = gs
        .service
        .create(Request::new(content::CreateRequest {
            events: None,
            collection: "posts".to_string(),
            data: Some(make_struct(&[("title", "Public")])),
            locale: None,
            draft: None,
        }))
        .await
        .expect("create")
        .into_inner()
        .document
        .expect("document");

    let mut stream = subscribe(
        &gs,
        content::SubscribeRequest {
            collections: vec!["posts".to_string()],
            ..Default::default()
        },
    )
    .await;

    gs.service
        .update(Request::new(content::UpdateRequest {
            collection: "posts".to_string(),
            id: created.id.clone(),
            unpublish: Some(true),
            ..Default::default()
        }))
        .await
        .expect("unpublish");

    let event = next_event(&mut stream, Duration::from_secs(2))
        .await
        .expect("the unpublish must reach a published-only subscriber as a removal");

    assert_eq!(event.operation(), content::MutationOperation::Delete);
    assert_eq!(event.document_id, created.id);
    assert!(
        event.data.is_none_or(|data| data.fields.is_empty()),
        "a removal carries no document"
    );
}

/// A subscriber scoped to `unpublish` only is not sent the removal a
/// published-only subscriber receives instead of the unpublish.
#[tokio::test]
async fn an_unpublish_scoped_subscriber_without_draft_access_gets_nothing() {
    let gs = setup_grpc();

    let created = gs
        .service
        .create(Request::new(content::CreateRequest {
            events: None,
            collection: "posts".to_string(),
            data: Some(make_struct(&[("title", "Public")])),
            locale: None,
            draft: None,
        }))
        .await
        .expect("create")
        .into_inner()
        .document
        .expect("document");

    let mut stream = subscribe(
        &gs,
        content::SubscribeRequest {
            collections: vec!["posts".to_string()],
            operations: vec!["unpublish".to_string()],
            ..Default::default()
        },
    )
    .await;

    gs.service
        .update(Request::new(content::UpdateRequest {
            collection: "posts".to_string(),
            id: created.id.clone(),
            unpublish: Some(true),
            ..Default::default()
        }))
        .await
        .expect("unpublish");

    assert!(
        next_event(&mut stream, Duration::from_millis(500))
            .await
            .is_none(),
        "the removal is a delete, which this subscriber did not ask for"
    );
}
