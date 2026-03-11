use std::path::PathBuf;

use crap_cms::config::CrapConfig;
use crap_cms::core::SharedRegistry;
use crap_cms::db::DbPool;
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

// ── 3A. crap.util Functions ──────────────────────────────────────────────────

#[test]
fn json_encode_table() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local t = { name = "test", count = 42 }
        return crap.util.json_encode(t)
    "#,
    );
    let parsed: serde_json::Value = serde_json::from_str(&result).expect("valid JSON");
    assert_eq!(parsed.get("name").unwrap().as_str().unwrap(), "test");
    assert_eq!(parsed.get("count").unwrap().as_i64().unwrap(), 42);
}

#[test]
fn json_decode_string() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local t = crap.util.json_decode('{"key":"value","num":99}')
        return t.key .. ":" .. tostring(t.num)
    "#,
    );
    assert_eq!(result, "value:99");
}

#[test]
fn json_roundtrip() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local original = { a = 1, b = "hello", c = true }
        local encoded = crap.util.json_encode(original)
        local decoded = crap.util.json_decode(encoded)
        return tostring(decoded.a) .. ":" .. decoded.b .. ":" .. tostring(decoded.c)
    "#,
    );
    assert_eq!(result, "1:hello:true");
}

#[test]
fn json_encode_nested() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local t = { nested = { x = 1, y = 2 }, arr = { 10, 20, 30 } }
        return crap.util.json_encode(t)
    "#,
    );
    let parsed: serde_json::Value = serde_json::from_str(&result).expect("valid JSON");
    let nested = parsed.get("nested").unwrap();
    assert_eq!(nested.get("x").unwrap().as_i64().unwrap(), 1);
    let arr = parsed.get("arr").unwrap().as_array().unwrap();
    assert_eq!(arr.len(), 3);
}

#[test]
fn nanoid_generates_unique_ids() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local id1 = crap.util.nanoid()
        local id2 = crap.util.nanoid()
        if id1 == id2 then return "SAME" end
        return "DIFFERENT"
    "#,
    );
    assert_eq!(result, "DIFFERENT");
}

#[test]
fn nanoid_correct_length() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local id = crap.util.nanoid()
        return tostring(#id)
    "#,
    );
    let len: usize = result.parse().expect("should be a number");
    assert_eq!(len, 21, "Default nanoid length should be 21");
}

// ── 3B. crap.config / crap.env ───────────────────────────────────────────────

#[test]
fn config_get_top_level() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local v = crap.config.get("database.path")
        return tostring(v)
    "#,
    );
    assert_eq!(result, "data/crap.db", "Default database path");
}

#[test]
fn config_get_nested() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local v = crap.config.get("auth.token_expiry")
        return tostring(v)
    "#,
    );
    assert_eq!(result, "7200", "Default token expiry");
}

#[test]
fn config_get_missing_returns_nil() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local v = crap.config.get("nonexistent.deeply.nested.key")
        if v == nil then return "nil" end
        return tostring(v)
    "#,
    );
    assert_eq!(result, "nil");
}

#[test]
fn env_get_existing_var() {
    // Set a test env var
    std::env::set_var("CRAP_TEST_VAR", "hello_from_env");
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local v = crap.env.get("CRAP_TEST_VAR")
        return tostring(v)
    "#,
    );
    assert_eq!(result, "hello_from_env");
    std::env::remove_var("CRAP_TEST_VAR");
}

#[test]
fn env_get_missing_returns_nil() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local v = crap.env.get("NONEXISTENT_CRAP_CMS_TEST_VAR_12345")
        if v == nil then return "nil" end
        return tostring(v)
    "#,
    );
    assert_eq!(result, "nil");
}

// ── 3C. crap.auth in Lua ────────────────────────────────────────────────────

#[test]
fn lua_hash_password() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local h = crap.auth.hash_password("secret")
        if h:sub(1, 7) == "$argon2" then return "ok" end
        return h
    "#,
    );
    assert_eq!(result, "ok", "hash_password should return an argon2 hash");
}

#[test]
fn lua_verify_password_correct() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local h = crap.auth.hash_password("mypassword")
        local ok = crap.auth.verify_password("mypassword", h)
        return tostring(ok)
    "#,
    );
    assert_eq!(result, "true");
}

#[test]
fn lua_verify_password_wrong() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local h = crap.auth.hash_password("mypassword")
        local ok = crap.auth.verify_password("wrongpassword", h)
        return tostring(ok)
    "#,
    );
    assert_eq!(result, "false");
}

// ── crap.util.slugify ────────────────────────────────────────────────────────

#[test]
fn lua_slugify() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        return crap.util.slugify("Hello World! This is a Test")
    "#,
    );
    assert_eq!(result, "hello-world-this-is-a-test");
}

#[test]
fn lua_slugify_special_chars() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        return crap.util.slugify("Über Straße & Café")
    "#,
    );
    // Should handle unicode gracefully
    assert!(!result.is_empty());
    assert!(!result.contains(' '));
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
// ── crap.hooks.remove ────────────────────────────────────────────────────────

