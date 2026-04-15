//! Globals lifecycle hook coverage.
//!
//! Globals fire `before_validate`, `before_change`, and `after_read` hooks
//! just like collections. This suite drives the service-layer update/read
//! paths against a globals-only fixture and asserts each phase fires and
//! affects the observable result.

use std::collections::HashMap;
use std::path::PathBuf;

use crap_cms::config::CrapConfig;
use crap_cms::db::{migrate, pool, query};
use crap_cms::hooks;
use crap_cms::hooks::lifecycle::HookRunner;
use crap_cms::service::{
    GetGlobalInput, RunnerReadHooks, RunnerWriteHooks, ServiceContext, WriteInput,
    get_global_document, update_global_core,
};
use serde_json::Value;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/globals_hook_tests")
}

fn setup() -> (
    tempfile::TempDir,
    crap_cms::db::DbPool,
    crap_cms::core::SharedRegistry,
    HookRunner,
) {
    let config_dir = fixture_dir();
    let config = CrapConfig::test_default();
    let registry = hooks::init_lua(&config_dir, &config).expect("init lua");

    let tmp = tempfile::tempdir().expect("tempdir");
    let mut pool_config = CrapConfig::test_default();
    pool_config.database.path = "test.db".to_string();
    let db_pool = pool::create_pool(tmp.path(), &pool_config).expect("create pool");
    migrate::sync_all(&db_pool, &registry, &config.locale).expect("sync schema");

    let runner = HookRunner::builder()
        .config_dir(&config_dir)
        .registry(registry.clone())
        .config(&config)
        .build()
        .expect("build runner");

    (tmp, db_pool, registry, runner)
}

fn seed_global_tagline(
    pool: &crap_cms::db::DbPool,
    registry: &crap_cms::core::SharedRegistry,
    slug: &str,
    tagline: &str,
) {
    let reg = registry.read().unwrap();
    let def = reg.get_global(slug).expect("global not found").clone();
    drop(reg);

    let mut data = HashMap::new();
    data.insert("tagline".to_string(), tagline.to_string());

    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    query::update_global(&tx, slug, &def, &data, None).expect("seed update");
    tx.commit().expect("commit");
}

// ── before_validate ──────────────────────────────────────────────────────

#[test]
fn global_before_validate_hook_fires() {
    let (_tmp, pool, registry, runner) = setup();
    let reg = registry.read().unwrap();
    let def = reg.get_global("site_settings").unwrap().clone();
    drop(reg);

    let mut data = HashMap::new();
    data.insert("title".to_string(), "  Padded Title  ".to_string());
    let join_data = HashMap::new();

    let mut conn = pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let wh = RunnerWriteHooks::new(&runner).with_conn(&tx);
    let ctx = ServiceContext::global("site_settings", &def)
        .conn(&tx)
        .write_hooks(&wh)
        .build();

    let input = WriteInput::builder(data, &join_data).build();
    let (doc, _after) = update_global_core(&ctx, input).expect("update should succeed");
    tx.commit().unwrap();

    // before_validate trimmed the title.
    assert_eq!(
        doc.fields.get("title").and_then(Value::as_str),
        Some("Padded Title"),
        "before_validate hook should have trimmed whitespace"
    );
}

// ── before_change (abort) ────────────────────────────────────────────────

#[test]
fn global_before_change_hook_fires() {
    let (_tmp, pool, registry, runner) = setup();
    let reg = registry.read().unwrap();
    let def = reg.get_global("site_settings").unwrap().clone();
    drop(reg);

    let mut data = HashMap::new();
    data.insert("site_name".to_string(), "POISON".to_string());
    let join_data = HashMap::new();

    let mut conn = pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let wh = RunnerWriteHooks::new(&runner).with_conn(&tx);
    let ctx = ServiceContext::global("site_settings", &def)
        .conn(&tx)
        .write_hooks(&wh)
        .build();

    let input = WriteInput::builder(data, &join_data).build();
    let err =
        update_global_core(&ctx, input).expect_err("before_change hook should abort the update");
    let msg = err.to_string();
    assert!(
        msg.contains("aborted by before_change hook")
            || msg.to_lowercase().contains("before_change"),
        "error should mention the hook abort, got: {msg}"
    );
}

