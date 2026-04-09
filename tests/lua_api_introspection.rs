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
    let config = CrapConfig::test_default();
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
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    let pool = crap_cms::db::pool::create_pool(tmp.path(), &config).expect("pool");
    let conn = pool.get().expect("conn");
    runner
        .eval_lua_with_conn(code, &conn, None)
        .expect("eval failed")
}

/// Set up a HookRunner with a real synced database (tables created from Lua definitions).
/// Returns (tempdir, pool, registry, runner). The tempdir must be kept alive for the DB.
#[allow(dead_code)]
fn setup_with_db() -> (tempfile::TempDir, DbPool, SharedRegistry, HookRunner) {
    let config_dir = fixture_dir();
    let config = CrapConfig::test_default();
    let registry = hooks::init_lua(&config_dir, &config).expect("init_lua failed");

    // Create a pool and sync tables from Lua-defined collections/globals
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut db_config = CrapConfig::test_default();
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

// ── 3D. Definition Parsing Edge Cases ────────────────────────────────────────

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
// crap.crypto.* tests (additional)
// ══════════════════════════════════════════════════════════════════════════════

#[test]
fn lua_crypto_sha256() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local hash = crap.crypto.sha256("hello")
        -- Known SHA256 of "hello"
        if hash == "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824" then
            return "ok"
        end
        return "WRONG:" .. hash
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn lua_crypto_hmac_sha256() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local sig = crap.crypto.hmac_sha256("message", "secret_key")
        -- HMAC should be a 64-char hex string
        if #sig ~= 64 then return "BAD_LEN:" .. tostring(#sig) end
        -- Verify it's hex only
        if sig:match("^[0-9a-f]+$") == nil then return "NOT_HEX" end
        -- Same inputs should always produce the same output
        local sig2 = crap.crypto.hmac_sha256("message", "secret_key")
        if sig ~= sig2 then return "NOT_DETERMINISTIC" end
        -- Different key should produce different output
        local sig3 = crap.crypto.hmac_sha256("message", "other_key")
        if sig == sig3 then return "SAME_WITH_DIFFERENT_KEY" end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn lua_crypto_base64_encode_decode() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local encoded = crap.crypto.base64_encode("Hello, World!")
        if encoded ~= "SGVsbG8sIFdvcmxkIQ==" then
            return "ENCODE:" .. encoded
        end
        local decoded = crap.crypto.base64_decode(encoded)
        if decoded ~= "Hello, World!" then
            return "DECODE:" .. decoded
        end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn lua_crypto_base64_decode_invalid() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local ok, err = pcall(function()
            crap.crypto.base64_decode("!!!invalid!!!")
        end)
        if ok then return "SHOULD_FAIL" end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn lua_crypto_encrypt_decrypt_roundtrip() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local plaintext = "secret message 123"
        local encrypted = crap.crypto.encrypt(plaintext)
        -- Encrypted text should be a base64 string, different from plaintext
        if encrypted == plaintext then return "NOT_ENCRYPTED" end
        if #encrypted == 0 then return "EMPTY_ENCRYPTED" end
        -- Decrypt should produce the original
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
fn lua_crypto_decrypt_invalid() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local ok, err = pcall(function()
            -- Too-short ciphertext (less than 12 bytes for nonce)
            crap.crypto.decrypt("AQID")
        end)
        if ok then return "SHOULD_FAIL" end
        local err_str = tostring(err)
        if err_str:find("too short") or err_str:find("decrypt") then
            return "ok"
        end
        return "UNEXPECTED:" .. err_str
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn lua_crypto_random_bytes() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local bytes16 = crap.crypto.random_bytes(16)
        -- Should produce 32-char hex string (16 bytes * 2 chars per byte)
        if #bytes16 ~= 32 then return "BAD_LEN:" .. tostring(#bytes16) end
        -- Should be hex
        if bytes16:match("^[0-9a-f]+$") == nil then return "NOT_HEX" end
        -- Different calls should produce different results
        local bytes16_2 = crap.crypto.random_bytes(16)
        if bytes16 == bytes16_2 then return "NOT_RANDOM" end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

// ══════════════════════════════════════════════════════════════════════════════
// crap.schema.* tests (covers hooks/api/schema.rs) - with setup_with_db
// ══════════════════════════════════════════════════════════════════════════════

#[test]
fn lua_schema_get_collection() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        local schema = crap.schema.get_collection("articles")
        if schema == nil then return "NIL" end
        if schema.slug ~= "articles" then return "WRONG_SLUG:" .. tostring(schema.slug) end
        if schema.timestamps ~= true then return "NO_TIMESTAMPS" end
        -- Should have fields
        if schema.fields == nil then return "NO_FIELDS" end
        if #schema.fields == 0 then return "EMPTY_FIELDS" end
        -- Check first field
        local f = schema.fields[1]
        if f.name == nil then return "NO_FIELD_NAME" end
        if f.type == nil then return "NO_FIELD_TYPE" end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn lua_schema_get_collection_nonexistent() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        local schema = crap.schema.get_collection("nonexistent")
        if schema == nil then return "ok" end
        return "NOT_NIL"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn lua_schema_get_global() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        local schema = crap.schema.get_global("settings")
        if schema == nil then return "NIL" end
        if schema.slug ~= "settings" then return "WRONG_SLUG:" .. tostring(schema.slug) end
        if schema.fields == nil then return "NO_FIELDS" end
        if #schema.fields == 0 then return "EMPTY_FIELDS" end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn lua_schema_get_global_nonexistent() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        local schema = crap.schema.get_global("nonexistent")
        if schema == nil then return "ok" end
        return "NOT_NIL"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn lua_schema_list_collections() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        local list = crap.schema.list_collections()
        if list == nil then return "NIL" end
        if #list == 0 then return "EMPTY" end
        -- Each entry should have slug and labels
        local found_articles = false
        for _, item in ipairs(list) do
            if item.slug == "articles" then
                found_articles = true
            end
        end
        if not found_articles then return "NO_ARTICLES" end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn lua_schema_list_globals() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        local list = crap.schema.list_globals()
        if list == nil then return "NIL" end
        if #list == 0 then return "EMPTY" end
        local found_settings = false
        for _, item in ipairs(list) do
            if item.slug == "settings" then
                found_settings = true
            end
        end
        if not found_settings then return "NO_SETTINGS" end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn lua_schema_field_options() {
    // Test that schema introspection returns select field options
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();
    std::fs::write(
        collections_dir.join("items.lua"),
        r#"
crap.collections.define("items", {
    fields = {
        { name = "status", type = "select", options = {
            { label = "Active", value = "active" },
            { label = "Inactive", value = "inactive" },
        }},
    },
})
        "#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::test_default();
    let registry = hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(registry)
        .config(&config)
        .build()
        .expect("HookRunner");

    let mut db_config = CrapConfig::test_default();
    db_config.database.path = "test.db".to_string();
    let pool = crap_cms::db::pool::create_pool(tmp.path(), &db_config).expect("pool");
    let conn = pool.get().expect("conn");
    let result = runner.eval_lua_with_conn(r#"
        local schema = crap.schema.get_collection("items")
        if schema == nil then return "NIL" end
        local status_field = schema.fields[1]
        if status_field.name ~= "status" then return "WRONG_FIELD:" .. tostring(status_field.name) end
        if status_field.options == nil then return "NO_OPTIONS" end
        if #status_field.options ~= 2 then return "WRONG_COUNT:" .. tostring(#status_field.options) end
        if status_field.options[1].value ~= "active" then
            return "WRONG_VALUE:" .. tostring(status_field.options[1].value)
        end
        return "ok"
    "#, &conn, None).expect("eval");
    assert_eq!(result, "ok");
}

#[test]
fn lua_schema_field_relationship() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();
    std::fs::write(
        collections_dir.join("posts.lua"),
        r#"
crap.collections.define("posts", {
    fields = {
        { name = "author", type = "relationship", relationship = {
            collection = "users",
            has_many = false,
        }},
        { name = "tags", type = "relationship", relationship = {
            collection = "tags",
            has_many = true,
            max_depth = 2,
        }},
    },
})
        "#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::test_default();
    let registry = hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(registry)
        .config(&config)
        .build()
        .expect("HookRunner");

    let mut db_config = CrapConfig::test_default();
    db_config.database.path = "test.db".to_string();
    let pool = crap_cms::db::pool::create_pool(tmp.path(), &db_config).expect("pool");
    let conn = pool.get().expect("conn");
    let result = runner
        .eval_lua_with_conn(
            r#"
        local schema = crap.schema.get_collection("posts")
        if schema == nil then return "NIL" end
        -- Check author field
        local author = schema.fields[1]
        if author.relationship == nil then return "NO_REL" end
        if author.relationship.collection ~= "users" then
            return "WRONG_COL:" .. tostring(author.relationship.collection)
        end
        if author.relationship.has_many ~= false then return "SHOULD_NOT_HAVE_MANY" end
        -- Check tags field
        local tags = schema.fields[2]
        if tags.relationship == nil then return "NO_TAGS_REL" end
        if tags.relationship.has_many ~= true then return "TAGS_SHOULD_HAVE_MANY" end
        if tags.relationship.max_depth ~= 2 then
            return "WRONG_MAX_DEPTH:" .. tostring(tags.relationship.max_depth)
        end
        return "ok"
    "#,
            &conn,
            None,
        )
        .expect("eval");
    assert_eq!(result, "ok");
}

#[test]
fn lua_schema_blocks_and_subfields() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();
    std::fs::write(
        collections_dir.join("pages.lua"),
        r#"
crap.collections.define("pages", {
    fields = {
        { name = "layout", type = "blocks", blocks = {
            { type = "hero", label = "Hero Section", fields = {
                { name = "heading", type = "text" },
                { name = "image", type = "text" },
            }},
        }},
        { name = "meta", type = "group", fields = {
            { name = "title", type = "text" },
            { name = "desc", type = "textarea" },
        }},
    },
})
        "#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::test_default();
    let registry = hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(registry)
        .config(&config)
        .build()
        .expect("HookRunner");

    let mut db_config = CrapConfig::test_default();
    db_config.database.path = "test.db".to_string();
    let pool = crap_cms::db::pool::create_pool(tmp.path(), &db_config).expect("pool");
    let conn = pool.get().expect("conn");
    let result = runner
        .eval_lua_with_conn(
            r#"
        local schema = crap.schema.get_collection("pages")
        if schema == nil then return "NIL" end
        -- Check blocks field
        local layout = schema.fields[1]
        if layout.type ~= "blocks" then return "NOT_BLOCKS:" .. tostring(layout.type) end
        if layout.blocks == nil then return "NO_BLOCKS" end
        if #layout.blocks ~= 1 then return "WRONG_BLOCK_COUNT:" .. tostring(#layout.blocks) end
        local hero = layout.blocks[1]
        if hero.type ~= "hero" then return "WRONG_BLOCK_TYPE:" .. tostring(hero.type) end
        if hero.label ~= "Hero Section" then return "WRONG_LABEL:" .. tostring(hero.label) end
        if #hero.fields ~= 2 then return "WRONG_FIELD_COUNT:" .. tostring(#hero.fields) end
        -- Check group field sub-fields
        local meta = schema.fields[2]
        if meta.type ~= "group" then return "NOT_GROUP:" .. tostring(meta.type) end
        if meta.fields == nil then return "NO_GROUP_FIELDS" end
        if #meta.fields ~= 2 then return "WRONG_GROUP_FIELDS:" .. tostring(#meta.fields) end
        return "ok"
    "#,
            &conn,
            None,
        )
        .expect("eval");
    assert_eq!(result, "ok");
}

// ── 5A. crap.schema with blocks ───────────────────────────────────────────────

#[test]
fn schema_get_collection_with_blocks() {
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
    )
    .unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::test_default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let runner = crap_cms::hooks::lifecycle::HookRunner::builder()
        .config_dir(tmp.path())
        .registry(registry)
        .config(&config)
        .build()
        .expect("HookRunner::new");

    let tmp2 = tempfile::tempdir().expect("tempdir2");
    let mut db_config = CrapConfig::test_default();
    db_config.database.path = "test.db".to_string();
    let pool = crap_cms::db::pool::create_pool(tmp2.path(), &db_config).expect("pool");
    let conn = pool.get().expect("conn");

    let result = runner.eval_lua_with_conn(r#"
        local def = crap.schema.get_collection("pages")
        if def == nil then return "NIL" end
        -- Find the blocks field
        local blocks_field = nil
        for _, f in ipairs(def.fields) do
            if f.name == "content" then blocks_field = f end
        end
        if blocks_field == nil then return "NO_CONTENT_FIELD" end
        if blocks_field.type ~= "blocks" then return "WRONG_TYPE:" .. blocks_field.type end
        if blocks_field.blocks == nil then return "NO_BLOCKS" end
        if #blocks_field.blocks ~= 2 then return "WRONG_BLOCK_COUNT:" .. tostring(#blocks_field.blocks) end
        if blocks_field.blocks[1].type ~= "text" then return "WRONG_BLOCK_1:" .. blocks_field.blocks[1].type end
        if blocks_field.blocks[1].label ~= "Text Block" then return "WRONG_LABEL_1" end
        if #blocks_field.blocks[1].fields ~= 1 then return "WRONG_FIELDS_1" end
        if blocks_field.blocks[2].type ~= "image" then return "WRONG_BLOCK_2" end
        if #blocks_field.blocks[2].fields ~= 2 then return "WRONG_FIELDS_2" end
        return "ok"
    "#, &conn, None).expect("eval");
    assert_eq!(result, "ok");
}

// ── 5F. crap.schema with sub-fields (array/group) ────────────────────────────

#[test]
fn schema_get_collection_with_array_subfields() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();

    std::fs::write(
        collections_dir.join("posts.lua"),
        r#"
crap.collections.define("posts", {
    fields = {
        { name = "title", type = "text" },
        { name = "tags", type = "array", fields = {
            { name = "label", type = "text", required = true },
            { name = "value", type = "text" },
        }},
    },
})
        "#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::test_default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let runner = crap_cms::hooks::lifecycle::HookRunner::builder()
        .config_dir(tmp.path())
        .registry(registry)
        .config(&config)
        .build()
        .expect("HookRunner::new");

    let tmp2 = tempfile::tempdir().expect("tempdir2");
    let mut db_config = CrapConfig::test_default();
    db_config.database.path = "test.db".to_string();
    let pool = crap_cms::db::pool::create_pool(tmp2.path(), &db_config).expect("pool");
    let conn = pool.get().expect("conn");

    let result = runner.eval_lua_with_conn(r#"
        local def = crap.schema.get_collection("posts")
        if def == nil then return "NIL" end
        local tags_field = nil
        for _, f in ipairs(def.fields) do
            if f.name == "tags" then tags_field = f end
        end
        if tags_field == nil then return "NO_TAGS_FIELD" end
        if tags_field.fields == nil then return "NO_SUB_FIELDS" end
        if #tags_field.fields ~= 2 then return "WRONG_SUB_COUNT:" .. tostring(#tags_field.fields) end
        if tags_field.fields[1].name ~= "label" then return "WRONG_NAME_1" end
        if tags_field.fields[1].required ~= true then return "NOT_REQUIRED" end
        return "ok"
    "#, &conn, None).expect("eval");
    assert_eq!(result, "ok");
}

// ── 5G. crap.schema with relationship fields ─────────────────────────────────

#[test]
fn schema_get_collection_with_relationship() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();

    std::fs::write(
        collections_dir.join("posts.lua"),
        r#"
crap.collections.define("posts", {
    fields = {
        { name = "title", type = "text" },
        { name = "author", type = "relationship", relationship = {
            collection = "users",
            has_many = true,
            max_depth = 2,
        }},
    },
})
        "#,
    )
    .unwrap();
    std::fs::write(
        collections_dir.join("users.lua"),
        r#"
crap.collections.define("users", {
    fields = {
        { name = "name", type = "text" },
    },
})
        "#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::test_default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let runner = crap_cms::hooks::lifecycle::HookRunner::builder()
        .config_dir(tmp.path())
        .registry(registry)
        .config(&config)
        .build()
        .expect("HookRunner::new");

    let tmp2 = tempfile::tempdir().expect("tempdir2");
    let mut db_config = CrapConfig::test_default();
    db_config.database.path = "test.db".to_string();
    let pool = crap_cms::db::pool::create_pool(tmp2.path(), &db_config).expect("pool");
    let conn = pool.get().expect("conn");

    let result = runner
        .eval_lua_with_conn(
            r#"
        local def = crap.schema.get_collection("posts")
        if def == nil then return "NIL" end
        local author_field = nil
        for _, f in ipairs(def.fields) do
            if f.name == "author" then author_field = f end
        end
        if author_field == nil then return "NO_AUTHOR" end
        if author_field.relationship == nil then return "NO_REL" end
        if author_field.relationship.collection ~= "users" then return "WRONG_COLLECTION" end
        if author_field.relationship.has_many ~= true then return "NOT_HAS_MANY" end
        if author_field.relationship.max_depth ~= 2 then return "WRONG_MAX_DEPTH" end
        return "ok"
    "#,
            &conn,
            None,
        )
        .expect("eval");
    assert_eq!(result, "ok");
}

// ── 5H. crap.schema with select options ──────────────────────────────────────

#[test]
fn schema_get_collection_with_options() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();

    std::fs::write(
        collections_dir.join("posts.lua"),
        r#"
crap.collections.define("posts", {
    fields = {
        { name = "status", type = "select", options = {
            { label = "Draft", value = "draft" },
            { label = "Published", value = "published" },
        }},
    },
})
        "#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::test_default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let runner = crap_cms::hooks::lifecycle::HookRunner::builder()
        .config_dir(tmp.path())
        .registry(registry)
        .config(&config)
        .build()
        .expect("HookRunner::new");

    let tmp2 = tempfile::tempdir().expect("tempdir2");
    let mut db_config = CrapConfig::test_default();
    db_config.database.path = "test.db".to_string();
    let pool = crap_cms::db::pool::create_pool(tmp2.path(), &db_config).expect("pool");
    let conn = pool.get().expect("conn");

    let result = runner
        .eval_lua_with_conn(
            r#"
        local def = crap.schema.get_collection("posts")
        local status_field = nil
        for _, f in ipairs(def.fields) do
            if f.name == "status" then status_field = f end
        end
        if status_field == nil then return "NO_STATUS" end
        if status_field.options == nil then return "NO_OPTIONS" end
        if #status_field.options ~= 2 then return "WRONG_OPTION_COUNT" end
        if status_field.options[1].value ~= "draft" then return "WRONG_VALUE_1" end
        if status_field.options[1].label ~= "Draft" then return "WRONG_LABEL_1" end
        return "ok"
    "#,
            &conn,
            None,
        )
        .expect("eval");
    assert_eq!(result, "ok");
}

#[test]
fn parse_richtext_format_json() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();

    std::fs::write(
        collections_dir.join("pages.lua"),
        r#"
crap.collections.define("pages", {
    fields = {
        { name = "content", type = "richtext", admin = { format = "json" } },
    },
})
        "#,
    )
    .unwrap();

    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::test_default();
    let registry = hooks::init_lua(tmp.path(), &config).expect("init_lua failed");

    let reg = registry.read().unwrap();
    let def = reg
        .get_collection("pages")
        .expect("pages should be registered");
    let field = def.fields.iter().find(|f| f.name == "content").unwrap();
    assert_eq!(field.admin.richtext_format.as_deref(), Some("json"));
}

#[test]
fn parse_richtext_format_absent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();

    std::fs::write(
        collections_dir.join("pages.lua"),
        r#"
crap.collections.define("pages", {
    fields = {
        { name = "content", type = "richtext" },
    },
})
        "#,
    )
    .unwrap();

    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::test_default();
    let registry = hooks::init_lua(tmp.path(), &config).expect("init_lua failed");

    let reg = registry.read().unwrap();
    let def = reg
        .get_collection("pages")
        .expect("pages should be registered");
    let field = def.fields.iter().find(|f| f.name == "content").unwrap();
    assert!(field.admin.richtext_format.is_none());
}
