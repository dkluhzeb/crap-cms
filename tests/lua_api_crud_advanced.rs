use std::collections::HashMap;
use std::path::PathBuf;

use crap_cms::config::CrapConfig;
use crap_cms::core::SharedRegistry;
use crap_cms::db::DbPool;
use crap_cms::hooks;
use crap_cms::hooks::lifecycle::HookRunner;
use serde_json::json;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hook_tests")
}

fn setup_lua() -> HookRunner {
    let config_dir = fixture_dir();
    let config = CrapConfig::default();
    let registry = hooks::init_lua(&config_dir, &config).expect("init_lua failed");
    HookRunner::builder()
        .config_dir(&config_dir)
        .registry(registry)
        .config(&config)
        .build()
        .expect("HookRunner::new failed")
}

/// Helper to eval Lua code and get a string result (no DB connection needed for pure functions).
/// This uses a temporary in-memory DB for the eval.
fn eval_lua(runner: &HookRunner, code: &str) -> String {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    let pool = crap_cms::db::pool::create_pool(tmp.path(), &config).expect("pool");
    let conn = pool.get().expect("conn");
    runner
        .eval_lua_with_conn(code, &conn, None)
        .expect("eval failed")
}

// ── Helper: setup with real DB tables ────────────────────────────────────────

/// Set up a HookRunner with a real synced database (tables created from Lua definitions).
/// Returns (tempdir, pool, registry, runner). The tempdir must be kept alive for the DB.
#[allow(dead_code)]
fn setup_with_db() -> (tempfile::TempDir, DbPool, SharedRegistry, HookRunner) {
    let config_dir = fixture_dir();
    let config = CrapConfig::default();
    let registry = hooks::init_lua(&config_dir, &config).expect("init_lua failed");

    // Create a pool and sync tables from Lua-defined collections/globals
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut db_config = CrapConfig::default();
    db_config.database.path = "test.db".to_string();
    let pool = crap_cms::db::pool::create_pool(tmp.path(), &db_config).expect("pool");
    crap_cms::db::migrate::sync_all(&pool, &registry, &config.locale).expect("sync failed");

    let runner = HookRunner::builder()
        .config_dir(&config_dir)
        .registry(registry.clone())
        .config(&config)
        .build()
        .expect("HookRunner::new failed");
    (tmp, pool, registry, runner)
}

/// Helper to eval Lua code with a real synced DB connection. CRUD functions work here.
#[allow(dead_code)]
fn eval_lua_db(runner: &HookRunner, pool: &DbPool, code: &str) -> String {
    let conn = pool.get().expect("conn");
    runner
        .eval_lua_with_conn(code, &conn, None)
        .expect("eval failed")
}

// ── Lua CRUD Functions ───────────────────────────────────────────────────────

#[test]
fn util_clone() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local orig = { a = 1, b = 2 }
        local copy = crap.util.clone(orig)
        copy.a = 99
        if orig.a ~= 1 then return "ORIGINAL_MODIFIED" end
        if copy.a ~= 99 then return "COPY_WRONG" end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

// ══════════════════════════════════════════════════════════════════════════════
// NEW FEATURES: crap.util -- pure Lua string helpers
// ══════════════════════════════════════════════════════════════════════════════

#[test]
fn util_trim() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local trimmed = crap.util.trim("  hello world  ")
        return trimmed
    "#,
    );
    assert_eq!(result, "hello world");
}

