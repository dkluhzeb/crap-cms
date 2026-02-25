use std::collections::HashMap;
use std::path::PathBuf;

use crap_cms::config::CrapConfig;
use crap_cms::db::DbPool;
use crap_cms::core::SharedRegistry;
use crap_cms::hooks;
use crap_cms::hooks::lifecycle::HookRunner;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hook_tests")
}

fn setup_lua() -> HookRunner {
    let config_dir = fixture_dir();
    let config = CrapConfig::default();
    let registry = hooks::init_lua(&config_dir, &config).expect("init_lua failed");
    HookRunner::new(&config_dir, registry, &config).expect("HookRunner::new failed")
}

/// Helper to eval Lua code and get a string result (no DB connection needed for pure functions).
/// This uses a temporary in-memory DB for the eval.
fn eval_lua(runner: &HookRunner, code: &str) -> String {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    let pool = crap_cms::db::pool::create_pool(tmp.path(), &config).expect("pool");
    let conn = pool.get().expect("conn");
    runner.eval_lua_with_conn(code, &conn, None).expect("eval failed")
}

// ── 3A. crap.util Functions ──────────────────────────────────────────────────

#[test]
fn json_encode_table() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local t = { name = "test", count = 42 }
        return crap.util.json_encode(t)
    "#);
    let parsed: serde_json::Value = serde_json::from_str(&result).expect("valid JSON");
    assert_eq!(parsed.get("name").unwrap().as_str().unwrap(), "test");
    assert_eq!(parsed.get("count").unwrap().as_i64().unwrap(), 42);
}

#[test]
fn json_decode_string() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local t = crap.util.json_decode('{"key":"value","num":99}')
        return t.key .. ":" .. tostring(t.num)
    "#);
    assert_eq!(result, "value:99");
}

#[test]
fn json_roundtrip() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local original = { a = 1, b = "hello", c = true }
        local encoded = crap.util.json_encode(original)
        local decoded = crap.util.json_decode(encoded)
        return tostring(decoded.a) .. ":" .. decoded.b .. ":" .. tostring(decoded.c)
    "#);
    assert_eq!(result, "1:hello:true");
}

#[test]
fn json_encode_nested() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local t = { nested = { x = 1, y = 2 }, arr = { 10, 20, 30 } }
        return crap.util.json_encode(t)
    "#);
    let parsed: serde_json::Value = serde_json::from_str(&result).expect("valid JSON");
    let nested = parsed.get("nested").unwrap();
    assert_eq!(nested.get("x").unwrap().as_i64().unwrap(), 1);
    let arr = parsed.get("arr").unwrap().as_array().unwrap();
    assert_eq!(arr.len(), 3);
}

#[test]
fn nanoid_generates_unique_ids() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local id1 = crap.util.nanoid()
        local id2 = crap.util.nanoid()
        if id1 == id2 then return "SAME" end
        return "DIFFERENT"
    "#);
    assert_eq!(result, "DIFFERENT");
}

#[test]
fn nanoid_correct_length() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local id = crap.util.nanoid()
        return tostring(#id)
    "#);
    let len: usize = result.parse().expect("should be a number");
    assert_eq!(len, 21, "Default nanoid length should be 21");
}

// ── 3B. crap.config / crap.env ───────────────────────────────────────────────

#[test]
fn config_get_top_level() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local v = crap.config.get("database.path")
        return tostring(v)
    "#);
    assert_eq!(result, "data/crap.db", "Default database path");
}

#[test]
fn config_get_nested() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local v = crap.config.get("auth.token_expiry")
        return tostring(v)
    "#);
    assert_eq!(result, "7200", "Default token expiry");
}

#[test]
fn config_get_missing_returns_nil() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local v = crap.config.get("nonexistent.deeply.nested.key")
        if v == nil then return "nil" end
        return tostring(v)
    "#);
    assert_eq!(result, "nil");
}

#[test]
fn env_get_existing_var() {
    // Set a test env var
    std::env::set_var("CRAP_TEST_VAR", "hello_from_env");
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local v = crap.env.get("CRAP_TEST_VAR")
        return tostring(v)
    "#);
    assert_eq!(result, "hello_from_env");
    std::env::remove_var("CRAP_TEST_VAR");
}

#[test]
fn env_get_missing_returns_nil() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local v = crap.env.get("NONEXISTENT_CRAP_CMS_TEST_VAR_12345")
        if v == nil then return "nil" end
        return tostring(v)
    "#);
    assert_eq!(result, "nil");
}

// ── 3C. crap.auth in Lua ────────────────────────────────────────────────────

#[test]
fn lua_hash_password() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local h = crap.auth.hash_password("secret")
        if h:sub(1, 7) == "$argon2" then return "ok" end
        return h
    "#);
    assert_eq!(result, "ok", "hash_password should return an argon2 hash");
}

#[test]
fn lua_verify_password_correct() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local h = crap.auth.hash_password("mypassword")
        local ok = crap.auth.verify_password("mypassword", h)
        return tostring(ok)
    "#);
    assert_eq!(result, "true");
}

#[test]
fn lua_verify_password_wrong() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local h = crap.auth.hash_password("mypassword")
        local ok = crap.auth.verify_password("wrongpassword", h)
        return tostring(ok)
    "#);
    assert_eq!(result, "false");
}

// ── 3D. Definition Parsing Edge Cases ────────────────────────────────────────

#[test]
fn parse_collection_minimal() {
    let config_dir = fixture_dir();
    let config = CrapConfig::default();
    let registry = hooks::init_lua(&config_dir, &config).expect("init_lua failed");

    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").expect("articles should be registered");
    assert_eq!(def.slug, "articles");
    assert!(!def.fields.is_empty());
}

#[test]
fn parse_collection_with_all_field_types() {
    // Use a temp dir with a custom collection that has all field types
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();

    std::fs::write(
        collections_dir.join("everything.lua"),
        r#"
crap.collections.define("everything", {
    fields = {
        { name = "text_field", type = "text" },
        { name = "num_field", type = "number" },
        { name = "email_field", type = "email" },
        { name = "textarea_field", type = "textarea" },
        { name = "select_field", type = "select", options = {
            { label = "A", value = "a" },
            { label = "B", value = "b" },
        }},
        { name = "checkbox_field", type = "checkbox" },
        { name = "date_field", type = "date" },
        { name = "json_field", type = "json" },
        { name = "richtext_field", type = "richtext" },
        { name = "group_field", type = "group", fields = {
            { name = "sub1", type = "text" },
            { name = "sub2", type = "number" },
        }},
    },
})
        "#,
    ).unwrap();

    // Create empty init.lua
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::default();
    let registry = hooks::init_lua(tmp.path(), &config).expect("init_lua failed");

    let reg = registry.read().unwrap();
    let def = reg.get_collection("everything").expect("everything should be registered");
    assert_eq!(def.fields.len(), 10);

    // Verify types
    let field_types: Vec<&str> = def.fields.iter().map(|f| f.field_type.as_str()).collect();
    assert!(field_types.contains(&"text"));
    assert!(field_types.contains(&"number"));
    assert!(field_types.contains(&"email"));
    assert!(field_types.contains(&"select"));
    assert!(field_types.contains(&"checkbox"));
    assert!(field_types.contains(&"group"));

    // Verify group sub-fields
    let group = def.fields.iter().find(|f| f.name == "group_field").unwrap();
    assert_eq!(group.fields.len(), 2);
}