#[test]
fn lua_hooks_remove() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        -- Track how many times the hook fires
        local count = 0
        local function my_hook(ctx)
            count = count + 1
            return ctx
        end

        -- Register
        crap.hooks.register("before_change", my_hook)

        -- Verify it's in the table
        local hooks = _crap_event_hooks["before_change"]
        local found = false
        for i = 1, #hooks do
            if rawequal(hooks[i], my_hook) then
                found = true
                break
            end
        end
        if not found then return "NOT_REGISTERED" end

        -- Remove
        crap.hooks.remove("before_change", my_hook)

        -- Verify it's gone
        local still_found = false
        hooks = _crap_event_hooks["before_change"]
        for i = 1, #hooks do
            if rawequal(hooks[i], my_hook) then
                still_found = true
                break
            end
        end
        if still_found then return "NOT_REMOVED" end

        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

// ── crap.locale functions ────────────────────────────────────────────────────

#[test]
fn lua_locale_get_default() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        return crap.locale.get_default()
    "#,
    );
    assert_eq!(result, "en", "Default locale should be 'en'");
}

#[test]
fn lua_locale_get_all() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local all = crap.locale.get_all()
        return tostring(#all)
    "#,
    );
    assert_eq!(result, "0", "No locales configured by default");
}

#[test]
fn lua_locale_is_enabled() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        return tostring(crap.locale.is_enabled())
    "#,
    );
    assert_eq!(result, "false", "Locale should not be enabled by default");
}

// ── crap.http.request ────────────────────────────────────────────────────────

#[test]
fn lua_http_request_invalid_url() {
    let runner = setup_lua();
    // Connecting to localhost:1 should fail with a transport error.
    // eval_lua uses .expect("eval failed"), so we need to test the error path manually.
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    let pool = crap_cms::db::pool::create_pool(tmp.path(), &config).expect("pool");
    let conn = pool.get().expect("conn");
    let result = runner.eval_lua_with_conn(
        r#"
            local ok, err = pcall(function()
                crap.http.request({ url = "http://localhost:1", method = "GET", timeout = 1 })
            end)
            if ok then return "SHOULD_HAVE_FAILED" end
            -- err should contain some transport/connection error message
            local err_str = tostring(err)
            if err_str:find("transport") or err_str:find("Connection") or err_str:find("connection") then
                return "ok"
            end
            return "UNEXPECTED_ERROR:" .. err_str
        "#,
        &conn,
        None,
    ).expect("eval failed");
    assert_eq!(
        result, "ok",
        "HTTP request to invalid port should produce a transport error"
    );
}

// ── crap.email.send (no-op when not configured) ─────────────────────────────

#[test]
fn lua_email_not_configured() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local ok = crap.email.send({
            to = "test@example.com",
            subject = "Test Subject",
            html = "<p>Hello</p>",
        })
        return tostring(ok)
    "#,
    );
    assert_eq!(
        result, "true",
        "Email send should return true (no-op) when SMTP not configured"
    );
}

// ── 4A. crap.http.request edge cases ─────────────────────────────────────────

#[test]
fn http_request_unsupported_method() {
    let runner = setup_lua();
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    let pool = crap_cms::db::pool::create_pool(tmp.path(), &config).expect("pool");
    let conn = pool.get().expect("conn");
    let result = runner.eval_lua_with_conn(
        r#"
            local ok, err = pcall(function()
                crap.http.request({ url = "http://localhost:1", method = "OPTIONS" })
            end)
            if ok then return "SHOULD_HAVE_FAILED" end
            local err_str = tostring(err)
            if err_str:find("method") or err_str:find("unsupported") or err_str:find("OPTIONS") then
                return "ok"
            end
            -- Some implementations might still try to make the request, which will get a transport error
            if err_str:find("transport") or err_str:find("connection") or err_str:find("Connection") then
                return "ok"
            end
            return "UNEXPECTED_ERROR:" .. err_str
        "#,
        &conn,
        None,
    ).expect("eval failed");
    assert_eq!(result, "ok");
}

#[test]
fn http_request_missing_url() {
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
                crap.http.request({ method = "GET" })
            end)
            if ok then return "SHOULD_HAVE_FAILED" end
            -- Missing url causes a type conversion error (nil to String)
            return "ok"
        "#,
            &conn,
            None,
        )
        .expect("eval failed");
    assert_eq!(result, "ok");
}

#[test]
fn http_request_post_with_body() {
    let runner = setup_lua();
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    let pool = crap_cms::db::pool::create_pool(tmp.path(), &config).expect("pool");
    let conn = pool.get().expect("conn");
    // POST to localhost:1 with a body — should fail with transport error
    // but verifies the POST + body code path doesn't crash
    let result = runner
        .eval_lua_with_conn(
            r#"
            local ok, err = pcall(function()
                crap.http.request({
                    url = "http://localhost:1",
                    method = "POST",
                    body = '{"key":"value"}',
                    headers = { ["Content-Type"] = "application/json" },
                    timeout = 1,
                })
            end)
            if ok then return "SHOULD_HAVE_FAILED" end
            -- Transport/connection error expected
            return "ok"
        "#,
            &conn,
            None,
        )
        .expect("eval failed");
    assert_eq!(result, "ok");
}

// ── 4C. crap.email.send with text + html ─────────────────────────────────────

#[test]
fn email_send_with_text_and_html() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local ok = crap.email.send({
            to = "test@example.com",
            subject = "Test",
            html = "<p>HTML body</p>",
            text = "Plain text body",
        })
        return tostring(ok)
    "#,
    );
    assert_eq!(
        result, "true",
        "Email with both text and html should return true (no-op)"
    );
}