#[test]
fn util_split() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local parts = crap.util.split("a,b,c", ",")
        if #parts ~= 3 then return "LEN:" .. #parts end
        if parts[1] ~= "a" then return "P1:" .. parts[1] end
        if parts[2] ~= "b" then return "P2:" .. parts[2] end
        if parts[3] ~= "c" then return "P3:" .. parts[3] end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn util_starts_with_ends_with() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        if not crap.util.starts_with("hello world", "hello") then return "SW_FAIL" end
        if crap.util.starts_with("hello world", "world") then return "SW_FALSE_POS" end
        if not crap.util.ends_with("hello world", "world") then return "EW_FAIL" end
        if crap.util.ends_with("hello world", "hello") then return "EW_FALSE_POS" end
        if not crap.util.ends_with("test", "") then return "EW_EMPTY" end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn util_truncate() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local short = crap.util.truncate("hello", 10)
        if short ~= "hello" then return "SHORT:" .. short end

        local truncated = crap.util.truncate("hello world", 8)
        if truncated ~= "hello..." then return "TRUNC:" .. truncated end

        local custom = crap.util.truncate("hello world", 8, "~")
        if custom ~= "hello w~" then return "CUSTOM:" .. custom end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

// ══════════════════════════════════════════════════════════════════════════════
// NEW FEATURES: crap.util -- date helpers (Rust/chrono)
// ══════════════════════════════════════════════════════════════════════════════

#[test]
fn util_date_now_returns_iso_string() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local now = crap.util.date_now()
        -- Should contain 'T' (ISO 8601 separator) and be non-empty
        if #now < 10 then return "TOO_SHORT:" .. now end
        if not now:find("T") then return "NO_T:" .. now end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn util_date_timestamp_returns_number() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local ts = crap.util.date_timestamp()
        if type(ts) ~= "number" then return "NOT_NUMBER:" .. type(ts) end
        -- Sanity check: timestamp should be after 2024
        if ts < 1700000000 then return "TOO_OLD:" .. tostring(ts) end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn util_date_parse_rfc3339() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local ts = crap.util.date_parse("2024-01-15T12:30:00+00:00")
        if ts ~= 1705321800 then return "WRONG:" .. tostring(ts) end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn util_date_parse_date_only() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local ts = crap.util.date_parse("2024-01-01")
        if ts ~= 1704067200 then return "WRONG:" .. tostring(ts) end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn util_date_parse_datetime() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local ts = crap.util.date_parse("2024-01-01 12:00:00")
        if ts ~= 1704110400 then return "WRONG:" .. tostring(ts) end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn util_date_parse_invalid() {
    let runner = setup_lua();
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    let pool = crap_cms::db::pool::create_pool(tmp.path(), &config).expect("pool");
    let conn = pool.get().expect("conn");
    let result = runner
        .eval_lua_with_conn(
            r#"
        local ok, err = pcall(function()
            crap.util.date_parse("not-a-date")
        end)
        if ok then return "SHOULD_HAVE_FAILED" end
        if tostring(err):find("could not parse") then return "ok" end
        return "UNEXPECTED:" .. tostring(err)
    "#,
            &conn,
            None,
        )
        .expect("eval");
    assert_eq!(result, "ok");
}

#[test]
fn util_date_format() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        -- 2024-01-15 12:30:00 UTC
        local formatted = crap.util.date_format(1705321800, "%Y-%m-%d")
        if formatted ~= "2024-01-15" then return "WRONG:" .. formatted end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn util_date_add_and_diff() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local ts = 1000000
        local added = crap.util.date_add(ts, 3600)
        if added ~= 1003600 then return "ADD:" .. tostring(added) end

        local diff = crap.util.date_diff(added, ts)
        if diff ~= 3600 then return "DIFF:" .. tostring(diff) end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

// ══════════════════════════════════════════════════════════════════════════════
// NEW FEATURES: crap.crypto
// ══════════════════════════════════════════════════════════════════════════════

