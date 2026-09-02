//! Bulk `update_many` / `delete_many` service behavior: the
//! `server.bulk_max_documents` cap and cross-batch atomicity (a mid-operation
//! failure rolls the whole operation back, leaving no partial state).
//!
//! Drives the pool-mode service path (the one gRPC / admin / MCP use). The cap
//! and atomicity live in the service layer, so all surfaces inherit them.

#![allow(clippy::missing_panics_doc, clippy::too_many_lines)]

use std::sync::Arc;

use crap_cms::config::{CrapConfig, LocaleConfig, PasswordPolicy};
use crap_cms::core::cache::{CacheBackend, MemoryCache, SharedCache};
use crap_cms::core::collection::{Auth, CollectionDefinition, Labels};
use crap_cms::core::field::{FieldDefinition, FieldType, LocalizedString};
use crap_cms::core::{DocumentFields, Registry};
use crap_cms::db::{DbPool, LocaleContext, LocaleMode, migrate, pool, query};
use crap_cms::hooks::{self, lifecycle::HookRunner};
use crap_cms::service::{
    CreateManyItem, CreateManyOptions, DeleteManyOptions, RunnerWriteHooks, ServiceContext,
    ServiceError, UpdateManyOptions, create_many, delete_many, update_many,
};
use serde_json::json;

struct Setup {
    _tmp: tempfile::TempDir,
    pool: DbPool,
    runner: HookRunner,
    def: std::sync::Arc<CollectionDefinition>,
}

/// A `posts` collection with a **unique** `title` (used to trigger a
/// mid-operation failure for the atomicity test) and seeded with `n` docs.
fn setup(n: usize) -> Setup {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();

    let mut def = CollectionDefinition::new("posts");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Post".to_string())),
        plural: Some(LocalizedString::Plain("Posts".to_string())),
    };
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .unique(true)
            .build(),
        FieldDefinition::builder("status", FieldType::Text).build(),
    ];

    let pool = pool::create_pool(tmp.path(), &config).expect("pool");
    let shared = Registry::shared();
    shared.write().unwrap().register_collection(def.clone());
    let registry = Registry::snapshot(&shared);
    migrate::sync_all(&pool, &registry, &config.locale).expect("sync");

    for i in 0..n {
        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!(format!("Title {i}")));
        data.insert("status".to_string(), json!("draft"));
        let mut conn = pool.get().expect("conn");
        let tx = conn.transaction().expect("tx");
        query::create(&tx, "posts", &def, &data, None).expect("create");
        tx.commit().expect("commit");
    }

    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .expect("hook runner");

    Setup {
        _tmp: tmp,
        pool,
        runner,
        def: Arc::new(def),
    }
}

fn ctx(s: &Setup) -> ServiceContext<'_> {
    ServiceContext::collection("posts", &s.def)
        .pool(&s.pool)
        .runner(&s.runner)
        .override_access(true)
        .build()
}

fn update_opts(max_documents: i64) -> UpdateManyOptions<'static> {
    UpdateManyOptions {
        locale_ctx: None,
        run_hooks: false,
        draft: false,
        ui_locale: None,
        max_documents,
    }
}

fn count_all(s: &Setup) -> usize {
    let conn = s.pool.get().expect("conn");
    let q = query::FindQuery::builder().build();
    query::find(&conn, "posts", &s.def, &q, None)
        .expect("find")
        .len()
}

fn item(title: &str) -> CreateManyItem {
    let mut data = DocumentFields::new();
    data.insert("title".to_string(), json!(title));
    data.insert("status".to_string(), json!("draft"));
    CreateManyItem {
        data,
        password: None,
    }
}