// ── after_read ───────────────────────────────────────────────────────────

#[test]
fn global_after_read_hook_fires() {
    let (_tmp, pool, registry, runner) = setup();

    // Seed the global with a mixed-case tagline bypassing hooks so we can
    // verify after_read uppercases it on the read path.
    seed_global_tagline(&pool, &registry, "site_settings", "hello world");

    let reg = registry.read().unwrap();
    let def = reg.get_global("site_settings").unwrap().clone();
    drop(reg);

    let conn = pool.get().unwrap();
    let rh = RunnerReadHooks::new(&runner, &conn);
    let ctx = ServiceContext::global("site_settings", &def)
        .conn(&conn)
        .read_hooks(&rh)
        .build();

    let input = GetGlobalInput::new(None, None);
    let doc = get_global_document(&ctx, &input).expect("read should succeed");

    assert_eq!(
        doc.fields.get("tagline").and_then(Value::as_str),
        Some("HELLO WORLD"),
        "after_read hook should have uppercased the tagline"
    );
}

// ── Access: non-admin read is denied, admin is allowed ───────────────────

#[test]
fn global_read_access_denied_for_non_admin() {
    let (_tmp, pool, registry, runner) = setup();
    let reg = registry.read().unwrap();
    let def = reg.get_global("restricted").unwrap().clone();
    drop(reg);

    // Editor user (not admin) — admin_only returns false.
    let mut editor_fields = HashMap::new();
    editor_fields.insert("role".to_string(), serde_json::json!("editor"));
    let editor = crap_cms::core::Document {
        id: "editor-1".into(),
        fields: editor_fields,
        created_at: None,
        updated_at: None,
    };

    let conn = pool.get().unwrap();
    let rh = RunnerReadHooks::new(&runner, &conn);
    let ctx = ServiceContext::global("restricted", &def)
        .conn(&conn)
        .read_hooks(&rh)
        .user(Some(&editor))
        .build();

    let input = GetGlobalInput::new(None, None);
    let err =
        get_global_document(&ctx, &input).expect_err("non-admin should be denied read access");
    let msg = err.to_string();
    assert!(
        msg.to_lowercase().contains("access denied") || msg.to_lowercase().contains("denied"),
        "error should mention access denied, got: {msg}"
    );
}

#[test]
fn global_update_access_denied_for_non_admin() {
    let (_tmp, pool, registry, runner) = setup();
    let reg = registry.read().unwrap();
    let def = reg.get_global("restricted").unwrap().clone();
    drop(reg);

    let mut editor_fields = HashMap::new();
    editor_fields.insert("role".to_string(), serde_json::json!("editor"));
    let editor = crap_cms::core::Document {
        id: "editor-1".into(),
        fields: editor_fields,
        created_at: None,
        updated_at: None,
    };

    let mut conn = pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let wh = RunnerWriteHooks::new(&runner).with_conn(&tx);
    let ctx = ServiceContext::global("restricted", &def)
        .conn(&tx)
        .write_hooks(&wh)
        .user(Some(&editor))
        .build();

    let mut data = HashMap::new();
    data.insert("secret_value".to_string(), "Hacked".to_string());
    let join_data = HashMap::new();
    let input = WriteInput::builder(data, &join_data).build();

    let err =
        update_global_core(&ctx, input).expect_err("non-admin should be denied update access");
    let msg = err.to_string();
    assert!(
        msg.to_lowercase().contains("access denied") || msg.to_lowercase().contains("denied"),
        "error should mention access denied, got: {msg}"
    );
}