#[test]
fn crypto_sha256() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local hash = crap.crypto.sha256("hello")
        -- Known SHA-256 of "hello"
        if hash ~= "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824" then
            return "WRONG:" .. hash
        end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn crypto_sha256_empty() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local hash = crap.crypto.sha256("")
        -- Known SHA-256 of empty string
        if hash ~= "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855" then
            return "WRONG:" .. hash
        end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn crypto_hmac_sha256() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local mac = crap.crypto.hmac_sha256("hello", "secret-key")
        -- Should be 64 hex characters (32 bytes)
        if #mac ~= 64 then return "LEN:" .. #mac end
        -- Should be deterministic
        local mac2 = crap.crypto.hmac_sha256("hello", "secret-key")
        if mac ~= mac2 then return "NOT_DETERMINISTIC" end
        -- Different key should give different result
        local mac3 = crap.crypto.hmac_sha256("hello", "other-key")
        if mac == mac3 then return "SAME_WITH_DIFF_KEY" end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn crypto_base64_roundtrip() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local original = "Hello, World! 123 Special chars: @#$%"
        local encoded = crap.crypto.base64_encode(original)
        local decoded = crap.crypto.base64_decode(encoded)
        if decoded ~= original then
            return "MISMATCH:" .. decoded
        end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn crypto_base64_known_value() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local encoded = crap.crypto.base64_encode("hello")
        if encoded ~= "aGVsbG8=" then return "WRONG:" .. encoded end
        local decoded = crap.crypto.base64_decode("aGVsbG8=")
        if decoded ~= "hello" then return "DECODE_WRONG:" .. decoded end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn crypto_encrypt_decrypt_roundtrip() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local plaintext = "secret message 123!"
        local encrypted = crap.crypto.encrypt(plaintext)
        -- Encrypted should be different from plaintext
        if encrypted == plaintext then return "NOT_ENCRYPTED" end
        -- Should be base64 encoded
        if #encrypted < #plaintext then return "TOO_SHORT" end

        local decrypted = crap.crypto.decrypt(encrypted)
        if decrypted ~= plaintext then
            return "MISMATCH:" .. decrypted
        end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn crypto_encrypt_produces_different_ciphertexts() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        -- Same plaintext should produce different ciphertexts (random nonce)
        local a = crap.crypto.encrypt("same text")
        local b = crap.crypto.encrypt("same text")
        if a == b then return "SAME_CIPHERTEXT" end
        -- But both should decrypt to the same thing
        if crap.crypto.decrypt(a) ~= "same text" then return "A_WRONG" end
        if crap.crypto.decrypt(b) ~= "same text" then return "B_WRONG" end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn crypto_decrypt_invalid_input() {
    let runner = setup_lua();
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    let pool = crap_cms::db::pool::create_pool(tmp.path(), &config).expect("pool");
    let conn = pool.get().expect("conn");
    let result = runner
        .eval_lua_with_conn(
            r#"
        local ok, err = pcall(function()
            crap.crypto.decrypt("not-valid-base64!@#$")
        end)
        if ok then return "SHOULD_HAVE_FAILED" end
        return "ok"
    "#,
            &conn,
            None,
        )
        .expect("eval");
    assert_eq!(result, "ok");
}

#[test]
fn crypto_random_bytes() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local hex = crap.crypto.random_bytes(16)
        -- 16 bytes = 32 hex characters
        if #hex ~= 32 then return "LEN:" .. #hex end
        -- Should be hex (only 0-9a-f)
        if hex:find("[^0-9a-f]") then return "NOT_HEX:" .. hex end
        -- Two calls should produce different results
        local hex2 = crap.crypto.random_bytes(16)
        if hex == hex2 then return "SAME" end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn crypto_random_bytes_various_sizes() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local h1 = crap.crypto.random_bytes(1)
        if #h1 ~= 2 then return "1B:" .. #h1 end
        local h32 = crap.crypto.random_bytes(32)
        if #h32 ~= 64 then return "32B:" .. #h32 end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

// ══════════════════════════════════════════════════════════════════════════════
// NEW FEATURES: crap.schema
// ══════════════════════════════════════════════════════════════════════════════