/// A minimal auth collection (`accounts`) for password-policy tests.
fn setup_auth() -> Setup {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();

    let mut def = CollectionDefinition::new("accounts");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Account".to_string())),
        plural: Some(LocalizedString::Plain("Accounts".to_string())),
    };
    def.fields = vec![
        FieldDefinition::builder("email", FieldType::Email)
            .required(true)
            .unique(true)
            .build(),
    ];
    def.auth = Some(Auth::enabled());

    let pool = pool::create_pool(tmp.path(), &config).expect("pool");
    let shared = Registry::shared();
    shared.write().unwrap().register_collection(def.clone());
    let registry = Registry::snapshot(&shared);
    migrate::sync_all(&pool, &registry, &config.locale).expect("sync");

    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .expect("hook runner");

    Setup {
        _tmp: tmp,
        pool,
        runner,
        def: Arc::new(def),
    }
}

fn auth_item(email: &str, password: Option<&str>) -> CreateManyItem {
    let mut data = DocumentFields::new();
    data.insert("email".to_string(), json!(email));
    CreateManyItem {
        data,
        password: password.map(std::string::ToString::to_string),
    }
}

fn bulk_opts() -> CreateManyOptions {
    CreateManyOptions {
        run_hooks: false,
        draft: false,
        max_documents: 0,
        locale_ctx: None,
    }
}

/// Regression (cross-surface harmonization): `create_many` on an auth
/// collection enforces the password policy at the service chokepoint — even
/// when the calling context did NOT thread a policy, the DEFAULT policy is the
/// fail-safe (min length 8). This is the Lua `create_many` weak-password hole,
/// now closed for every surface at one authoritative point.
#[test]
fn create_many_enforces_password_policy_even_without_threaded_policy() {
    let s = setup_auth();
    let ctx = ServiceContext::collection("accounts", &s.def)
        .pool(&s.pool)
        .runner(&s.runner)
        .override_access(true)
        // No `.password_policy(...)` on purpose: the chokepoint must still
        // enforce the default policy.
        .build();

    let items = vec![auth_item("a@x.com", Some("short"))];

    let err = create_many(&ctx, &items, &bulk_opts()).expect_err("weak password must be rejected");
    assert!(
        matches!(err, ServiceError::Validation(_)),
        "expected a Validation error, got {err:?}"
    );
}

/// A policy-compliant password is accepted and the users are created (proving
/// the relaxed `create_many` still hashes+persists, it just polices first).
#[test]
fn create_many_accepts_policy_compliant_password() {
    let s = setup_auth();
    let policy = PasswordPolicy::default();
    let ctx = ServiceContext::collection("accounts", &s.def)
        .pool(&s.pool)
        .runner(&s.runner)
        .override_access(true)
        .password_policy(Some(&policy))
        .build();

    let items = vec![
        auth_item("a@x.com", Some("longenough")),
        auth_item("b@x.com", Some("alsolongenough")),
    ];

    let result = create_many(&ctx, &items, &bulk_opts()).expect("valid passwords create");
    assert_eq!(result.created, 2);
}

/// A stricter threaded policy overrides the default: a password that passes the
/// default (>= 8) but fails the configured policy (>= 12) is rejected.
#[test]
fn create_many_honors_stricter_threaded_policy() {
    let s = setup_auth();
    let policy = PasswordPolicy {
        min_length: 12,
        ..PasswordPolicy::default()
    };
    let ctx = ServiceContext::collection("accounts", &s.def)
        .pool(&s.pool)
        .runner(&s.runner)
        .override_access(true)
        .password_policy(Some(&policy))
        .build();

    let items = vec![auth_item("a@x.com", Some("longenough"))];

    let err = create_many(&ctx, &items, &bulk_opts())
        .expect_err("password shorter than the configured minimum must be rejected");
    assert!(
        matches!(err, ServiceError::Validation(_)),
        "expected a Validation error, got {err:?}"
    );
}

fn count_with_status(s: &Setup, status: &str) -> usize {
    let conn = s.pool.get().expect("conn");
    let q = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Single(query::Filter {
            field: "status".to_string(),
            op: query::FilterOp::Equals(status.to_string()),
        })])
        .build();
    query::find(&conn, "posts", &s.def, &q, None)
        .expect("find")
        .len()
}