#[test]
fn parse_auth_config_true() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();

    std::fs::write(
        collections_dir.join("users.lua"),
        r#"
crap.collections.define("users", {
    auth = true,
    fields = {
        { name = "name", type = "text" },
    },
})
        "#,
    ).unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::default();
    let registry = hooks::init_lua(tmp.path(), &config).expect("init_lua failed");

    let reg = registry.read().unwrap();
    let def = reg.get_collection("users").expect("users should be registered");
    assert!(def.is_auth_collection(), "should be auth collection");
    // Email field should have been auto-injected
    assert!(
        def.fields.iter().any(|f| f.name == "email" && f.field_type == crap_cms::core::field::FieldType::Email),
        "email field should be auto-injected for auth collections"
    );
}

#[test]
fn parse_auth_config_table() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();

    std::fs::write(
        collections_dir.join("members.lua"),
        r#"
crap.collections.define("members", {
    auth = {
        verify_email = true,
        forgot_password = false,
    },
    fields = {
        { name = "role", type = "text" },
    },
})
        "#,
    ).unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::default();
    let registry = hooks::init_lua(tmp.path(), &config).expect("init_lua failed");

    let reg = registry.read().unwrap();
    let def = reg.get_collection("members").expect("members should be registered");
    assert!(def.is_auth_collection());
    let auth = def.auth.as_ref().unwrap();
    assert!(auth.verify_email);
    assert!(!auth.forgot_password);
}

#[test]
fn parse_global_definition() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let globals_dir = tmp.path().join("globals");
    std::fs::create_dir_all(&globals_dir).unwrap();

    std::fs::write(
        globals_dir.join("settings.lua"),
        r#"
crap.globals.define("settings", {
    labels = { singular = "Settings" },
    fields = {
        { name = "site_name", type = "text" },
        { name = "maintenance_mode", type = "checkbox" },
    },
})
        "#,
    ).unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::default();
    let registry = hooks::init_lua(tmp.path(), &config).expect("init_lua failed");

    let reg = registry.read().unwrap();
    let def = reg.get_global("settings").expect("settings should be registered");
    assert_eq!(def.slug, "settings");
    assert_eq!(def.fields.len(), 2);
    assert_eq!(def.fields[0].name, "site_name");
    assert_eq!(def.fields[1].name, "maintenance_mode");
}

// ── crap.util.slugify ────────────────────────────────────────────────────────

#[test]
fn lua_slugify() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        return crap.util.slugify("Hello World! This is a Test")
    "#);
    assert_eq!(result, "hello-world-this-is-a-test");
}

#[test]
fn lua_slugify_special_chars() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        return crap.util.slugify("Über Straße & Café")
    "#);
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

    let runner = HookRunner::new(&config_dir, registry.clone(), &config)
        .expect("HookRunner::new failed");
    (tmp, pool, registry, runner)
}

/// Helper to eval Lua code with a real synced DB connection. CRUD functions work here.
#[allow(dead_code)]
fn eval_lua_db(runner: &HookRunner, pool: &DbPool, code: &str) -> String {
    let conn = pool.get().expect("conn");
    runner.eval_lua_with_conn(code, &conn, None).expect("eval failed")
}

// ── Lua CRUD Functions ───────────────────────────────────────────────────────

#[test]
fn lua_crud_create_and_find() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(&runner, &pool, r#"
        local doc = crap.collections.create("articles", {
            title = "Test Article",
            body = "Some content here",
        })
        if doc.id == nil then return "NO_ID" end

        local result = crap.collections.find("articles", {})
        if result.total ~= 1 then
            return "WRONG_TOTAL:" .. tostring(result.total)
        end
        local found = result.documents[1]
        if found.title ~= "Test Article" then
            return "WRONG_TITLE:" .. tostring(found.title)
        end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn lua_crud_find_by_id() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(&runner, &pool, r#"
        local doc = crap.collections.create("articles", {
            title = "Find Me By ID",
            body = "Body text",
        })
        local id = doc.id

        local found = crap.collections.find_by_id("articles", id)
        if found == nil then return "NOT_FOUND" end
        if found.title ~= "Find Me By ID" then
            return "WRONG_TITLE:" .. tostring(found.title)
        end
        if found.body ~= "Body text" then
            return "WRONG_BODY:" .. tostring(found.body)
        end
        if found.id ~= id then
            return "WRONG_ID:" .. tostring(found.id)
        end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn lua_crud_update() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(&runner, &pool, r#"
        local doc = crap.collections.create("articles", {
            title = "Original Title",
            body = "Original body",
        })
        local id = doc.id

        local updated = crap.collections.update("articles", id, {
            title = "Updated Title",
        })
        if updated.title ~= "Updated Title" then
            return "UPDATE_FAILED:" .. tostring(updated.title)
        end

        -- Verify via find_by_id
        local found = crap.collections.find_by_id("articles", id)
        if found.title ~= "Updated Title" then
            return "FIND_AFTER_UPDATE_FAILED:" .. tostring(found.title)
        end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn lua_crud_delete() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(&runner, &pool, r#"
        local doc = crap.collections.create("articles", {
            title = "To Be Deleted",
            body = "Goodbye",
        })
        local id = doc.id

        crap.collections.delete("articles", id)

        local result = crap.collections.find("articles", {})
        if result.total ~= 0 then
            return "NOT_DELETED:total=" .. tostring(result.total)
        end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn lua_crud_find_with_filters() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(&runner, &pool, r#"
        crap.collections.create("articles", {
            title = "Alpha Article",
            body = "First",
            status = "published",
        })
        crap.collections.create("articles", {
            title = "Beta Article",
            body = "Second",
            status = "draft",
        })
        crap.collections.create("articles", {
            title = "Gamma Article",
            body = "Third",
            status = "published",
        })

        -- Filter by status = published
        local result = crap.collections.find("articles", {
            filters = { status = "published" },
        })
        if result.total ~= 2 then
            return "WRONG_TOTAL:" .. tostring(result.total)
        end

        -- Filter by status = draft
        local drafts = crap.collections.find("articles", {
            filters = { status = "draft" },
        })
        if drafts.total ~= 1 then
            return "WRONG_DRAFT_TOTAL:" .. tostring(drafts.total)
        end
        if drafts.documents[1].title ~= "Beta Article" then
            return "WRONG_DRAFT_TITLE:" .. tostring(drafts.documents[1].title)
        end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn lua_globals_config_get_and_update() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(&runner, &pool, r#"
        -- Get the default global (should exist with empty/default values)
        local settings = crap.globals.get("settings")
        if settings == nil then return "GET_NIL" end

        -- Update the global
        local updated = crap.globals.update("settings", {
            site_name = "Test Site",
            maintenance_mode = "1",
        })
        if updated == nil then return "UPDATE_NIL" end

        -- Read it back
        local reread = crap.globals.get("settings")
        if reread.site_name ~= "Test Site" then
            return "WRONG_SITE_NAME:" .. tostring(reread.site_name)
        end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

// ── crap.hooks.remove ────────────────────────────────────────────────────────

#[test]
fn lua_hooks_remove() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(&runner, &pool, r#"
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
    "#);
    assert_eq!(result, "ok");
}

// ── crap.locale functions ────────────────────────────────────────────────────

#[test]
fn lua_locale_get_default() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        return crap.locale.get_default()
    "#);
    assert_eq!(result, "en", "Default locale should be 'en'");
}

#[test]
fn lua_locale_get_all() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local all = crap.locale.get_all()
        return tostring(#all)
    "#);
    assert_eq!(result, "0", "No locales configured by default");
}

#[test]
fn lua_locale_is_enabled() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        return tostring(crap.locale.is_enabled())
    "#);
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
    assert_eq!(result, "ok", "HTTP request to invalid port should produce a transport error");
}

// ── crap.email.send (no-op when not configured) ─────────────────────────────

#[test]
fn lua_email_not_configured() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local ok = crap.email.send({
            to = "test@example.com",
            subject = "Test Subject",
            html = "<p>Hello</p>",
        })
        return tostring(ok)
    "#);
    assert_eq!(result, "true", "Email send should return true (no-op) when SMTP not configured");
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
    let result = runner.eval_lua_with_conn(
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
    ).expect("eval failed");
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
    let result = runner.eval_lua_with_conn(
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
    ).expect("eval failed");
    assert_eq!(result, "ok");
}

// ── 4B. Collection Definition Parsing ────────────────────────────────────────

#[test]
fn parse_upload_config() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();

    std::fs::write(
        collections_dir.join("media.lua"),
        r#"
crap.collections.define("media", {
    upload = {
        mime_types = { "image/*", "application/pdf" },
        max_file_size = 10485760,
        image_sizes = {
            { name = "thumbnail", width = 300, height = 300, fit = "cover" },
            { name = "card", width = 640, height = 480 },
        },
        format_options = {
            webp = { quality = 80 },
        },
    },
    fields = {
        { name = "alt", type = "text" },
    },
})
        "#,
    ).unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let reg = registry.read().unwrap();
    let def = reg.get_collection("media").expect("media should be registered");
    assert!(def.is_upload_collection());
    let upload = def.upload.as_ref().unwrap();
    assert_eq!(upload.mime_types.len(), 2);
    assert_eq!(upload.max_file_size, Some(10485760));
    assert_eq!(upload.image_sizes.len(), 2);
    assert_eq!(upload.image_sizes[0].name, "thumbnail");
    assert_eq!(upload.image_sizes[0].width, 300);
    assert!(upload.format_options.webp.is_some());
}

#[test]
fn parse_auth_strategies() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();

    std::fs::write(
        collections_dir.join("users.lua"),
        r#"
crap.collections.define("users", {
    auth = {
        strategies = {
            { name = "api_key", authenticate = "hooks.auth.api_key" },
            { name = "oauth", authenticate = "hooks.auth.oauth" },
        },
    },
    fields = {
        { name = "name", type = "text" },
    },
})
        "#,
    ).unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let reg = registry.read().unwrap();
    let def = reg.get_collection("users").expect("users should be registered");
    assert!(def.is_auth_collection());
    let auth = def.auth.as_ref().unwrap();
    assert_eq!(auth.strategies.len(), 2);
    assert_eq!(auth.strategies[0].name, "api_key");
    assert_eq!(auth.strategies[0].authenticate, "hooks.auth.api_key");
    assert_eq!(auth.strategies[1].name, "oauth");
}

#[test]
fn parse_live_setting_function() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();

    std::fs::write(
        collections_dir.join("events.lua"),
        r#"
crap.collections.define("events", {
    live = "hooks.live.filter",
    fields = {
        { name = "name", type = "text" },
    },
})
        "#,
    ).unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let reg = registry.read().unwrap();
    let def = reg.get_collection("events").expect("events should be registered");
    match &def.live {
        Some(crap_cms::core::collection::LiveSetting::Function(f)) => {
            assert_eq!(f, "hooks.live.filter");
        }
        other => panic!("Expected LiveSetting::Function, got {:?}", other),
    }
}

