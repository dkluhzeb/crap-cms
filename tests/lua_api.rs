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
fn lua_globals_get_and_update() {
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