#[test]
fn update_many_over_cap_is_rejected_and_changes_nothing() {
    let s = setup(5);
    let mut data = DocumentFields::new();
    data.insert("status".to_string(), json!("published"));

    // Cap of 2, but the empty filter matches all 5.
    let err = update_many(
        &ctx(&s),
        &[],
        &data,
        &LocaleConfig::default(),
        &update_opts(2),
    )
    .expect_err("should exceed the cap");
    assert!(
        matches!(err, ServiceError::LimitExceeded(_)),
        "expected LimitExceeded, got: {err:?}"
    );

    // Nothing was changed — all 5 are still drafts.
    assert_eq!(count_with_status(&s, "published"), 0);
    assert_eq!(count_with_status(&s, "draft"), 5);
}

#[test]
fn delete_many_over_cap_is_rejected_and_changes_nothing() {
    let s = setup(5);
    let opts = DeleteManyOptions {
        run_hooks: false,
        max_documents: 2,
        ..Default::default()
    };

    let err = delete_many(&ctx(&s), &[], &LocaleConfig::default(), &opts)
        .expect_err("should exceed the cap");
    assert!(
        matches!(err, ServiceError::LimitExceeded(_)),
        "expected LimitExceeded, got: {err:?}"
    );

    assert_eq!(
        count_with_status(&s, "draft"),
        5,
        "nothing should be deleted"
    );
}

#[test]
fn update_many_unlimited_cap_updates_all() {
    let s = setup(5);
    let mut data = DocumentFields::new();
    data.insert("status".to_string(), json!("published"));

    // 0 = no limit.
    let result = update_many(
        &ctx(&s),
        &[],
        &data,
        &LocaleConfig::default(),
        &update_opts(0),
    )
    .expect("should succeed");
    assert_eq!(result.modified, 5);
    assert_eq!(count_with_status(&s, "published"), 5);
}

#[test]
fn create_many_over_cap_is_rejected_and_creates_nothing() {
    let s = setup(0);
    let items = vec![item("A"), item("B"), item("C"), item("D"), item("E")];
    let opts = CreateManyOptions {
        run_hooks: false,
        draft: false,
        max_documents: 2,
        locale_ctx: None,
    };

    let err = create_many(&ctx(&s), &items, &opts).expect_err("should exceed the cap");
    assert!(
        matches!(err, ServiceError::LimitExceeded(_)),
        "expected LimitExceeded, got: {err:?}"
    );
    assert_eq!(count_all(&s), 0, "nothing should be created");
}

#[test]
fn create_many_is_atomic_on_mid_operation_failure() {
    let s = setup(0);
    // Third item duplicates the first's (unique) title → the create fails
    // partway. The whole batch must roll back: zero docs created.
    let items = vec![item("A"), item("B"), item("A")];
    let opts = CreateManyOptions {
        run_hooks: false,
        draft: false,
        max_documents: 0,
        locale_ctx: None,
    };

    let err =
        create_many(&ctx(&s), &items, &opts).expect_err("duplicate title should abort the batch");
    assert!(
        !matches!(err, ServiceError::LimitExceeded(_)),
        "got: {err:?}"
    );
    assert_eq!(
        count_all(&s),
        0,
        "no document should remain after the batch rolled back"
    );
}