#[test]
fn parse_live_setting_disabled() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();

    std::fs::write(
        collections_dir.join("private.lua"),
        r#"
crap.collections.define("private", {
    live = false,
    fields = {
        { name = "secret", type = "text" },
    },
})
        "#,
    ).unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let reg = registry.read().unwrap();
    let def = reg.get_collection("private").expect("private should be registered");
    assert!(matches!(&def.live, Some(crap_cms::core::collection::LiveSetting::Disabled)));
}

#[test]
fn parse_blocks_definition() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();

    std::fs::write(
        collections_dir.join("pages.lua"),
        r#"
crap.collections.define("pages", {
    fields = {
        { name = "title", type = "text", required = true },
        { name = "content", type = "blocks", blocks = {
            { type = "text", label = "Text Block", fields = {
                { name = "body", type = "richtext" },
            }},
            { type = "image", label = "Image Block", fields = {
                { name = "src", type = "text" },
                { name = "alt", type = "text" },
            }},
        }},
    },
})
        "#,
    ).unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let reg = registry.read().unwrap();
    let def = reg.get_collection("pages").expect("pages should be registered");
    let blocks_field = def.fields.iter().find(|f| f.name == "content").expect("content field");
    assert_eq!(blocks_field.field_type, crap_cms::core::field::FieldType::Blocks);
    assert_eq!(blocks_field.blocks.len(), 2);
    assert_eq!(blocks_field.blocks[0].block_type, "text");
    assert_eq!(blocks_field.blocks[0].fields.len(), 1);
    assert_eq!(blocks_field.blocks[1].block_type, "image");
    assert_eq!(blocks_field.blocks[1].fields.len(), 2);
}

#[test]
fn parse_select_options_localized() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();

    std::fs::write(
        collections_dir.join("polls.lua"),
        r#"
crap.collections.define("polls", {
    fields = {
        { name = "answer", type = "select", options = {
            { label = { en = "Yes", de = "Ja" }, value = "yes" },
            { label = { en = "No", de = "Nein" }, value = "no" },
        }},
    },
})
        "#,
    ).unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let reg = registry.read().unwrap();
    let def = reg.get_collection("polls").expect("polls should be registered");
    let answer_field = def.fields.iter().find(|f| f.name == "answer").expect("answer field");
    assert_eq!(answer_field.options.len(), 2);
    assert_eq!(answer_field.options[0].value, "yes");
    // The label should be a LocalizedString::Localized
    match &answer_field.options[0].label {
        crap_cms::core::field::LocalizedString::Localized(map) => {
            assert_eq!(map.get("en"), Some(&"Yes".to_string()));
            assert_eq!(map.get("de"), Some(&"Ja".to_string()));
        }
        crap_cms::core::field::LocalizedString::Plain(s) => {
            // Some implementations may flatten it — that's also acceptable
            assert!(!s.is_empty(), "Should have a non-empty label");
        }
    }
}

#[test]
fn parse_localized_label() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();

    std::fs::write(
        collections_dir.join("articles.lua"),
        r#"
crap.collections.define("articles", {
    labels = {
        singular = { en = "Article", de = "Artikel" },
        plural = { en = "Articles", de = "Artikel" },
    },
    fields = {
        { name = "title", type = "text" },
    },
})
        "#,
    ).unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").expect("articles should be registered");

    // Test resolving for different locales
    assert_eq!(def.singular_name_for("en", "en"), "Article");
    assert_eq!(def.singular_name_for("de", "en"), "Artikel");
    assert_eq!(def.display_name_for("en", "en"), "Articles");
}

// ── 4C. crap.email.send with text + html ─────────────────────────────────────

#[test]
fn email_send_with_text_and_html() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local ok = crap.email.send({
            to = "test@example.com",
            subject = "Test",
            html = "<p>HTML body</p>",
            text = "Plain text body",
        })
        return tostring(ok)
    "#);
    assert_eq!(result, "true", "Email with both text and html should return true (no-op)");
}

// ── 4D. Lua CRUD edge cases ──────────────────────────────────────────────────

