//! Accounts created through Lua CRUD outside a service write — in a job
//! handler, a custom route, an `on_init` hook or a data migration — get their
//! verification token and email exactly like an account created through the
//! service layer.
//!
//! Regression: those surfaces carried no email context, and their per-write
//! verification queue had nowhere to go after the commit, so the pending
//! verification was dropped silently: the account existed, but no token was
//! ever minted and no email queued — the user could neither verify nor sign
//! up again.

#![allow(clippy::missing_panics_doc)]

use std::{collections::HashMap, fs, path::Path, sync::Arc};

use tempfile::TempDir;

use crap_cms::{
    config::CrapConfig,
    core::{
        HookRef, JobDefinition, JobRun, Registry,
        collection::{Auth, CollectionDefinition},
        field::{FieldDefinition, FieldType},
        upload,
    },
    db::{DbConnection, DbValue, migrate, pool},
    hooks::{
        LuaCrudInfra,
        lifecycle::{HookRunner, MigrationCall, RouteHandlerInput},
    },
    service::{AppInfra, StandaloneInfra},
};

/// One handler per surface, each creating an account on the `members`
/// verify-email collection.
const SEED_HOOKS: &str = r#"
local M = {}

local function create(email)
    crap.collections.create("members", {
        email = email,
        password = "correct-horse-battery-staple",
    })
end

function M.job(ctx)
    create("job@example.test")
end

function M.on_init(ctx)
    create("init@example.test")
end

function M.route(ctx)
    create("route@example.test")

    return { status = 201 }
end

return M
"#;

const SEED_MIGRATION: &str = r#"
local M = {}

function M.up()
    crap.collections.create("members", {
        email = "migration@example.test",
        password = "correct-horse-battery-staple",
    })
end

function M.down() end

return M
"#;

struct Ctx {
    tmp: TempDir,
    infra: Arc<AppInfra>,
}

fn members() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("members");
    def.fields = vec![
        FieldDefinition::builder("email", FieldType::Email)
            .required(true)
            .build(),
    ];
    def.auth = Some(Auth::enabled().map_password_login(|b| b.verify_email(true)));

    def
}

fn write_fixtures(dir: &Path) {
    fs::create_dir_all(dir.join("hooks")).expect("hooks dir");
    fs::write(dir.join("hooks/seed.lua"), SEED_HOOKS).expect("hook file");

    fs::create_dir_all(dir.join("migrations")).expect("migrations dir");
    fs::write(dir.join("migrations/0001_seed.lua"), SEED_MIGRATION).expect("migration file");
}

fn setup() -> Ctx {
    let tmp = tempfile::tempdir().expect("tempdir");
    write_fixtures(tmp.path());

    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    // A configured transport, so verification emails are actually queued.
    // Nothing is sent: the test never runs the `_system_email` job.
    config.email.smtp_host = "smtp.example.test".to_string();

    let db_pool = pool::create_pool(tmp.path(), &config).expect("pool");

    let shared = Registry::shared();
    shared
        .write()
        .expect("registry")
        .register_collection(members());
    let registry = Registry::snapshot(&shared);
    migrate::sync_all(&db_pool, &registry, &config.locale).expect("sync schema");

    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .expect("hook runner");
    let storage = upload::create_storage(tmp.path(), &config.upload).expect("storage backend");

    let infra = AppInfra::standalone(StandaloneInfra {
        pool: db_pool,
        registry,
        hook_runner: runner,
        storage,
        token_provider: None,
        event_transport: None,
        invalidation_transport: None,
        config: &config,
        config_dir: tmp.path(),
    })
    .expect("infra");

    Ctx { tmp, infra }
}

fn crud_infra(ctx: &Ctx) -> LuaCrudInfra {
    LuaCrudInfra::for_pool_crud(&ctx.infra)
}

/// The account exists, carries a verification token, and exactly one
/// `_system_email` job addressed to it is queued.
fn assert_verification_issued(ctx: &Ctx, email: &str) {
    let conn = ctx.infra.pool.get().expect("connection");

    let account = conn
        .query_one(
            "SELECT _verification_token FROM members WHERE email = ?1",
            &[DbValue::Text(email.to_string())],
        )
        .expect("read account")
        .unwrap_or_else(|| panic!("the account {email} was not created"));

    assert!(
        account.opt_text_at(0).is_some(),
        "no verification token was minted for {email}"
    );

    let queued = conn
        .query_one(
            "SELECT COUNT(*) FROM _crap_jobs WHERE slug = '_system_email' AND data LIKE ?1",
            &[DbValue::Text(format!("%{email}%"))],
        )
        .expect("count email jobs")
        .and_then(|r| r.i64_at(0));

    assert_eq!(
        queued,
        Some(1),
        "exactly one verification email must be queued for {email}"
    );
}

#[test]
fn an_account_created_by_a_job_gets_a_verification() {
    let ctx = setup();
    let job = JobDefinition::builder("seed", "hooks.seed.job").build();
    let run = JobRun::builder("seed-run", "seed").data("{}").build();

    ctx.infra
        .hook_runner
        .run_job_handler(&job, &run, &ctx.infra.pool, Some(crud_infra(&ctx)))
        .expect("job handler");

    assert_verification_issued(&ctx, "job@example.test");
}

#[test]
fn an_account_created_by_on_init_gets_a_verification() {
    let ctx = setup();

    ctx.infra
        .hook_runner
        .run_system_hooks_in_tx(
            &["hooks.seed.on_init".to_string()],
            &ctx.infra.pool,
            Some(crud_infra(&ctx)),
        )
        .expect("on_init hooks");

    assert_verification_issued(&ctx, "init@example.test");
}

#[test]
fn an_account_created_by_a_custom_route_gets_a_verification() {
    let ctx = setup();
    let input = RouteHandlerInput {
        method: "POST".to_string(),
        path: "/seed".to_string(),
        params: HashMap::new(),
        query: HashMap::new(),
        headers: HashMap::new(),
        cookies: HashMap::new(),
        body: None,
        json: None,
        form: None,
        user: None,
        collection: None,
        ip: "127.0.0.1".to_string(),
        ui_locale: None,
        options: None,
    };

    ctx.infra
        .hook_runner
        .run_route_handler(
            &HookRef::new("hooks.seed.route"),
            &input,
            &ctx.infra.pool,
            Some(crud_infra(&ctx)),
        )
        .expect("route handler");

    assert_verification_issued(&ctx, "route@example.test");
}

#[test]
fn an_account_created_by_a_migration_gets_a_verification() {
    let ctx = setup();
    let path = ctx.tmp.path().join("migrations/0001_seed.lua");

    ctx.infra
        .hook_runner
        .run_migration(
            &MigrationCall::new(&path, "up"),
            &ctx.infra.pool,
            Some(crud_infra(&ctx)),
            |_| Ok(()),
        )
        .expect("migration");

    assert_verification_issued(&ctx, "migration@example.test");
}