#[test]
fn update_many_is_atomic_on_mid_operation_failure() {
    let s = setup(5);

    // Setting every doc's (unique) title to the same value succeeds for the
    // first row and then violates the unique constraint — a failure partway
    // through the operation. The whole thing must roll back: no row keeps the
    // duplicate, and the original titles are intact.
    let mut data = DocumentFields::new();
    data.insert("title".to_string(), json!("DUPLICATE"));

    let err = update_many(
        &ctx(&s),
        &[],
        &data,
        &LocaleConfig::default(),
        &update_opts(0),
    )
    .expect_err("unique violation should abort the operation");
    // Not a cap error — a real mid-op failure.
    assert!(
        !matches!(err, ServiceError::LimitExceeded(_)),
        "got: {err:?}"
    );

    // Atomicity: no row was left with the duplicate title.
    let conn = s.pool.get().expect("conn");
    let q = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Single(query::Filter {
            field: "title".to_string(),
            op: query::FilterOp::Equals("DUPLICATE".to_string()),
        })])
        .build();
    let dupes = query::find(&conn, "posts", &s.def, &q, None).expect("find");
    assert_eq!(
        dupes.len(),
        0,
        "no row should have been committed with the duplicate title (operation rolled back)"
    );
}

/// A `posts` collection (defined in Lua) whose `before_delete` hook
/// unconditionally raises, so a per-document delete fails inside the bulk
/// transaction. Seeded with `n` docs. Used to exercise the delete-side atomic
/// rollback (the `create`/`update` cases use a unique-constraint violation; a
/// hard delete has no unique constraint to trip, so we fail it via a hook).
fn setup_with_delete_guard(n: usize) -> Setup {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    let hooks_dir = tmp.path().join("hooks");
    std::fs::create_dir_all(&collections_dir).unwrap();
    std::fs::create_dir_all(&hooks_dir).unwrap();

    std::fs::write(
        collections_dir.join("posts.lua"),
        r#"
crap.collections.define("posts", {
    fields = {
        { name = "title", type = "text", required = true },
        { name = "status", type = "text" },
    },
    hooks = {
        before_delete = { "hooks.guard.block_delete" },
    },
})
"#,
    )
    .unwrap();

    std::fs::write(
        hooks_dir.join("guard.lua"),
        r#"
local M = {}

function M.block_delete(_ctx)
    error("guard: deletion is blocked")
end

return M
"#,
    )
    .unwrap();

    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();

    let registry = hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let def = registry.get_collection("posts").expect("posts def").clone();

    let pool = pool::create_pool(tmp.path(), &config).expect("pool");
    migrate::sync_all(&pool, &registry, &config.locale).expect("sync");

    for i in 0..n {
        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!(format!("Title {i}")));
        data.insert("status".to_string(), json!("draft"));
        let mut conn = pool.get().expect("conn");
        let tx = conn.transaction().expect("tx");
        query::create(&tx, "posts", &def, &data, None).expect("create");
        tx.commit().expect("commit");
    }

    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .expect("hook runner");

    Setup {
        _tmp: tmp,
        pool,
        runner,
        def,
    }
}

#[test]
fn delete_many_is_atomic_on_mid_operation_failure() {
    let s = setup_with_delete_guard(3);

    // The `before_delete` hook raises, so the per-document delete errors inside
    // the bulk transaction. That error aborts the whole operation (the tx is
    // dropped without commit), so nothing is deleted — and it is a real
    // failure, not a cap rejection.
    let opts = DeleteManyOptions {
        run_hooks: true,
        max_documents: 0,
        ..Default::default()
    };

    let err = delete_many(&ctx(&s), &[], &LocaleConfig::default(), &opts)
        .expect_err("before_delete hook error should abort the operation");
    assert!(
        !matches!(err, ServiceError::LimitExceeded(_)),
        "expected a real failure, not a cap error, got: {err:?}"
    );

    assert_eq!(
        count_all(&s),
        3,
        "no document should be deleted after the batch rolled back"
    );
}