#[test]
fn schema_get_collection() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local schema = crap.schema.get_collection("articles")
        if schema == nil then return "NIL" end
        if schema.slug ~= "articles" then return "SLUG:" .. tostring(schema.slug) end
        if schema.labels.singular ~= "Article" then return "SINGULAR:" .. tostring(schema.labels.singular) end
        if schema.labels.plural ~= "Articles" then return "PLURAL:" .. tostring(schema.labels.plural) end
        if #schema.fields < 1 then return "NO_FIELDS" end
        -- Check first field
        local title_field = nil
        for _, f in ipairs(schema.fields) do
            if f.name == "title" then title_field = f; break end
        end
        if title_field == nil then return "NO_TITLE" end
        if title_field.type ~= "text" then return "TITLE_TYPE:" .. title_field.type end
        if not title_field.required then return "TITLE_NOT_REQUIRED" end
        if not title_field.unique then return "TITLE_NOT_UNIQUE" end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn schema_get_collection_nonexistent() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local schema = crap.schema.get_collection("nonexistent")
        if schema ~= nil then return "NOT_NIL" end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn schema_get_global() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local schema = crap.schema.get_global("settings")
        if schema == nil then return "NIL" end
        if schema.slug ~= "settings" then return "SLUG:" .. tostring(schema.slug) end
        if #schema.fields ~= 2 then return "FIELDS:" .. tostring(#schema.fields) end
        if schema.fields[1].name ~= "site_name" then return "F1:" .. schema.fields[1].name end
        if schema.fields[2].name ~= "maintenance_mode" then return "F2:" .. schema.fields[2].name end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn schema_get_global_nonexistent() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local schema = crap.schema.get_global("nonexistent")
        if schema ~= nil then return "NOT_NIL" end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn schema_list_collections() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local list = crap.schema.list_collections()
        if #list < 1 then return "EMPTY" end
        -- Should contain articles
        local found = false
        for _, item in ipairs(list) do
            if item.slug == "articles" then
                found = true
                if item.labels.singular ~= "Article" then
                    return "LABEL:" .. tostring(item.labels.singular)
                end
            end
        end
        if not found then return "NO_ARTICLES" end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn schema_list_globals() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local list = crap.schema.list_globals()
        if #list < 1 then return "EMPTY" end
        -- Should contain settings
        local found = false
        for _, item in ipairs(list) do
            if item.slug == "settings" then
                found = true
            end
        end
        if not found then return "NO_SETTINGS" end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn schema_collection_metadata() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local schema = crap.schema.get_collection("articles")
        -- articles fixture doesn't have auth/upload/versions
        if schema.has_auth then return "HAS_AUTH" end
        if schema.has_upload then return "HAS_UPLOAD" end
        if schema.has_versions then return "HAS_VERSIONS" end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn schema_field_with_options() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local schema = crap.schema.get_collection("articles")
        -- Find the status field which has select options
        local status_field = nil
        for _, f in ipairs(schema.fields) do
            if f.name == "status" then status_field = f; break end
        end
        if status_field == nil then return "NO_STATUS" end
        if status_field.type ~= "select" then return "TYPE:" .. status_field.type end
        if #status_field.options ~= 9 then return "OPTS:" .. #status_field.options end
        if status_field.options[1].value ~= "draft" then return "OPT1:" .. status_field.options[1].value end
        if status_field.options[2].value ~= "published" then return "OPT2:" .. status_field.options[2].value end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

// ══════════════════════════════════════════════════════════════════════════════
// NEW FEATURES: crap.context -- request-scoped shared table
// ══════════════════════════════════════════════════════════════════════════════