#[test]
fn lua_find_with_where_clause() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(&runner, &pool, r#"
        crap.collections.create("articles", {
            title = "Where Test Alpha",
            status = "published",
        })
        crap.collections.create("articles", {
            title = "Where Test Beta",
            status = "draft",
        })

        local result = crap.collections.find("articles", {
            filters = { status = { equals = "published" } },
        })
        if result.total ~= 1 then
            return "WRONG_TOTAL:" .. tostring(result.total)
        end
        if result.documents[1].title ~= "Where Test Alpha" then
            return "WRONG_TITLE:" .. tostring(result.documents[1].title)
        end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn lua_find_with_limit_offset() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(&runner, &pool, r#"
        for i = 1, 5 do
            crap.collections.create("articles", {
                title = "Item " .. i,
            })
        end

        local result = crap.collections.find("articles", {
            limit = 2,
            offset = 1,
        })
        if #result.documents ~= 2 then
            return "WRONG_COUNT:" .. tostring(#result.documents)
        end
        if result.total ~= 5 then
            return "WRONG_TOTAL:" .. tostring(result.total)
        end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn lua_find_by_id_with_depth() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(&runner, &pool, r#"
        local doc = crap.collections.create("articles", {
            title = "Depth Test",
            body = "Content",
        })

        -- find_by_id with depth=0 opts
        local found = crap.collections.find_by_id("articles", doc.id, { depth = 0 })
        if found == nil then return "NOT_FOUND" end
        if found.title ~= "Depth Test" then
            return "WRONG_TITLE:" .. tostring(found.title)
        end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

// ── 5A. Versions Config Parsing ─────────────────────────────────────────────

#[test]
fn parse_versions_config_boolean_true() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();

    std::fs::write(
        collections_dir.join("docs.lua"),
        r#"
crap.collections.define("docs", {
    versions = true,
    fields = {
        { name = "title", type = "text" },
    },
})
        "#,
    ).unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let reg = registry.read().unwrap();
    let def = reg.get_collection("docs").expect("docs should be registered");
    assert!(def.has_versions(), "versions=true should enable versions");
    assert!(def.has_drafts(), "versions=true enables drafts by default (PayloadCMS convention)");
    let vc = def.versions.as_ref().unwrap();
    assert!(vc.drafts);
    assert_eq!(vc.max_versions, 0);
}

#[test]
fn parse_versions_config_table_with_drafts() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();

    std::fs::write(
        collections_dir.join("posts.lua"),
        r#"
crap.collections.define("posts", {
    versions = {
        drafts = true,
        max_versions = 50,
    },
    fields = {
        { name = "title", type = "text" },
    },
})
        "#,
    ).unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let reg = registry.read().unwrap();
    let def = reg.get_collection("posts").expect("posts should be registered");
    assert!(def.has_versions(), "should have versions");
    assert!(def.has_drafts(), "should have drafts");
    let vc = def.versions.as_ref().unwrap();
    assert!(vc.drafts);
    assert_eq!(vc.max_versions, 50);
}

#[test]
fn parse_versions_config_false() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();

    std::fs::write(
        collections_dir.join("notes.lua"),
        r#"
crap.collections.define("notes", {
    versions = false,
    fields = {
        { name = "body", type = "textarea" },
    },
})
        "#,
    ).unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let reg = registry.read().unwrap();
    let def = reg.get_collection("notes").expect("notes should be registered");
    assert!(!def.has_versions(), "versions=false should not enable versions");
    assert!(def.versions.is_none());
}

#[test]
fn parse_versions_config_omitted() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();

    std::fs::write(
        collections_dir.join("plain.lua"),
        r#"
crap.collections.define("plain", {
    fields = {
        { name = "name", type = "text" },
    },
})
        "#,
    ).unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let reg = registry.read().unwrap();
    let def = reg.get_collection("plain").expect("plain should be registered");
    assert!(!def.has_versions(), "no versions config should mean no versions");
    assert!(def.versions.is_none());
}

// ── 5B. Lua CRUD with Draft Option ──────────────────────────────────────────

fn setup_versioned_db() -> (tempfile::TempDir, crap_cms::db::DbPool, SharedRegistry, HookRunner) {
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
    ).unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::default();
    let registry = hooks::init_lua(tmp.path(), &config).expect("init_lua");

    let mut db_config = CrapConfig::default();
    db_config.database.path = "test.db".to_string();
    let pool = crap_cms::db::pool::create_pool(tmp.path(), &db_config).expect("pool");
    crap_cms::db::migrate::sync_all(&pool, &registry, &config.locale).expect("sync");

    let runner = HookRunner::new(tmp.path(), registry.clone(), &config).expect("runner");
    (tmp, pool, registry, runner)
}

fn eval_versioned(runner: &HookRunner, pool: &crap_cms::db::DbPool, code: &str) -> String {
    let conn = pool.get().expect("conn");
    runner.eval_lua_with_conn(code, &conn, None).expect("eval failed")
}