/// Regression: a bulk `update_many` fires per-document `before_change` collection
/// hooks via `update_many_single`. Those hooks must see the resolved content
/// `ctx.locale` (the default "en" when localization is enabled), not nil — the
/// `update_many_single` before-hook builder was missing `.locale(...)`.
#[test]
fn update_many_before_change_hook_sees_resolved_locale() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    let hooks_dir = tmp.path().join("hooks");
    std::fs::create_dir_all(&collections_dir).unwrap();
    std::fs::create_dir_all(&hooks_dir).unwrap();

    std::fs::write(
        collections_dir.join("posts.lua"),
        r#"
crap.collections.define("posts", {
    fields = {
        { name = "title", type = "text", required = true },
        { name = "status", type = "text" },
    },
    hooks = {
        before_change = { "hooks.lh.assert_locale" },
    },
})
"#,
    )
    .unwrap();

    std::fs::write(
        hooks_dir.join("lh.lua"),
        r#"
local M = {}

-- On a bulk update, the before_change hook must see the resolved content
-- locale ("en") and the admin ui_locale ("fr"), not nil.
function M.assert_locale(ctx)
    if ctx.operation == "update" then
        if ctx.locale ~= "en" then
            error("WRONG_LOCALE:" .. tostring(ctx.locale))
        end
        if ctx.ui_locale ~= "fr" then
            error("WRONG_UI_LOCALE:" .. tostring(ctx.ui_locale))
        end
    end
    return ctx
end

return M
"#,
    )
    .unwrap();

    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.locale.locales = vec!["en".to_string(), "de".to_string()];
    config.locale.default_locale = "en".to_string();

    let registry = hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let def = registry.get_collection("posts").expect("posts def").clone();

    let pool = pool::create_pool(tmp.path(), &config).expect("pool");
    migrate::sync_all(&pool, &registry, &config.locale).expect("sync");

    for i in 0..2 {
        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!(format!("Title {i}")));
        data.insert("status".to_string(), json!("draft"));
        let mut conn = pool.get().expect("conn");
        let tx = conn.transaction().expect("tx");
        query::create(&tx, "posts", &def, &data, None).expect("create");
        tx.commit().expect("commit");
    }

    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .expect("hook runner");

    let s = Setup {
        _tmp: tmp,
        pool,
        runner,
        def,
    };

    let lctx = LocaleContext {
        mode: LocaleMode::Default,
        config: config.locale.clone(),
    };
    let opts = UpdateManyOptions {
        locale_ctx: Some(&lctx),
        run_hooks: true,
        draft: false,
        ui_locale: Some("fr".to_string()),
        max_documents: 100,
    };
    let mut data = DocumentFields::new();
    data.insert("status".to_string(), json!("published"));

    // If the before_change hook saw `ctx.locale == nil` on update, it errors and
    // the bulk update fails.
    let result = update_many(&ctx(&s), &[], &data, &config.locale, &opts);
    assert!(
        result.is_ok(),
        "update_many before_change hook must see resolved locale 'en', got: {result:?}"
    );
}

/// Regression: pool-mode `create_many` must honor `ctx.override_access` on
/// its write hooks like its update/delete siblings. With `default_deny = true`
/// and no access fn, the runner denies everything — an override context (MCP)
/// must still create, and a non-override context must still be denied.
#[test]
fn create_many_override_access_bypasses_default_deny() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.access.default_deny = true;

    let mut def = CollectionDefinition::new("posts");
    def.fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];

    let pool = pool::create_pool(tmp.path(), &config).expect("pool");
    let shared = Registry::shared();
    shared.write().unwrap().register_collection(def.clone());
    let registry = Registry::snapshot(&shared);
    migrate::sync_all(&pool, &registry, &config.locale).expect("sync");

    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .expect("hook runner");

    // Non-override context: default-deny blocks the bulk create.
    let denied_ctx = ServiceContext::collection("posts", &def)
        .pool(&pool)
        .runner(&runner)
        .build();
    let one = |title: &str| {
        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!(title));
        CreateManyItem {
            data,
            password: None,
        }
    };

    let err = create_many(&denied_ctx, &[one("Denied")], &bulk_opts())
        .expect_err("default-deny must block a non-override bulk create");
    assert!(
        matches!(err, ServiceError::AccessDenied(_)),
        "expected AccessDenied, got {err:?}"
    );

    // Override context (MCP's Principal::Override): bypasses the gate. This
    // was the one bulk op missing `with_override_access` on its write hooks.
    let override_ctx = ServiceContext::collection("posts", &def)
        .pool(&pool)
        .runner(&runner)
        .override_access(true)
        .build();
    let result = create_many(&override_ctx, &[one("Allowed")], &bulk_opts())
        .expect("override_access must bypass default-deny on bulk create");
    assert_eq!(result.created, 1);
}