#[test]
fn context_flows_through_hooks() {
    // Test that context set in before_validate is available in before_change and after_change
    use crap_cms::hooks::lifecycle::{HookContext, HookEvent};

    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    let hooks_dir = tmp.path().join("hooks");
    std::fs::create_dir_all(&collections_dir).unwrap();
    std::fs::create_dir_all(&hooks_dir).unwrap();

    std::fs::write(
        hooks_dir.join("context_test.lua"),
        r#"
local M = {}

function M.before_validate(ctx)
    ctx.context.step1 = "validated"
    ctx.context.counter = 1
    return ctx
end

function M.before_change(ctx)
    -- Should see values from before_validate
    if ctx.context.step1 ~= "validated" then
        error("context.step1 missing in before_change")
    end
    ctx.context.step2 = "changed"
    ctx.context.counter = (ctx.context.counter or 0) + 1
    return ctx
end

return M
        "#,
    )
    .unwrap();

    std::fs::write(
        collections_dir.join("items.lua"),
        r#"
crap.collections.define("items", {
    fields = {
        { name = "name", type = "text" },
    },
    hooks = {
        before_validate = { "hooks.context_test.before_validate" },
        before_change = { "hooks.context_test.before_change" },
    },
})
        "#,
    )
    .unwrap();

    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::default();
    let registry = hooks::init_lua(tmp.path(), &config).expect("init_lua");

    let mut db_config = CrapConfig::default();
    db_config.database.path = "test.db".to_string();
    let pool = crap_cms::db::pool::create_pool(tmp.path(), &db_config).expect("pool");
    crap_cms::db::migrate::sync_all(&pool, &registry, &config.locale).expect("sync");

    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(registry.clone())
        .config(&config)
        .build()
        .expect("runner");

    let reg = registry.read().unwrap();
    let def = reg.get_collection("items").expect("items");

    let mut data = HashMap::new();
    data.insert("name".to_string(), json!("test"));

    let ctx = HookContext {
        collection: "items".to_string(),
        operation: "create".to_string(),
        data,
        locale: None,
        draft: None,
        context: HashMap::new(),
        user: None,
        ui_locale: None,
    };

    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");

    // Run before_validate
    let ctx = runner
        .run_hooks_with_conn(&def.hooks, HookEvent::BeforeValidate, ctx, &tx)
        .expect("before_validate");

    assert_eq!(
        ctx.context.get("step1"),
        Some(&json!("validated")),
        "step1 should be set after before_validate"
    );

    // Run before_change -- should see context from before_validate
    let ctx = runner
        .run_hooks_with_conn(&def.hooks, HookEvent::BeforeChange, ctx, &tx)
        .expect("before_change");

    assert_eq!(
        ctx.context.get("step1"),
        Some(&json!("validated")),
        "step1 should persist through before_change"
    );
    assert_eq!(
        ctx.context.get("step2"),
        Some(&json!("changed")),
        "step2 should be set after before_change"
    );
    assert_eq!(
        ctx.context.get("counter"),
        Some(&json!(2)),
        "counter should be incremented by both hooks"
    );
}

#[test]
fn context_starts_empty() {
    use crap_cms::hooks::lifecycle::HookContext;

    let ctx = HookContext {
        collection: "test".to_string(),
        operation: "create".to_string(),
        data: HashMap::new(),
        locale: None,
        draft: None,
        context: HashMap::new(),
        user: None,
        ui_locale: None,
    };

    assert!(ctx.context.is_empty(), "Context should start empty");
}

// ── After-Hook CRUD Access Tests ─────────────────────────────────────────────

#[test]
fn after_hook_has_crud_access() {
    use crap_cms::core::collection::Hooks;
    use crap_cms::hooks::lifecycle::{HookContext, HookEvent};

    let (_tmp, pool, registry, runner) = setup_with_db();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    // Build hooks with an after_change hook that creates a side-effect document
    let hooks = Hooks {
        after_change: vec!["hooks.after_crud.create_side_effect".to_string()],
        ..Default::default()
    };

    // First, create a document so the after-hook has something to work with
    let mut conn = pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = [
        ("title".to_string(), "original".to_string()),
        ("status".to_string(), "published".to_string()),
    ]
    .into();
    let doc = crap_cms::db::query::create(&tx, "articles", &def, &data, None).unwrap();

    // Run after_change hooks inside the same transaction
    let ctx = HookContext {
        collection: "articles".to_string(),
        operation: "create".to_string(),
        data: doc.fields.clone(),
        locale: None,
        draft: None,
        context: std::collections::HashMap::new(),
        user: None,
        ui_locale: None,
    };
    let result = runner.run_after_write(&hooks, &def.fields, HookEvent::AfterChange, ctx, &tx);
    assert!(
        result.is_ok(),
        "after_change hook with CRUD should succeed: {:?}",
        result.err()
    );

    // Commit the transaction
    tx.commit().unwrap();

    // Verify the side-effect document was created
    let conn2 = pool.get().unwrap();
    let docs = crap_cms::db::query::find(
        &conn2,
        "articles",
        &def,
        &crap_cms::db::query::FindQuery::default(),
        None,
    )
    .unwrap();

    let side_effect = docs.iter().find(|d| {
        d.fields.get("title").and_then(|v| v.as_str()) == Some("side-effect-from-after-hook")
    });
    assert!(
        side_effect.is_some(),
        "Side-effect document should have been created by after_change hook"
    );
}

