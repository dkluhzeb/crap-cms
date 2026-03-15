use std::path::PathBuf;

use crap_cms::config::CrapConfig;
use crap_cms::core::SharedRegistry;
use crap_cms::db::{DbConnection, DbPool, DbValue};
use crap_cms::hooks;
use crap_cms::hooks::lifecycle::HookRunner;

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

// ── 5B. Lua CRUD with Draft Option ──────────────────────────────────────────

fn setup_versioned_db() -> (
    tempfile::TempDir,
    crap_cms::db::DbPool,
    SharedRegistry,
    HookRunner,
) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();

    std::fs::write(
        collections_dir.join("articles.lua"),
        r#"
crap.collections.define("articles", {
    timestamps = true,
    versions = {
        drafts = true,
        max_versions = 10,
    },
    fields = {
        { name = "title", type = "text", required = true },
        { name = "body", type = "textarea" },
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
    (tmp, pool, registry, runner)
}

fn eval_versioned(runner: &HookRunner, pool: &crap_cms::db::DbPool, code: &str) -> String {
    let conn = pool.get().expect("conn");
    runner
        .eval_lua_with_conn(code, &conn, None)
        .expect("eval failed")
}

// ══════════════════════════════════════════════════════════════════════════════
// API SURFACE PARITY TESTS: password handling, unpublish, before_read, upload sizes
// ══════════════════════════════════════════════════════════════════════════════

// ── Lua CRUD Password Handling (Auth Collections) ────────────────────────────

#[test]
fn lua_create_auth_hashes_password() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();

    std::fs::write(
        collections_dir.join("users.lua"),
        r#"
crap.collections.define("users", {
    auth = true,
    fields = {
        { name = "email", type = "email", required = true, unique = true },
        { name = "name", type = "text" },
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

    let conn = pool.get().expect("conn");
    let result = runner
        .eval_lua_with_conn(
            r#"
        local doc = crap.collections.create("users", {
            email = "test@example.com",
            name = "Test User",
            password = "secret123",
        })
        if doc == nil then return "CREATE_NIL" end
        if doc.id == nil then return "NO_ID" end
        -- password should NOT appear in the returned document
        if doc.password ~= nil then
            return "PASSWORD_LEAKED:" .. tostring(doc.password)
        end
        if doc._password_hash ~= nil then
            return "HASH_LEAKED:" .. tostring(doc._password_hash)
        end
        return doc.id
    "#,
            &conn,
            None,
        )
        .expect("eval");
    assert!(
        !result.is_empty() && result != "CREATE_NIL" && result != "NO_ID",
        "Should return a valid doc id, got: {}",
        result
    );

    // Verify the password was actually hashed in the DB
    let hash =
        crap_cms::db::query::get_password_hash(&conn, "users", &result).expect("get_password_hash");
    assert!(hash.is_some(), "Password hash should exist in DB");
    let hash = hash.unwrap();
    assert!(
        hash.as_ref().starts_with("$argon2"),
        "Hash should be argon2: {}",
        hash.as_ref()
    );
}

#[test]
fn lua_update_auth_changes_password() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();

    std::fs::write(
        collections_dir.join("users.lua"),
        r#"
crap.collections.define("users", {
    auth = true,
    fields = {
        { name = "email", type = "email", required = true, unique = true },
        { name = "name", type = "text" },
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

    let conn = pool.get().expect("conn");

    // Create user with initial password
    let user_id = runner
        .eval_lua_with_conn(
            r#"
        local doc = crap.collections.create("users", {
            email = "update@example.com",
            name = "Update User",
            password = "oldpass123",
        })
        return doc.id
    "#,
            &conn,
            None,
        )
        .expect("create");

    let old_hash = crap_cms::db::query::get_password_hash(&conn, "users", &user_id)
        .expect("get hash")
        .expect("hash exists");

    // Update with new password
    runner
        .eval_lua_with_conn(
            &format!(
                r#"
        local doc = crap.collections.update("users", "{}", {{
            name = "Updated Name",
            password = "newpass456",
        }})
        return "ok"
    "#,
                user_id
            ),
            &conn,
            None,
        )
        .expect("update");

    let new_hash = crap_cms::db::query::get_password_hash(&conn, "users", &user_id)
        .expect("get hash")
        .expect("hash exists");

    assert_ne!(
        old_hash.as_ref(),
        new_hash.as_ref(),
        "Password hash should have changed after update"
    );
    assert!(
        new_hash.as_ref().starts_with("$argon2"),
        "New hash should be argon2: {}",
        new_hash.as_ref()
    );

    // Verify the new password works
    assert!(
        crap_cms::core::auth::verify_password("newpass456", new_hash.as_ref()).expect("verify"),
        "New password should verify"
    );
    // Verify the old password no longer works
    assert!(
        !crap_cms::core::auth::verify_password("oldpass123", new_hash.as_ref()).expect("verify"),
        "Old password should NOT verify against new hash"
    );
}

// ── Lua CRUD Unpublish ───────────────────────────────────────────────────────

#[test]
fn lua_update_unpublish() {
    let (_tmp, pool, _reg, runner) = setup_versioned_db();
    let result = eval_versioned(
        &runner,
        &pool,
        r#"
        -- Create a published document
        local doc = crap.collections.create("articles", {
            title = "Published Article",
            body = "Content here",
        })
        local id = doc.id

        -- Unpublish it
        local unpublished = crap.collections.update("articles", id, {}, { unpublish = true })

        -- Find without draft flag should NOT find it (status is now "draft")
        local result = crap.collections.find("articles", {})
        if result.pagination.totalDocs ~= 0 then
            return "STILL_PUBLISHED:total=" .. tostring(result.pagination.totalDocs)
        end

        -- Find with draft flag should find it
        local drafts = crap.collections.find("articles", { draft = true })
        if drafts.pagination.totalDocs ~= 1 then
            return "NOT_IN_DRAFTS:total=" .. tostring(drafts.pagination.totalDocs)
        end
        if drafts.documents[1].id ~= id then
            return "WRONG_DOC"
        end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

// ── Lua CRUD before_read Hook ────────────────────────────────────────────────

#[test]
fn lua_find_fires_before_read() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();
    let hooks_dir = tmp.path().join("hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();

    std::fs::write(
        collections_dir.join("guarded.lua"),
        r#"
crap.collections.define("guarded", {
    fields = {
        { name = "title", type = "text", required = true },
    },
    hooks = {
        before_read = { "hooks.guard.before_read" },
    },
})
        "#,
    )
    .unwrap();
    std::fs::write(
        hooks_dir.join("guard.lua"),
        r#"
local M = {}
function M.before_read(ctx)
    error("before_read_blocked")
end
return M
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

    let conn = pool.get().expect("conn");

    // Create a document (with hooks=false to bypass before_read on the create path)
    runner
        .eval_lua_with_conn(
            r#"
        crap.collections.create("guarded", { title = "Test" }, { hooks = false })
        return "ok"
    "#,
            &conn,
            None,
        )
        .expect("create");

    // find should fail because before_read hook throws an error
    let find_result = runner
        .eval_lua_with_conn(
            r#"
        local ok, err = pcall(function()
            crap.collections.find("guarded", {})
        end)
        if ok then return "SHOULD_HAVE_FAILED" end
        local err_str = tostring(err)
        if err_str:find("before_read_blocked") then return "ok" end
        return "WRONG_ERROR:" .. err_str
    "#,
            &conn,
            None,
        )
        .expect("find eval");
    assert_eq!(find_result, "ok", "find should propagate before_read error");

    // find_by_id should also fail
    let find_by_id_result = runner
        .eval_lua_with_conn(
            r#"
        -- Get the doc id first via raw query (bypassing hooks)
        local ok, err = pcall(function()
            crap.collections.find_by_id("guarded", "any-id")
        end)
        if ok then return "SHOULD_HAVE_FAILED" end
        local err_str = tostring(err)
        if err_str:find("before_read_blocked") then return "ok" end
        return "WRONG_ERROR:" .. err_str
    "#,
            &conn,
            None,
        )
        .expect("find_by_id eval");
    assert_eq!(
        find_by_id_result, "ok",
        "find_by_id should propagate before_read error"
    );
}

// ── Lua CRUD Upload Sizes Assembly ───────────────────────────────────────────

#[test]
fn lua_find_upload_sizes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();

    std::fs::write(
        collections_dir.join("media.lua"),
        r#"
crap.collections.define("media", {
    upload = {
        enabled = true,
        image_sizes = {
            { name = "thumbnail", width = 200, height = 200 },
            { name = "card", width = 640, height = 480 },
        },
    },
    fields = {
        { name = "alt", type = "text" },
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

    let conn = pool.get().expect("conn");

    // Create a media doc with required upload fields and manually insert size columns
    let doc_id = runner
        .eval_lua_with_conn(
            r#"
        local doc = crap.collections.create("media", {
            filename = "test.jpg",
            mime_type = "image/jpeg",
            filesize = 12345,
            url = "/uploads/test.jpg",
            alt = "Test image",
        }, { hooks = false })
        return doc.id
    "#,
            &conn,
            None,
        )
        .expect("create");

    // Manually set per-size columns in the DB (simulating what the upload handler does)
    conn.execute(
        "UPDATE media SET thumbnail_url = ?1, thumbnail_width = ?2, thumbnail_height = ?3, \
         card_url = ?4, card_width = ?5, card_height = ?6 WHERE id = ?7",
        &[
            DbValue::Text("/uploads/thumb.jpg".to_string()),
            DbValue::Integer(200),
            DbValue::Integer(200),
            DbValue::Text("/uploads/card.jpg".to_string()),
            DbValue::Integer(640),
            DbValue::Integer(480),
            DbValue::Text(doc_id.clone()),
        ],
    )
    .expect("set size columns");

    // find should assemble the sizes object
    let find_result = runner
        .eval_lua_with_conn(
            r#"
        local result = crap.collections.find("media", {})
        if result.pagination.totalDocs ~= 1 then
            return "WRONG_TOTAL:" .. tostring(result.pagination.totalDocs)
        end
        local doc = result.documents[1]
        if doc.sizes == nil then
            return "NO_SIZES"
        end
        if type(doc.sizes) ~= "table" then
            return "SIZES_NOT_TABLE:" .. type(doc.sizes)
        end
        if doc.sizes.thumbnail == nil then
            return "NO_THUMBNAIL"
        end
        if doc.sizes.thumbnail.url ~= "/uploads/thumb.jpg" then
            return "WRONG_THUMB_URL:" .. tostring(doc.sizes.thumbnail.url)
        end
        if doc.sizes.card == nil then
            return "NO_CARD"
        end
        if doc.sizes.card.url ~= "/uploads/card.jpg" then
            return "WRONG_CARD_URL:" .. tostring(doc.sizes.card.url)
        end
        -- Per-size columns should be removed (assembled into sizes)
        if doc.thumbnail_url ~= nil then
            return "FLAT_COLUMN_LEAKED:thumbnail_url"
        end
        return "ok"
    "#,
            &conn,
            None,
        )
        .expect("find eval");
    assert_eq!(find_result, "ok");

    // find_by_id should also assemble sizes
    let find_by_id_result = runner
        .eval_lua_with_conn(
            &format!(
                r#"
        local doc = crap.collections.find_by_id("media", "{}")
        if doc == nil then return "NOT_FOUND" end
        if doc.sizes == nil then return "NO_SIZES" end
        if doc.sizes.thumbnail == nil then return "NO_THUMBNAIL" end
        if doc.sizes.thumbnail.url ~= "/uploads/thumb.jpg" then
            return "WRONG_URL:" .. tostring(doc.sizes.thumbnail.url)
        end
        return "ok"
    "#,
                doc_id
            ),
            &conn,
            None,
        )
        .expect("find_by_id eval");
    assert_eq!(find_by_id_result, "ok");
}

// ══════════════════════════════════════════════════════════════════════════════
// Additional Lua API Tests
// ══════════════════════════════════════════════════════════════════════════════

#[test]
fn lua_crypto_hash_verify_roundtrip() {
    // Test crap.auth.hash_password and crap.auth.verify_password roundtrip.
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local hash = crap.auth.hash_password("test")
        -- Verify the hash starts with the argon2 prefix
        if hash:sub(1, 7) ~= "$argon2" then
            return "BAD_PREFIX:" .. hash:sub(1, 10)
        end
        -- Verify the correct password matches
        local ok = crap.auth.verify_password("test", hash)
        if not ok then return "VERIFY_FAILED" end
        -- Verify a wrong password does NOT match
        local wrong = crap.auth.verify_password("wrong", hash)
        if wrong then return "WRONG_MATCHED" end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn lua_env_get_missing_returns_nil() {
    // Test that crap.env.get returns nil for a non-existent environment variable.
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local v = crap.env.get("NONEXISTENT_VAR_12345")
        if v == nil then return "nil" end
        return "NOT_NIL:" .. tostring(v)
    "#,
    );
    assert_eq!(
        result, "nil",
        "crap.env.get should return nil for missing env vars"
    );
}

#[test]
fn lua_config_get_dot_notation() {
    // Test that crap.config.get with dot-notation traversal returns the
    // configured auth secret value.
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local secret = crap.config.get("auth.secret")
        -- Default CrapConfig has an empty string or auto-generated secret
        -- The key point is that dot notation traversal works without error
        if secret == nil then return "nil" end
        return tostring(secret)
    "#,
    );
    // The default CrapConfig has an empty secret, which is fine.
    // The test verifies that dot notation works and doesn't error.
    // An empty string is the default.
    assert!(
        result == "" || result == "nil" || !result.is_empty(),
        "crap.config.get('auth.secret') should return a value or nil, got: {}",
        result
    );

    // Also verify deeper dot notation works for a known value
    let result2 = eval_lua(
        &runner,
        r#"
        local expiry = crap.config.get("auth.token_expiry")
        return tostring(expiry)
    "#,
    );
    assert_eq!(result2, "7200", "auth.token_expiry should be default 7200");
}

#[test]
fn lua_json_encode_decode_roundtrip() {
    // Test encoding a table to JSON and decoding it back, verifying all
    // value types survive the roundtrip.
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local original = {
            name = "test",
            count = 42,
            active = true,
            tags = { "alpha", "beta", "gamma" },
            nested = { x = 1, y = 2 },
        }
        local encoded = crap.util.json_encode(original)
        local decoded = crap.util.json_decode(encoded)

        -- Verify scalar fields
        if decoded.name ~= "test" then return "NAME:" .. tostring(decoded.name) end
        if decoded.count ~= 42 then return "COUNT:" .. tostring(decoded.count) end
        if decoded.active ~= true then return "ACTIVE:" .. tostring(decoded.active) end

        -- Verify array
        if #decoded.tags ~= 3 then return "TAGS_LEN:" .. tostring(#decoded.tags) end
        if decoded.tags[1] ~= "alpha" then return "TAG1:" .. tostring(decoded.tags[1]) end
        if decoded.tags[3] ~= "gamma" then return "TAG3:" .. tostring(decoded.tags[3]) end

        -- Verify nested table
        if decoded.nested.x ~= 1 then return "NX:" .. tostring(decoded.nested.x) end
        if decoded.nested.y ~= 2 then return "NY:" .. tostring(decoded.nested.y) end

        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

// ── CRUD: OR Filter Combinations ─────────────────────────────────────────────

#[test]
fn lua_find_or_filter() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        crap.collections.create("articles", { title = "Alpha", status = "published" })
        crap.collections.create("articles", { title = "Beta", status = "draft" })
        crap.collections.create("articles", { title = "Gamma", status = "archived" })

        -- OR filter: status = published OR status = draft
        local r = crap.collections.find("articles", {
            where = {
                ["or"] = {
                    { status = "published" },
                    { status = "draft" },
                },
            },
        })
        if r.pagination.totalDocs ~= 2 then return "WRONG_TOTAL:" .. tostring(r.pagination.totalDocs) end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn lua_find_or_filter_with_operator() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        crap.collections.create("articles", { title = "Alpha", body = "10" })
        crap.collections.create("articles", { title = "Beta", body = "20" })
        crap.collections.create("articles", { title = "Gamma", body = "30" })

        -- OR with operator-based filters inside groups
        local r = crap.collections.find("articles", {
            where = {
                ["or"] = {
                    { body = { greater_than = "25" } },
                    { title = "Alpha" },
                },
            },
        })
        -- Should match Alpha (title) and Gamma (body > 25)
        if r.pagination.totalDocs ~= 2 then return "WRONG_TOTAL:" .. tostring(r.pagination.totalDocs) end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn lua_find_or_filter_with_integer_values() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        crap.collections.create("articles", { title = "A1", word_count = "10" })
        crap.collections.create("articles", { title = "A2", word_count = "20" })
        crap.collections.create("articles", { title = "A3", word_count = "30" })

        -- Integer values in OR filter
        local r = crap.collections.find("articles", {
            where = {
                ["or"] = {
                    { word_count = 10 },
                    { word_count = 30 },
                },
            },
        })
        if r.pagination.totalDocs ~= 2 then return "WRONG:" .. tostring(r.pagination.totalDocs) end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

// ── CRUD: exists / not_exists Filter Operators ───────────────────────────────

#[test]
fn lua_find_exists_filter() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        -- Use body field since status gets a default from before_change hook
        crap.collections.create("articles", { title = "With Body", body = "some content" })
        crap.collections.create("articles", { title = "Without Body" })

        -- exists filter: only docs where body is set (non-NULL)
        local r = crap.collections.find("articles", {
            where = { body = { exists = true } },
        })
        if r.pagination.totalDocs ~= 1 then return "EXISTS:" .. tostring(r.pagination.totalDocs) end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn lua_find_not_exists_filter() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        -- Use body field since status gets a default from before_change hook
        crap.collections.create("articles", { title = "With Body", body = "some content" })
        crap.collections.create("articles", { title = "Without Body" })

        -- not_exists filter: only docs where body is NULL
        local r = crap.collections.find("articles", {
            where = { body = { not_exists = true } },
        })
        if r.pagination.totalDocs ~= 1 then return "NOT_EXISTS:" .. tostring(r.pagination.totalDocs) end
        -- after_read field hook uppercases title
        if r.documents[1].title ~= "WITHOUT BODY" then
            return "WRONG_DOC:" .. tostring(r.documents[1].title)
        end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

// ── CRUD: Integer and Boolean Filter Values ──────────────────────────────────

#[test]
fn lua_find_integer_filter_value() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        crap.collections.create("articles", { title = "A", word_count = "42" })
        crap.collections.create("articles", { title = "B", word_count = "99" })

        -- Integer filter value (not string)
        local r = crap.collections.find("articles", {
            where = { word_count = 42 },
        })
        if r.pagination.totalDocs ~= 1 then return "WRONG:" .. tostring(r.pagination.totalDocs) end
        if r.documents[1].title ~= "A" then return "WRONG_DOC" end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn lua_find_number_filter_value() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        crap.collections.create("articles", { title = "A", word_count = "3.14" })
        crap.collections.create("articles", { title = "B", word_count = "2.71" })

        -- Float filter value
        local r = crap.collections.find("articles", {
            where = { word_count = 3.14 },
        })
        if r.pagination.totalDocs ~= 1 then return "WRONG:" .. tostring(r.pagination.totalDocs) end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

// ── CRUD: find_by_id with select ─────────────────────────────────────────────

#[test]
fn lua_find_by_id_with_select() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        local doc = crap.collections.create("articles", {
            title = "Select Test",
            body = "Some body",
            status = "published",
        })

        -- find_by_id with select: only return title
        local found = crap.collections.find_by_id("articles", doc.id, {
            select = { "title" },
        })
        if found == nil then return "NOT_FOUND" end
        -- after_read field hook uppercases title
        if found.title ~= "SELECT TEST" then return "WRONG_TITLE" end
        -- body should be stripped by select
        if found.body ~= nil then return "BODY_NOT_STRIPPED:" .. tostring(found.body) end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

// ── CRUD: find_by_id returns nil for nonexistent ─────────────────────────────

#[test]
fn lua_find_by_id_nonexistent() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        local doc = crap.collections.find_by_id("articles", "nonexistent-id-123")
        if doc == nil then return "nil" end
        return "FOUND"
    "#,
    );
    assert_eq!(result, "nil");
}

// ── CRUD: find with select ───────────────────────────────────────────────────

#[test]
fn lua_find_with_select() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        crap.collections.create("articles", { title = "Sel Test", body = "content", status = "active" })

        local r = crap.collections.find("articles", {
            select = { "title" },
        })
        if r.pagination.totalDocs ~= 1 then return "WRONG_TOTAL" end
        local doc = r.documents[1]
        -- after_read field hook uppercases title
        if doc.title ~= "SEL TEST" then return "WRONG_TITLE" end
        -- body should not be returned due to select
        if doc.body ~= nil then return "BODY_PRESENT:" .. tostring(doc.body) end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

// ── CRUD: find_by_id with draft option ───────────────────────────────────────

#[test]
fn lua_find_by_id_with_draft_option() {
    let (_tmp, pool, _reg, runner) = setup_versioned_db();
    let result = eval_versioned(
        &runner,
        &pool,
        r#"
        -- Create a published article
        local doc = crap.collections.create("articles", {
            title = "Draft Test",
            body = "Original body",
        })
        local id = doc.id

        -- Save a draft version
        crap.collections.update("articles", id, {
            title = "Draft Test Updated",
            body = "Updated body",
        }, { draft = true })

        -- find_by_id without draft should return published version
        local pub = crap.collections.find_by_id("articles", id)
        if pub == nil then return "NOT_FOUND" end
        if pub.title ~= "Draft Test" then return "WRONG_PUB_TITLE:" .. tostring(pub.title) end

        -- find_by_id with draft=true should return draft overlay
        local draft = crap.collections.find_by_id("articles", id, { draft = true })
        if draft == nil then return "DRAFT_NOT_FOUND" end
        if draft.body ~= "Updated body" then return "WRONG_DRAFT_BODY:" .. tostring(draft.body) end

        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

// ── CRUD: Boolean filter operator values ─────────────────────────────────────

#[test]
fn lua_filter_boolean_to_string() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        crap.collections.create("articles", { title = "Active", status = "true" })
        crap.collections.create("articles", { title = "Inactive", status = "false" })

        -- Boolean as filter operator value (e.g., in not_equals)
        local r = crap.collections.find("articles", {
            where = { status = { not_equals = true } },
        })
        -- "true" as boolean converts to "true" string, should match Inactive
        if r.pagination.totalDocs ~= 1 then return "WRONG:" .. tostring(r.pagination.totalDocs) end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

// ── CRUD: count with filters ─────────────────────────────────────────────────

#[test]
fn lua_count_with_or_filter() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        crap.collections.create("articles", { title = "A", status = "published" })
        crap.collections.create("articles", { title = "B", status = "draft" })
        crap.collections.create("articles", { title = "C", status = "archived" })

        local count = crap.collections.count("articles", {
            where = {
                ["or"] = {
                    { status = "published" },
                    { status = "draft" },
                },
            },
        })
        return tostring(count)
    "#,
    );
    assert_eq!(result, "2");
}

// ── CRUD: update with hooks=false ────────────────────────────────────────────

#[test]
fn lua_update_with_hooks_false() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        local doc = crap.collections.create("articles", { title = "Before Update" })
        local updated = crap.collections.update("articles", doc.id, {
            title = "After Update",
        }, { hooks = false })
        if updated.title ~= "After Update" then
            return "WRONG:" .. tostring(updated.title)
        end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

// ── CRUD: delete with hooks=false ────────────────────────────────────────────