/// Regression (cache invalidation parity): the Lua conn-mode bulk paths
/// (`create_many`/`update_many`/`delete_many` on an existing connection)
/// must clear the populate cache exactly like the single-document conn
/// paths and the pool-mode bulk paths do — cache.md documents "every write
/// operation clears the entire cache". Previously the three `*_conn`
/// functions skipped the clear, serving stale populated reads after a Lua
/// bulk write.
#[test]
fn bulk_conn_paths_clear_cache() {
    let s = setup(3);
    let cache: Arc<MemoryCache> = Arc::new(MemoryCache::new(64));
    let shared: SharedCache = cache.clone();

    let seed = |c: &MemoryCache| c.set("k", b"v").expect("seed cache");
    let cleared = |c: &MemoryCache| !c.has("k").expect("has");

    // create_many on conn
    seed(&cache);
    {
        let mut conn = s.pool.get().expect("conn");
        let tx = conn.transaction().expect("tx");
        let wh = RunnerWriteHooks::new(&s.runner)
            .with_conn(&tx)
            .with_override_access();
        let ctx = ServiceContext::collection("posts", &s.def)
            .conn(&tx)
            .write_hooks(&wh)
            .override_access(true)
            .cache(Some(shared.clone()))
            .build();
        create_many(&ctx, &[item("Conn A")], &bulk_opts()).expect("create_many conn");
        drop(ctx);
        drop(wh);
        tx.commit().expect("commit");
    }
    assert!(
        cleared(&cache),
        "create_many conn path must clear the cache"
    );

    // update_many on conn
    seed(&cache);
    {
        let mut conn = s.pool.get().expect("conn");
        let tx = conn.transaction().expect("tx");
        let wh = RunnerWriteHooks::new(&s.runner)
            .with_conn(&tx)
            .with_override_access();
        let ctx = ServiceContext::collection("posts", &s.def)
            .conn(&tx)
            .write_hooks(&wh)
            .override_access(true)
            .cache(Some(shared.clone()))
            .build();
        let mut data = DocumentFields::new();
        data.insert("status".to_string(), json!("published"));
        update_many(
            &ctx,
            &[],
            &data,
            &LocaleConfig::default(),
            &update_opts(100),
        )
        .expect("update_many conn");
        drop(ctx);
        drop(wh);
        tx.commit().expect("commit");
    }
    assert!(
        cleared(&cache),
        "update_many conn path must clear the cache"
    );

    // delete_many on conn
    seed(&cache);
    {
        let mut conn = s.pool.get().expect("conn");
        let tx = conn.transaction().expect("tx");
        let wh = RunnerWriteHooks::new(&s.runner)
            .with_conn(&tx)
            .with_override_access();
        let ctx = ServiceContext::collection("posts", &s.def)
            .conn(&tx)
            .write_hooks(&wh)
            .override_access(true)
            .cache(Some(shared.clone()))
            .build();
        delete_many(
            &ctx,
            &[],
            &LocaleConfig::default(),
            &DeleteManyOptions {
                run_hooks: false,
                include_deleted: false,
                max_documents: 100,
            },
        )
        .expect("delete_many conn");
        drop(ctx);
        drop(wh);
        tx.commit().expect("commit");
    }
    assert!(
        cleared(&cache),
        "delete_many conn path must clear the cache"
    );
}