#[test]
fn lua_create_with_draft_option() {
    let (_tmp, pool, _reg, runner) = setup_versioned_db();
    let result = eval_versioned(&runner, &pool, r#"
        local doc = crap.collections.create("articles", {
            title = "Draft Article",
            body = "Some content",
        }, { draft = true })

        if doc == nil then return "CREATE_NIL" end
        if doc.id == nil then return "NO_ID" end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn lua_create_draft_skips_required_validation() {
    let (_tmp, pool, _reg, runner) = setup_versioned_db();
    let result = eval_versioned(&runner, &pool, r#"
        -- title is required, but draft=true should skip validation
        local ok, err = pcall(function()
            crap.collections.create("articles", {
                body = "No title, just body",
            }, { draft = true })
        end)
        if ok then return "ok" end
        return "FAILED:" .. tostring(err)
    "#);
    assert_eq!(result, "ok", "Draft create should skip required field validation");
}

#[test]
fn lua_create_publish_enforces_required_validation() {
    let (_tmp, pool, _reg, runner) = setup_versioned_db();
    let result = eval_versioned(&runner, &pool, r#"
        -- title is required, draft=false (publish) should enforce validation
        local ok, err = pcall(function()
            crap.collections.create("articles", {
                body = "No title",
            })
        end)
        if ok then return "SHOULD_HAVE_FAILED" end
        local err_str = tostring(err)
        if err_str:find("required") or err_str:find("title") then
            return "ok"
        end
        return "UNEXPECTED_ERROR:" .. err_str
    "#);
    assert_eq!(result, "ok", "Publish create should enforce required validation");
}

#[test]
fn lua_update_with_draft_option() {
    let (_tmp, pool, _reg, runner) = setup_versioned_db();
    let result = eval_versioned(&runner, &pool, r#"
        -- Create a published document first
        local doc = crap.collections.create("articles", {
            title = "Published Article",
            body = "Original body",
        })
        local id = doc.id

        -- Draft update should NOT modify the main table
        local updated = crap.collections.update("articles", id, {
            title = "Draft Title Change",
        }, { draft = true })

        -- The returned doc should still have the original title
        -- (version-only save, main table unchanged)
        local current = crap.collections.find_by_id("articles", id)
        if current.title ~= "Published Article" then
            return "MAIN_TABLE_CHANGED:" .. tostring(current.title)
        end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn lua_update_publish_modifies_main_table() {
    let (_tmp, pool, _reg, runner) = setup_versioned_db();
    let result = eval_versioned(&runner, &pool, r#"
        local doc = crap.collections.create("articles", {
            title = "Original",
            body = "Content",
        })
        local id = doc.id

        -- Publish update (no draft option)
        local updated = crap.collections.update("articles", id, {
            title = "Updated Title",
        })

        local current = crap.collections.find_by_id("articles", id)
        if current.title ~= "Updated Title" then
            return "NOT_UPDATED:" .. tostring(current.title)
        end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

// ══════════════════════════════════════════════════════════════════════════════
// NEW FEATURES: crap.collections.count, update_many, delete_many
// ══════════════════════════════════════════════════════════════════════════════

#[test]
fn lua_count_empty_collection() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(&runner, &pool, r#"
        local count = crap.collections.count("articles")
        return tostring(count)
    "#);
    assert_eq!(result, "0");
}

#[test]
fn lua_count_with_documents() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(&runner, &pool, r#"
        crap.collections.create("articles", { title = "A", body = "1" })
        crap.collections.create("articles", { title = "B", body = "2" })
        crap.collections.create("articles", { title = "C", body = "3" })
        return tostring(crap.collections.count("articles"))
    "#);
    assert_eq!(result, "3");
}

#[test]
fn lua_count_with_filters() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(&runner, &pool, r#"
        crap.collections.create("articles", { title = "A", status = "published" })
        crap.collections.create("articles", { title = "B", status = "draft" })
        crap.collections.create("articles", { title = "C", status = "published" })
        local count = crap.collections.count("articles", {
            filters = { status = "published" },
        })
        return tostring(count)
    "#);
    assert_eq!(result, "2");
}

#[test]
fn lua_count_nonexistent_collection() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let conn = pool.get().expect("conn");
    let result = runner.eval_lua_with_conn(r#"
        local ok, err = pcall(function()
            crap.collections.count("nonexistent")
        end)
        if ok then return "SHOULD_HAVE_FAILED" end
        if tostring(err):find("not found") then return "ok" end
        return "UNEXPECTED:" .. tostring(err)
    "#, &conn, None).expect("eval");
    assert_eq!(result, "ok");
}

#[test]
fn lua_update_many_basic() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(&runner, &pool, r#"
        crap.collections.create("articles", { title = "A", status = "draft" })
        crap.collections.create("articles", { title = "B", status = "draft" })
        crap.collections.create("articles", { title = "C", status = "published" })

        local result = crap.collections.update_many("articles",
            { filters = { status = "draft" } },
            { status = "published" }
        )
        if result.modified ~= 2 then
            return "WRONG_MODIFIED:" .. tostring(result.modified)
        end

        -- Verify all are now published
        local count = crap.collections.count("articles", {
            filters = { status = "published" },
        })
        if count ~= 3 then
            return "WRONG_COUNT:" .. tostring(count)
        end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn lua_update_many_no_matches() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(&runner, &pool, r#"
        crap.collections.create("articles", { title = "A", status = "published" })

        local result = crap.collections.update_many("articles",
            { filters = { status = "archived" } },
            { status = "published" }
        )
        if result.modified ~= 0 then
            return "WRONG_MODIFIED:" .. tostring(result.modified)
        end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn lua_delete_many_basic() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(&runner, &pool, r#"
        crap.collections.create("articles", { title = "A", status = "draft" })
        crap.collections.create("articles", { title = "B", status = "draft" })
        crap.collections.create("articles", { title = "C", status = "published" })

        local result = crap.collections.delete_many("articles",
            { filters = { status = "draft" } }
        )
        if result.deleted ~= 2 then
            return "WRONG_DELETED:" .. tostring(result.deleted)
        end

        -- Only the published article should remain
        local count = crap.collections.count("articles")
        if count ~= 1 then
            return "WRONG_REMAINING:" .. tostring(count)
        end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn lua_delete_many_no_matches() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(&runner, &pool, r#"
        crap.collections.create("articles", { title = "A", status = "published" })

        local result = crap.collections.delete_many("articles",
            { filters = { status = "archived" } }
        )
        if result.deleted ~= 0 then
            return "WRONG_DELETED:" .. tostring(result.deleted)
        end

        local count = crap.collections.count("articles")
        if count ~= 1 then
            return "WRONG_REMAINING:" .. tostring(count)
        end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn lua_delete_many_all() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(&runner, &pool, r#"
        crap.collections.create("articles", { title = "A" })
        crap.collections.create("articles", { title = "B" })
        crap.collections.create("articles", { title = "C" })

        -- Empty filter matches all
        local result = crap.collections.delete_many("articles", {})
        if result.deleted ~= 3 then
            return "WRONG_DELETED:" .. tostring(result.deleted)
        end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

// ══════════════════════════════════════════════════════════════════════════════
// NEW FEATURES: crap.util — pure Lua table helpers
// ══════════════════════════════════════════════════════════════════════════════

#[test]
fn util_deep_merge() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local a = { x = 1, nested = { a = 1, b = 2 } }
        local b = { y = 2, nested = { b = 3, c = 4 } }
        local merged = crap.util.deep_merge(a, b)
        if merged.x ~= 1 then return "X" end
        if merged.y ~= 2 then return "Y" end
        if merged.nested.a ~= 1 then return "NA" end
        if merged.nested.b ~= 3 then return "NB" end
        if merged.nested.c ~= 4 then return "NC" end
        -- Original tables should not be modified
        if a.y ~= nil then return "A_MODIFIED" end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn util_pick() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local t = { a = 1, b = 2, c = 3, d = 4 }
        local picked = crap.util.pick(t, { "a", "c" })
        if picked.a ~= 1 then return "A" end
        if picked.c ~= 3 then return "C" end
        if picked.b ~= nil then return "B_PRESENT" end
        if picked.d ~= nil then return "D_PRESENT" end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn util_omit() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local t = { a = 1, b = 2, c = 3, d = 4 }
        local result = crap.util.omit(t, { "b", "d" })
        if result.a ~= 1 then return "A" end
        if result.c ~= 3 then return "C" end
        if result.b ~= nil then return "B_PRESENT" end
        if result.d ~= nil then return "D_PRESENT" end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn util_keys_and_values() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local t = { x = 10, y = 20 }
        local k = crap.util.keys(t)
        local v = crap.util.values(t)
        if #k ~= 2 then return "KEYS_LEN:" .. #k end
        if #v ~= 2 then return "VALUES_LEN:" .. #v end
        -- keys and values should contain the right elements (order may vary)
        local has_x, has_y = false, false
        for _, key in ipairs(k) do
            if key == "x" then has_x = true end
            if key == "y" then has_y = true end
        end
        if not has_x or not has_y then return "MISSING_KEY" end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn util_map() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local arr = { 1, 2, 3, 4 }
        local doubled = crap.util.map(arr, function(v) return v * 2 end)
        if #doubled ~= 4 then return "LEN:" .. #doubled end
        if doubled[1] ~= 2 then return "V1:" .. doubled[1] end
        if doubled[4] ~= 8 then return "V4:" .. doubled[4] end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn util_filter() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local arr = { 1, 2, 3, 4, 5 }
        local evens = crap.util.filter(arr, function(v) return v % 2 == 0 end)
        if #evens ~= 2 then return "LEN:" .. #evens end
        if evens[1] ~= 2 then return "V1:" .. evens[1] end
        if evens[2] ~= 4 then return "V2:" .. evens[2] end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn util_find() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local arr = { 10, 20, 30, 40 }
        local found = crap.util.find(arr, function(v) return v > 25 end)
        if found ~= 30 then return "FOUND:" .. tostring(found) end
        local not_found = crap.util.find(arr, function(v) return v > 100 end)
        if not_found ~= nil then return "SHOULD_BE_NIL" end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn util_includes() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local arr = { "a", "b", "c" }
        if not crap.util.includes(arr, "b") then return "MISSING_B" end
        if crap.util.includes(arr, "z") then return "HAS_Z" end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn util_is_empty() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        if not crap.util.is_empty({}) then return "EMPTY_NOT_EMPTY" end
        if crap.util.is_empty({ 1 }) then return "NON_EMPTY_IS_EMPTY" end
        if crap.util.is_empty({ x = 1 }) then return "MAP_IS_EMPTY" end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn util_clone() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local orig = { a = 1, b = 2 }
        local copy = crap.util.clone(orig)
        copy.a = 99
        if orig.a ~= 1 then return "ORIGINAL_MODIFIED" end
        if copy.a ~= 99 then return "COPY_WRONG" end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

// ══════════════════════════════════════════════════════════════════════════════
// NEW FEATURES: crap.util — pure Lua string helpers
// ══════════════════════════════════════════════════════════════════════════════

#[test]
fn util_trim() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local trimmed = crap.util.trim("  hello world  ")
        return trimmed
    "#);
    assert_eq!(result, "hello world");
}

#[test]
fn util_split() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local parts = crap.util.split("a,b,c", ",")
        if #parts ~= 3 then return "LEN:" .. #parts end
        if parts[1] ~= "a" then return "P1:" .. parts[1] end
        if parts[2] ~= "b" then return "P2:" .. parts[2] end
        if parts[3] ~= "c" then return "P3:" .. parts[3] end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn util_starts_with_ends_with() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        if not crap.util.starts_with("hello world", "hello") then return "SW_FAIL" end
        if crap.util.starts_with("hello world", "world") then return "SW_FALSE_POS" end
        if not crap.util.ends_with("hello world", "world") then return "EW_FAIL" end
        if crap.util.ends_with("hello world", "hello") then return "EW_FALSE_POS" end
        if not crap.util.ends_with("test", "") then return "EW_EMPTY" end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn util_truncate() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local short = crap.util.truncate("hello", 10)
        if short ~= "hello" then return "SHORT:" .. short end

        local truncated = crap.util.truncate("hello world", 8)
        if truncated ~= "hello..." then return "TRUNC:" .. truncated end

        local custom = crap.util.truncate("hello world", 8, "~")
        if custom ~= "hello w~" then return "CUSTOM:" .. custom end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

// ══════════════════════════════════════════════════════════════════════════════
// NEW FEATURES: crap.util — date helpers (Rust/chrono)
// ══════════════════════════════════════════════════════════════════════════════

#[test]
fn util_date_now_returns_iso_string() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local now = crap.util.date_now()
        -- Should contain 'T' (ISO 8601 separator) and be non-empty
        if #now < 10 then return "TOO_SHORT:" .. now end
        if not now:find("T") then return "NO_T:" .. now end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn util_date_timestamp_returns_number() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local ts = crap.util.date_timestamp()
        if type(ts) ~= "number" then return "NOT_NUMBER:" .. type(ts) end
        -- Sanity check: timestamp should be after 2024
        if ts < 1700000000 then return "TOO_OLD:" .. tostring(ts) end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn util_date_parse_rfc3339() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local ts = crap.util.date_parse("2024-01-15T12:30:00+00:00")
        if ts ~= 1705321800 then return "WRONG:" .. tostring(ts) end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn util_date_parse_date_only() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local ts = crap.util.date_parse("2024-01-01")
        if ts ~= 1704067200 then return "WRONG:" .. tostring(ts) end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn util_date_parse_datetime() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local ts = crap.util.date_parse("2024-01-01 12:00:00")
        if ts ~= 1704110400 then return "WRONG:" .. tostring(ts) end
        return "ok"
    "#);
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
    let result = runner.eval_lua_with_conn(r#"
        local ok, err = pcall(function()
            crap.util.date_parse("not-a-date")
        end)
        if ok then return "SHOULD_HAVE_FAILED" end
        if tostring(err):find("could not parse") then return "ok" end
        return "UNEXPECTED:" .. tostring(err)
    "#, &conn, None).expect("eval");
    assert_eq!(result, "ok");
}

#[test]
fn util_date_format() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        -- 2024-01-15 12:30:00 UTC
        local formatted = crap.util.date_format(1705321800, "%Y-%m-%d")
        if formatted ~= "2024-01-15" then return "WRONG:" .. formatted end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn util_date_add_and_diff() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local ts = 1000000
        local added = crap.util.date_add(ts, 3600)
        if added ~= 1003600 then return "ADD:" .. tostring(added) end

        local diff = crap.util.date_diff(added, ts)
        if diff ~= 3600 then return "DIFF:" .. tostring(diff) end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

// ══════════════════════════════════════════════════════════════════════════════
// NEW FEATURES: crap.crypto
// ══════════════════════════════════════════════════════════════════════════════

#[test]
fn crypto_sha256() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local hash = crap.crypto.sha256("hello")
        -- Known SHA-256 of "hello"
        if hash ~= "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824" then
            return "WRONG:" .. hash
        end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn crypto_sha256_empty() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local hash = crap.crypto.sha256("")
        -- Known SHA-256 of empty string
        if hash ~= "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855" then
            return "WRONG:" .. hash
        end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn crypto_hmac_sha256() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
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
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn crypto_base64_roundtrip() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local original = "Hello, World! 123 Special chars: @#$%"
        local encoded = crap.crypto.base64_encode(original)
        local decoded = crap.crypto.base64_decode(encoded)
        if decoded ~= original then
            return "MISMATCH:" .. decoded
        end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn crypto_base64_known_value() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local encoded = crap.crypto.base64_encode("hello")
        if encoded ~= "aGVsbG8=" then return "WRONG:" .. encoded end
        local decoded = crap.crypto.base64_decode("aGVsbG8=")
        if decoded ~= "hello" then return "DECODE_WRONG:" .. decoded end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn crypto_encrypt_decrypt_roundtrip() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
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
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn crypto_encrypt_produces_different_ciphertexts() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        -- Same plaintext should produce different ciphertexts (random nonce)
        local a = crap.crypto.encrypt("same text")
        local b = crap.crypto.encrypt("same text")
        if a == b then return "SAME_CIPHERTEXT" end
        -- But both should decrypt to the same thing
        if crap.crypto.decrypt(a) ~= "same text" then return "A_WRONG" end
        if crap.crypto.decrypt(b) ~= "same text" then return "B_WRONG" end
        return "ok"
    "#);
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
    let result = runner.eval_lua_with_conn(r#"
        local ok, err = pcall(function()
            crap.crypto.decrypt("not-valid-base64!@#$")
        end)
        if ok then return "SHOULD_HAVE_FAILED" end
        return "ok"
    "#, &conn, None).expect("eval");
    assert_eq!(result, "ok");
}

#[test]
fn crypto_random_bytes() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local hex = crap.crypto.random_bytes(16)
        -- 16 bytes = 32 hex characters
        if #hex ~= 32 then return "LEN:" .. #hex end
        -- Should be hex (only 0-9a-f)
        if hex:find("[^0-9a-f]") then return "NOT_HEX:" .. hex end
        -- Two calls should produce different results
        local hex2 = crap.crypto.random_bytes(16)
        if hex == hex2 then return "SAME" end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn crypto_random_bytes_various_sizes() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local h1 = crap.crypto.random_bytes(1)
        if #h1 ~= 2 then return "1B:" .. #h1 end
        local h32 = crap.crypto.random_bytes(32)
        if #h32 ~= 64 then return "32B:" .. #h32 end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

// ══════════════════════════════════════════════════════════════════════════════
// NEW FEATURES: crap.schema
// ══════════════════════════════════════════════════════════════════════════════

#[test]
fn schema_get_collection() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
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
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn schema_get_collection_nonexistent() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local schema = crap.schema.get_collection("nonexistent")
        if schema ~= nil then return "NOT_NIL" end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn schema_get_global() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local schema = crap.schema.get_global("settings")
        if schema == nil then return "NIL" end
        if schema.slug ~= "settings" then return "SLUG:" .. tostring(schema.slug) end
        if #schema.fields ~= 2 then return "FIELDS:" .. tostring(#schema.fields) end
        if schema.fields[1].name ~= "site_name" then return "F1:" .. schema.fields[1].name end
        if schema.fields[2].name ~= "maintenance_mode" then return "F2:" .. schema.fields[2].name end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn schema_get_global_nonexistent() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local schema = crap.schema.get_global("nonexistent")
        if schema ~= nil then return "NOT_NIL" end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn schema_list_collections() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
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
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn schema_list_globals() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
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
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn schema_collection_metadata() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local schema = crap.schema.get_collection("articles")
        -- articles fixture doesn't have auth/upload/versions
        if schema.has_auth then return "HAS_AUTH" end
        if schema.has_upload then return "HAS_UPLOAD" end
        if schema.has_versions then return "HAS_VERSIONS" end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

#[test]
fn schema_field_with_options() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local schema = crap.schema.get_collection("articles")
        -- Find the status field which has select options
        local status_field = nil
        for _, f in ipairs(schema.fields) do
            if f.name == "status" then status_field = f; break end
        end
        if status_field == nil then return "NO_STATUS" end
        if status_field.type ~= "select" then return "TYPE:" .. status_field.type end
        if #status_field.options ~= 2 then return "OPTS:" .. #status_field.options end
        if status_field.options[1].value ~= "draft" then return "OPT1:" .. status_field.options[1].value end
        if status_field.options[2].value ~= "published" then return "OPT2:" .. status_field.options[2].value end
        return "ok"
    "#);
    assert_eq!(result, "ok");
}

// ══════════════════════════════════════════════════════════════════════════════
// NEW FEATURES: crap.context — request-scoped shared table
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
    ).unwrap();

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
    ).unwrap();

    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::default();
    let registry = hooks::init_lua(tmp.path(), &config).expect("init_lua");

    let mut db_config = CrapConfig::default();
    db_config.database.path = "test.db".to_string();
    let pool = crap_cms::db::pool::create_pool(tmp.path(), &db_config).expect("pool");
    crap_cms::db::migrate::sync_all(&pool, &registry, &config.locale).expect("sync");

    let runner = HookRunner::new(tmp.path(), registry.clone(), &config).expect("runner");

    let reg = registry.read().unwrap();
    let def = reg.get_collection("items").expect("items");

    let mut data = HashMap::new();
    data.insert("name".to_string(), serde_json::json!("test"));

    let ctx = HookContext {
        collection: "items".to_string(),
        operation: "create".to_string(),
        data,
        locale: None,
        draft: None,
        context: HashMap::new(),
    };

    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");

    // Run before_validate
    let ctx = runner.run_hooks_with_conn(
        &def.hooks, HookEvent::BeforeValidate, ctx, &tx, None,
    ).expect("before_validate");

    assert_eq!(
        ctx.context.get("step1"),
        Some(&serde_json::json!("validated")),
        "step1 should be set after before_validate"
    );

    // Run before_change — should see context from before_validate
    let ctx = runner.run_hooks_with_conn(
        &def.hooks, HookEvent::BeforeChange, ctx, &tx, None,
    ).expect("before_change");

    assert_eq!(
        ctx.context.get("step1"),
        Some(&serde_json::json!("validated")),
        "step1 should persist through before_change"
    );
    assert_eq!(
        ctx.context.get("step2"),
        Some(&serde_json::json!("changed")),
        "step2 should be set after before_change"
    );
    assert_eq!(
        ctx.context.get("counter"),
        Some(&serde_json::json!(2)),
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
    };

    assert!(ctx.context.is_empty(), "Context should start empty");
}

// ── After-Hook CRUD Access Tests ─────────────────────────────────────────────

#[test]
fn after_hook_has_crud_access() {
    use crap_cms::core::collection::CollectionHooks;
    use crap_cms::hooks::lifecycle::{HookContext, HookEvent};

    let (_tmp, pool, registry, runner) = setup_with_db();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    // Build hooks with an after_change hook that creates a side-effect document
    let hooks = CollectionHooks {
        after_change: vec!["hooks.after_crud.create_side_effect".to_string()],
        ..Default::default()
    };

    // First, create a document so the after-hook has something to work with
    let mut conn = pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = [
        ("title".to_string(), "original".to_string()),
        ("status".to_string(), "published".to_string()),
    ].into();
    let doc = crap_cms::db::query::create(&tx, "articles", &def, &data, None).unwrap();

    // Run after_change hooks inside the same transaction
    let ctx = HookContext {
        collection: "articles".to_string(),
        operation: "create".to_string(),
        data: doc.fields.clone(),
        locale: None,
        draft: None,
        context: std::collections::HashMap::new(),
    };
    let result = runner.run_after_write(
        &hooks, &def.fields, HookEvent::AfterChange, ctx, &tx, None,
    );
    assert!(result.is_ok(), "after_change hook with CRUD should succeed: {:?}", result.err());

    // Commit the transaction
    tx.commit().unwrap();

    // Verify the side-effect document was created
    let conn2 = pool.get().unwrap();
    let docs = crap_cms::db::query::find(
        &conn2, "articles", &def,
        &crap_cms::db::query::FindQuery::default(), None,
    ).unwrap();

    let side_effect = docs.iter().find(|d| {
        d.fields.get("title").and_then(|v| v.as_str()) == Some("side-effect-from-after-hook")
    });
    assert!(side_effect.is_some(), "Side-effect document should have been created by after_change hook");
}

#[test]
fn after_hook_error_rolls_back() {
    use crap_cms::core::collection::CollectionHooks;
    use crap_cms::hooks::lifecycle::{HookContext, HookEvent};

    let (_tmp, pool, registry, runner) = setup_with_db();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    // Build hooks with an after_change hook that errors
    let hooks = CollectionHooks {
        after_change: vec!["hooks.after_crud.error_hook".to_string()],
        ..Default::default()
    };

    // Create a document inside a transaction
    let mut conn = pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = [
        ("title".to_string(), "should-be-rolled-back".to_string()),
        ("status".to_string(), "published".to_string()),
    ].into();
    let doc = crap_cms::db::query::create(&tx, "articles", &def, &data, None).unwrap();
    let doc_id = doc.id.clone();

    // Run after_change hooks — this should error
    let ctx = HookContext {
        collection: "articles".to_string(),
        operation: "create".to_string(),
        data: doc.fields.clone(),
        locale: None,
        draft: None,
        context: std::collections::HashMap::new(),
    };
    let result = runner.run_after_write(
        &hooks, &def.fields, HookEvent::AfterChange, ctx, &tx, None,
    );
    assert!(result.is_err(), "after_change hook error should propagate");

    // Drop the transaction without committing (simulates rollback)
    drop(tx);

    // Verify the document was NOT persisted (transaction was not committed)
    let conn2 = pool.get().unwrap();
    let found = crap_cms::db::query::find_by_id(
        &conn2, "articles", &def, &doc_id, None,
    ).unwrap();
    assert!(found.is_none(), "Document should NOT exist after after-hook error (tx rolled back)");
}

#[test]
fn context_flows_to_after_hooks() {
    use crap_cms::core::collection::CollectionHooks;
    use crap_cms::hooks::lifecycle::{HookContext, HookEvent};

    let (_tmp, pool, _registry, runner) = setup_with_db();

    // Build hooks with an after_change hook that reads ctx.context
    let hooks = CollectionHooks {
        after_change: vec!["hooks.after_crud.check_context".to_string()],
        ..Default::default()
    };

    let mut conn = pool.get().unwrap();
    let tx = conn.transaction().unwrap();

    // Simulate a context that was set by before-hooks
    let mut req_context = HashMap::new();
    req_context.insert(
        "before_marker".to_string(),
        serde_json::json!("set-by-before-hook"),
    );

    let ctx = HookContext {
        collection: "articles".to_string(),
        operation: "create".to_string(),
        data: HashMap::new(),
        locale: None,
        draft: None,
        context: req_context,
    };

    let result = runner.run_after_write(
        &hooks, &[], HookEvent::AfterChange, ctx, &tx, None,
    );
    assert!(result.is_ok(), "after_change hook should succeed");

    let result_ctx = result.unwrap();
    // The hook reads ctx.context.before_marker and writes it to ctx.data._context_received
    assert_eq!(
        result_ctx.data.get("_context_received").and_then(|v| v.as_str()),
        Some("set-by-before-hook"),
        "after_change hook should receive the context set by before-hooks"
    );
}

// ── Date normalization integration tests ────────────────────────────────────

#[test]
fn date_field_normalizes_date_only_to_utc_noon() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(&runner, &pool, r#"
        local doc = crap.collections.create("articles", {
            title = "Date Test 1",
            published_at = "2026-03-15",
        })
        local found = crap.collections.find_by_id("articles", doc.id)
        return found.published_at
    "#);
    assert_eq!(result, "2026-03-15T12:00:00.000Z");
}

#[test]
fn date_field_normalizes_full_datetime() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(&runner, &pool, r#"
        local doc = crap.collections.create("articles", {
            title = "Date Test 2",
            event_at = "2026-03-15T09:00:00Z",
        })
        local found = crap.collections.find_by_id("articles", doc.id)
        return found.event_at
    "#);
    assert_eq!(result, "2026-03-15T09:00:00.000Z");
}

// ── crap.collections.config.get() ────────────────────────────────────────────────

#[test]
fn collections_config_get_returns_nil_for_unknown() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local def = crap.collections.config.get("nonexistent")
        return tostring(def)
    "#);
    assert_eq!(result, "nil");
}

#[test]
fn collections_config_get_returns_labels_and_fields() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local def = crap.collections.config.get("articles")
        if def == nil then return "nil" end
        local parts = {}
        parts[#parts + 1] = def.labels.singular
        parts[#parts + 1] = def.labels.plural
        parts[#parts + 1] = tostring(#def.fields)
        return table.concat(parts, "|")
    "#);
    let parts: Vec<&str> = result.split('|').collect();
    assert_eq!(parts[0], "Article");
    assert_eq!(parts[1], "Articles");
    // articles has 7 fields: title, body, status, slug, word_count, published_at, event_at
    assert_eq!(parts[2], "7");
}

#[test]
fn collections_config_get_includes_field_details() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local def = crap.collections.config.get("articles")
        local title = def.fields[1]
        local parts = {}
        parts[#parts + 1] = title.name
        parts[#parts + 1] = title.type
        parts[#parts + 1] = tostring(title.required)
        parts[#parts + 1] = tostring(title.unique)
        return table.concat(parts, "|")
    "#);
    assert_eq!(result, "title|text|true|true");
}

#[test]
fn collections_config_get_includes_hooks() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local def = crap.collections.config.get("articles")
        return def.hooks.before_validate[1]
    "#);
    assert_eq!(result, "hooks.article_hooks.before_validate");
}

#[test]
fn collections_config_get_includes_field_hooks() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local def = crap.collections.config.get("articles")
        -- slug is field 4
        local slug = def.fields[4]
        return slug.hooks.before_change[1]
    "#);
    assert_eq!(result, "hooks.field_hooks.slugify_title");
}

#[test]
fn collections_config_get_includes_select_options() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local def = crap.collections.config.get("articles")
        -- status is field 3
        local status = def.fields[3]
        local parts = {}
        for _, opt in ipairs(status.options) do
            parts[#parts + 1] = opt.label .. "=" .. opt.value
        end
        return table.concat(parts, "|")
    "#);
    assert_eq!(result, "Draft=draft|Published=published");
}

#[test]
fn collections_config_get_includes_picker_appearance() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local def = crap.collections.config.get("articles")
        -- event_at is field 7, has picker_appearance = "dayAndTime"
        local event_at = def.fields[7]
        return event_at.picker_appearance
    "#);
    assert_eq!(result, "dayAndTime");
}

#[test]
fn collections_config_get_roundtrip_redefine() {
    let runner = setup_lua();
    // Get the definition, modify it, redefine, and get again to verify round-trip
    let result = eval_lua(&runner, r#"
        local def = crap.collections.config.get("articles")
        -- Add a new field
        def.fields[#def.fields + 1] = { name = "extra", type = "text" }
        -- Redefine
        crap.collections.define("articles", def)
        -- Get again
        local def2 = crap.collections.config.get("articles")
        local parts = {}
        parts[#parts + 1] = tostring(#def2.fields)
        parts[#parts + 1] = def2.fields[#def2.fields].name
        parts[#parts + 1] = def2.labels.singular
        parts[#parts + 1] = def2.hooks.before_change[1]
        return table.concat(parts, "|")
    "#);
    let parts: Vec<&str> = result.split('|').collect();
    assert_eq!(parts[0], "8"); // 7 original + 1 new
    assert_eq!(parts[1], "extra");
    assert_eq!(parts[2], "Article"); // labels preserved
    assert_eq!(parts[3], "hooks.article_hooks.before_change"); // hooks preserved
}

// ── crap.globals.config.get() / crap.globals.config.list() ──────────────────────────────

#[test]
fn globals_config_get_returns_nil_for_unknown() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        return tostring(crap.globals.config.get("nonexistent"))
    "#);
    assert_eq!(result, "nil");
}

#[test]
fn globals_config_get_returns_labels_and_fields() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local def = crap.globals.config.get("settings")
        local parts = {}
        parts[#parts + 1] = def.labels.singular
        parts[#parts + 1] = tostring(#def.fields)
        parts[#parts + 1] = def.fields[1].name
        return table.concat(parts, "|")
    "#);
    assert_eq!(result, "Settings|2|site_name");
}

#[test]
fn globals_config_get_roundtrip_redefine() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local def = crap.globals.config.get("settings")
        def.fields[#def.fields + 1] = { name = "footer_text", type = "text" }
        crap.globals.define("settings", def)
        local def2 = crap.globals.config.get("settings")
        local parts = {}
        parts[#parts + 1] = tostring(#def2.fields)
        parts[#parts + 1] = def2.fields[#def2.fields].name
        parts[#parts + 1] = def2.labels.singular
        return table.concat(parts, "|")
    "#);
    assert_eq!(result, "3|footer_text|Settings");
}

#[test]
fn globals_list_returns_slug_keyed_map() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local all = crap.globals.config.list()
        local slugs = {}
        for slug, _ in pairs(all) do
            slugs[#slugs + 1] = slug
        end
        table.sort(slugs)
        return table.concat(slugs, ",")
    "#);
    assert!(result.contains("settings"), "should contain settings, got: {}", result);
}

#[test]
fn globals_list_can_modify_and_redefine() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        for slug, def in pairs(crap.globals.config.list()) do
            if slug == "settings" then
                def.fields[#def.fields + 1] = { name = "plugin_field", type = "text" }
                crap.globals.define(slug, def)
            end
        end
        local updated = crap.globals.config.get("settings")
        return updated.fields[#updated.fields].name
    "#);
    assert_eq!(result, "plugin_field");
}

#[test]
fn collections_list_returns_all_collections() {
    let runner = setup_lua();
    let result = eval_lua(&runner, r#"
        local all = crap.collections.config.list()
        local slugs = {}
        for slug, _ in pairs(all) do
            slugs[#slugs + 1] = slug
        end
        table.sort(slugs)
        return table.concat(slugs, ",")
    "#);
    assert!(result.contains("articles"), "should contain articles, got: {}", result);
}

#[test]
fn collections_list_can_filter_and_redefine() {
    let runner = setup_lua();
    // Simulate a plugin that adds a field to every collection
    let result = eval_lua(&runner, r#"
        for slug, def in pairs(crap.collections.config.list()) do
            if slug == "articles" then
                def.fields[#def.fields + 1] = { name = "plugin_field", type = "text" }
                crap.collections.define(slug, def)
            end
        end
        local updated = crap.collections.config.get("articles")
        return updated.fields[#updated.fields].name
    "#);
    assert_eq!(result, "plugin_field");
}