#[test]
fn after_hook_error_rolls_back() {
    use crap_cms::core::collection::Hooks;
    use crap_cms::hooks::lifecycle::{HookContext, HookEvent};

    let (_tmp, pool, registry, runner) = setup_with_db();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    // Build hooks with an after_change hook that errors
    let hooks = Hooks {
        after_change: vec!["hooks.after_crud.error_hook".to_string()],
        ..Default::default()
    };

    // Create a document inside a transaction
    let mut conn = pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = [
        ("title".to_string(), "should-be-rolled-back".to_string()),
        ("status".to_string(), "published".to_string()),
    ]
    .into();
    let doc = crap_cms::db::query::create(&tx, "articles", &def, &data, None).unwrap();
    let doc_id = doc.id.clone();

    // Run after_change hooks -- this should error
    let ctx = HookContext {
        collection: "articles".to_string(),
        operation: "create".to_string(),
        data: doc.fields.clone(),
        locale: None,
        draft: None,
        context: std::collections::HashMap::new(),
        user: None,
        ui_locale: None,
    };
    let result = runner.run_after_write(&hooks, &def.fields, HookEvent::AfterChange, ctx, &tx);
    assert!(result.is_err(), "after_change hook error should propagate");

    // Drop the transaction without committing (simulates rollback)
    drop(tx);

    // Verify the document was NOT persisted (transaction was not committed)
    let conn2 = pool.get().unwrap();
    let found = crap_cms::db::query::find_by_id(&conn2, "articles", &def, &doc_id, None).unwrap();
    assert!(
        found.is_none(),
        "Document should NOT exist after after-hook error (tx rolled back)"
    );
}

#[test]
fn context_flows_to_after_hooks() {
    use crap_cms::core::collection::Hooks;
    use crap_cms::hooks::lifecycle::{HookContext, HookEvent};

    let (_tmp, pool, _registry, runner) = setup_with_db();

    // Build hooks with an after_change hook that reads ctx.context
    let hooks = Hooks {
        after_change: vec!["hooks.after_crud.check_context".to_string()],
        ..Default::default()
    };

    let mut conn = pool.get().unwrap();
    let tx = conn.transaction().unwrap();

    // Simulate a context that was set by before-hooks
    let mut req_context = HashMap::new();
    req_context.insert("before_marker".to_string(), json!("set-by-before-hook"));

    let ctx = HookContext {
        collection: "articles".to_string(),
        operation: "create".to_string(),
        data: HashMap::new(),
        locale: None,
        draft: None,
        context: req_context,
        user: None,
        ui_locale: None,
    };

    let result = runner.run_after_write(&hooks, &[], HookEvent::AfterChange, ctx, &tx);
    assert!(result.is_ok(), "after_change hook should succeed");

    let result_ctx = result.unwrap();
    // The hook reads ctx.context.before_marker and writes it to ctx.data._context_received
    assert_eq!(
        result_ctx
            .data
            .get("_context_received")
            .and_then(|v| v.as_str()),
        Some("set-by-before-hook"),
        "after_change hook should receive the context set by before-hooks"
    );
}
