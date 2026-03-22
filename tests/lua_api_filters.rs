use std::path::PathBuf;

use crap_cms::config::CrapConfig;
use crap_cms::core::SharedRegistry;
use crap_cms::db::DbPool;
use crap_cms::hooks;
use crap_cms::hooks::lifecycle::{HookRunner, ValidationCtx};
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

// ── Date normalization integration tests ────────────────────────────────────

#[test]
fn date_field_normalizes_date_only_to_utc_noon() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        local doc = crap.collections.create("articles", {
            title = "Date Test 1",
            published_at = "2026-03-15",
        })
        local found = crap.collections.find_by_id("articles", doc.id)
        return found.published_at
    "#,
    );
    assert_eq!(result, "2026-03-15T12:00:00.000Z");
}

#[test]
fn date_field_normalizes_full_datetime() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        local doc = crap.collections.create("articles", {
            title = "Date Test 2",
            event_at = "2026-03-15T09:00:00Z",
        })
        local found = crap.collections.find_by_id("articles", doc.id)
        return found.event_at
    "#,
    );
    assert_eq!(result, "2026-03-15T09:00:00.000Z");
}

// ── crap.collections.config.get() ────────────────────────────────────────────────

#[test]
fn collections_config_get_returns_nil_for_unknown() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local def = crap.collections.config.get("nonexistent")
        return tostring(def)
    "#,
    );
    assert_eq!(result, "nil");
}

#[test]
fn collections_config_get_returns_labels_and_fields() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local def = crap.collections.config.get("articles")
        if def == nil then return "nil" end
        local parts = {}
        parts[#parts + 1] = def.labels.singular
        parts[#parts + 1] = def.labels.plural
        parts[#parts + 1] = tostring(#def.fields)
        return table.concat(parts, "|")
    "#,
    );
    let parts: Vec<&str> = result.split('|').collect();
    assert_eq!(parts[0], "Article");
    assert_eq!(parts[1], "Articles");
    // articles has 7 fields: title, body, status, slug, word_count, published_at, event_at
    assert_eq!(parts[2], "7");
}

#[test]
fn collections_config_get_includes_field_details() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local def = crap.collections.config.get("articles")
        local title = def.fields[1]
        local parts = {}
        parts[#parts + 1] = title.name
        parts[#parts + 1] = title.type
        parts[#parts + 1] = tostring(title.required)
        parts[#parts + 1] = tostring(title.unique)
        return table.concat(parts, "|")
    "#,
    );
    assert_eq!(result, "title|text|true|true");
}

#[test]
fn collections_config_get_includes_hooks() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local def = crap.collections.config.get("articles")
        return def.hooks.before_validate[1]
    "#,
    );
    assert_eq!(result, "hooks.article_hooks.before_validate");
}

#[test]
fn collections_config_get_includes_field_hooks() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local def = crap.collections.config.get("articles")
        -- slug is field 4
        local slug = def.fields[4]
        return slug.hooks.before_change[1]
    "#,
    );
    assert_eq!(result, "hooks.field_hooks.slugify_title");
}

#[test]
fn collections_config_get_includes_select_options() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local def = crap.collections.config.get("articles")
        -- status is field 3
        local status = def.fields[3]
        local parts = {}
        for _, opt in ipairs(status.options) do
            parts[#parts + 1] = opt.label .. "=" .. opt.value
        end
        return table.concat(parts, "|")
    "#,
    );
    assert_eq!(
        result,
        "Draft=draft|Published=published|Archived=archived|Active=active|Red=red|Blue=blue|Green=green|True=true|False=false"
    );
}

#[test]
fn collections_config_get_includes_picker_appearance() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local def = crap.collections.config.get("articles")
        -- event_at is field 7, has picker_appearance = "dayAndTime"
        local event_at = def.fields[7]
        return event_at.picker_appearance
    "#,
    );
    assert_eq!(result, "dayAndTime");
}

#[test]
fn collections_config_get_roundtrip_redefine() {
    let runner = setup_lua();
    // Get the definition, modify it, redefine, and get again to verify round-trip
    let result = eval_lua(
        &runner,
        r#"
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
    "#,
    );
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
    let result = eval_lua(
        &runner,
        r#"
        return tostring(crap.globals.config.get("nonexistent"))
    "#,
    );
    assert_eq!(result, "nil");
}

#[test]
fn globals_config_get_returns_labels_and_fields() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local def = crap.globals.config.get("settings")
        local parts = {}
        parts[#parts + 1] = def.labels.singular
        parts[#parts + 1] = tostring(#def.fields)
        parts[#parts + 1] = def.fields[1].name
        return table.concat(parts, "|")
    "#,
    );
    assert_eq!(result, "Settings|2|site_name");
}

#[test]
fn globals_config_get_roundtrip_redefine() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local def = crap.globals.config.get("settings")
        def.fields[#def.fields + 1] = { name = "footer_text", type = "text" }
        crap.globals.define("settings", def)
        local def2 = crap.globals.config.get("settings")
        local parts = {}
        parts[#parts + 1] = tostring(#def2.fields)
        parts[#parts + 1] = def2.fields[#def2.fields].name
        parts[#parts + 1] = def2.labels.singular
        return table.concat(parts, "|")
    "#,
    );
    assert_eq!(result, "3|footer_text|Settings");
}

#[test]
fn globals_list_returns_slug_keyed_map() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local all = crap.globals.config.list()
        local slugs = {}
        for slug, _ in pairs(all) do
            slugs[#slugs + 1] = slug
        end
        table.sort(slugs)
        return table.concat(slugs, ",")
    "#,
    );
    assert!(
        result.contains("settings"),
        "should contain settings, got: {}",
        result
    );
}

#[test]
fn globals_list_can_modify_and_redefine() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        for slug, def in pairs(crap.globals.config.list()) do
            if slug == "settings" then
                def.fields[#def.fields + 1] = { name = "plugin_field", type = "text" }
                crap.globals.define(slug, def)
            end
        end
        local updated = crap.globals.config.get("settings")
        return updated.fields[#updated.fields].name
    "#,
    );
    assert_eq!(result, "plugin_field");
}

#[test]
fn collections_list_returns_all_collections() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local all = crap.collections.config.list()
        local slugs = {}
        for slug, _ in pairs(all) do
            slugs[#slugs + 1] = slug
        end
        table.sort(slugs)
        return table.concat(slugs, ",")
    "#,
    );
    assert!(
        result.contains("articles"),
        "should contain articles, got: {}",
        result
    );
}

#[test]
fn collections_list_can_filter_and_redefine() {
    let runner = setup_lua();
    // Simulate a plugin that adds a field to every collection
    let result = eval_lua(
        &runner,
        r#"
        for slug, def in pairs(crap.collections.config.list()) do
            if slug == "articles" then
                def.fields[#def.fields + 1] = { name = "plugin_field", type = "text" }
                crap.collections.define(slug, def)
            end
        end
        local updated = crap.collections.config.get("articles")
        return updated.fields[#updated.fields].name
    "#,
    );
    assert_eq!(result, "plugin_field");
}

// ── Dot-notation filter e2e tests ────────────────────────────────────────────

#[test]
fn lua_find_dot_notation_where() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        -- Create Product 1: "Widget" with red variant, text block
        crap.collections.create("products", {
            name = "Widget",
            seo = { meta_title = "Buy Widget" },
            variants = {
                { color = "red", dimensions = { width = "10", height = "20" } },
            },
            content = {
                { _block_type = "text", body = "Widget description here" },
            },
        })

        -- Create Product 2: "Gadget" with blue variant, section block
        crap.collections.create("products", {
            name = "Gadget",
            seo = { meta_title = "Buy Gadget" },
            variants = {
                { color = "blue", dimensions = { width = "5", height = "15" } },
            },
            content = {
                { _block_type = "section", heading = "About Gadget", meta = { author = "Alice" } },
            },
        })

        -- 1. Group sub-field: seo.meta_title contains "Widget"
        local r1 = crap.collections.find("products", {
            where = { ["seo.meta_title"] = { contains = "Widget" } },
        })
        if r1.pagination.totalDocs ~= 1 then return "GROUP:WRONG_TOTAL:" .. tostring(r1.pagination.totalDocs) end
        if r1.documents[1].name ~= "Widget" then return "GROUP:WRONG_NAME:" .. r1.documents[1].name end

        -- 2. Array sub-field: variants.color = "red"
        local r2 = crap.collections.find("products", {
            where = { ["variants.color"] = "red" },
        })
        if r2.pagination.totalDocs ~= 1 then return "ARRAY:WRONG_TOTAL:" .. tostring(r2.pagination.totalDocs) end
        if r2.documents[1].name ~= "Widget" then return "ARRAY:WRONG_NAME:" .. r2.documents[1].name end

        -- 3. Group-in-array: variants.dimensions.width = "10"
        local r3 = crap.collections.find("products", {
            where = { ["variants.dimensions.width"] = "10" },
        })
        if r3.pagination.totalDocs ~= 1 then return "GIA:WRONG_TOTAL:" .. tostring(r3.pagination.totalDocs) end
        if r3.documents[1].name ~= "Widget" then return "GIA:WRONG_NAME:" .. r3.documents[1].name end

        -- 4. Block sub-field: content.body contains "description"
        local r4 = crap.collections.find("products", {
            where = { ["content.body"] = { contains = "description" } },
        })
        if r4.pagination.totalDocs ~= 1 then return "BLOCK:WRONG_TOTAL:" .. tostring(r4.pagination.totalDocs) end
        if r4.documents[1].name ~= "Widget" then return "BLOCK:WRONG_NAME:" .. r4.documents[1].name end

        -- 5. Block type: content._block_type = "section"
        local r5 = crap.collections.find("products", {
            where = { ["content._block_type"] = "section" },
        })
        if r5.pagination.totalDocs ~= 1 then return "BTYPE:WRONG_TOTAL:" .. tostring(r5.pagination.totalDocs) end
        if r5.documents[1].name ~= "Gadget" then return "BTYPE:WRONG_NAME:" .. r5.documents[1].name end

        -- 6. Group-in-block: content.meta.author = "Alice"
        local r6 = crap.collections.find("products", {
            where = { ["content.meta.author"] = "Alice" },
        })
        if r6.pagination.totalDocs ~= 1 then return "GIB:WRONG_TOTAL:" .. tostring(r6.pagination.totalDocs) end
        if r6.documents[1].name ~= "Gadget" then return "GIB:WRONG_NAME:" .. r6.documents[1].name end

        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

// ══════════════════════════════════════════════════════════════════════════════
// E2E COVERAGE: Filter Operators, Unique Constraints, Validators, Locale, Drafts
// ══════════════════════════════════════════════════════════════════════════════

// ── Group 1: Filter Operators (Lua) ──────────────────────────────────────────

#[test]
fn lua_find_filter_operators() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        -- Seed data
        crap.collections.create("articles", { title = "Alpha", body = "10", status = "red" })
        crap.collections.create("articles", { title = "Beta", body = "20", status = "blue" })
        crap.collections.create("articles", { title = "Gamma", body = "30", status = "red" })
        crap.collections.create("articles", { title = "Delta", body = "40", status = "" })
        crap.collections.create("articles", { title = "Epsilon", body = "50", status = "green" })

        -- not_equals
        local r1 = crap.collections.find("articles", {
            where = { status = { not_equals = "red" } },
        })
        -- Delta has status="" stored as NULL — SQL NULL != 'red' is NULL (not true)
        if r1.pagination.totalDocs ~= 3 and r1.pagination.totalDocs ~= 2 then return "NE:" .. tostring(r1.pagination.totalDocs) end

        -- greater_than (body is stored as text, but numeric comparison should work)
        local r2 = crap.collections.find("articles", {
            where = { body = { greater_than = "30" } },
        })
        if r2.pagination.totalDocs ~= 2 then return "GT:" .. tostring(r2.pagination.totalDocs) end

        -- less_than
        local r3 = crap.collections.find("articles", {
            where = { body = { less_than = "20" } },
        })
        if r3.pagination.totalDocs ~= 1 then return "LT:" .. tostring(r3.pagination.totalDocs) end

        -- greater_than_or_equal
        local r4 = crap.collections.find("articles", {
            where = { body = { greater_than_or_equal = "30" } },
        })
        if r4.pagination.totalDocs ~= 3 then return "GTE:" .. tostring(r4.pagination.totalDocs) end

        -- less_than_or_equal
        local r5 = crap.collections.find("articles", {
            where = { body = { less_than_or_equal = "20" } },
        })
        if r5.pagination.totalDocs ~= 2 then return "LTE:" .. tostring(r5.pagination.totalDocs) end

        -- in
        local r6 = crap.collections.find("articles", {
            where = { status = { ["in"] = { "red", "green" } } },
        })
        if r6.pagination.totalDocs ~= 3 then return "IN:" .. tostring(r6.pagination.totalDocs) end

        -- not_in
        local r7 = crap.collections.find("articles", {
            where = { status = { not_in = { "red", "green" } } },
        })
        -- Delta has status="" stored as NULL — SQL NOT IN excludes NULLs
        if r7.pagination.totalDocs ~= 2 and r7.pagination.totalDocs ~= 1 then return "NIN:" .. tostring(r7.pagination.totalDocs) end

        -- like
        local r8 = crap.collections.find("articles", {
            where = { title = { like = "%lph%" } },
        })
        if r8.pagination.totalDocs ~= 1 then return "LIKE:" .. tostring(r8.pagination.totalDocs) end

        -- contains
        local r9 = crap.collections.find("articles", {
            where = { title = { contains = "eta" } },
        })
        if r9.pagination.totalDocs ~= 1 then return "CONTAINS:" .. tostring(r9.pagination.totalDocs) end

        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

// ── Group 2: Unique Constraints (Lua) ────────────────────────────────────────

#[test]
fn lua_create_unique_constraint_violation() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let conn = pool.get().expect("conn");
    let result = runner
        .eval_lua_with_conn(
            r#"
        -- title is unique in the fixture articles collection
        crap.collections.create("articles", { title = "Unique Title", body = "First" })

        local ok, err = pcall(function()
            crap.collections.create("articles", { title = "Unique Title", body = "Second" })
        end)
        if ok then return "SHOULD_HAVE_FAILED" end
        local err_str = tostring(err)
        if err_str:find("unique") or err_str:find("UNIQUE") or err_str:find("duplicate")
            or err_str:find("Failed to insert") then
            return "ok"
        end
        return "UNEXPECTED:" .. err_str
    "#,
            &conn,
            None,
        )
        .expect("eval");
    assert_eq!(result, "ok");
}

// ── Group 3: Custom Validators (Lua) ─────────────────────────────────────────
// Note: Lua CRUD functions (crap.collections.create) do NOT run the full
// hook/validate lifecycle — validators only fire through the gRPC/admin
// path via run_before_write. This test verifies field-level validators
// work through the HookRunner.validate_fields method directly.

#[test]
fn lua_validate_fields_with_custom_validator() {
    let (_tmp, pool, registry, runner) = setup_with_db();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    // Valid: positive number should pass
    let mut conn = pool.get().unwrap();
    let tx = conn.transaction().unwrap();

    let mut data = std::collections::HashMap::new();
    data.insert("title".to_string(), json!("Valid Article"));
    data.insert("word_count".to_string(), json!("100"));

    let result = runner.validate_fields(
        &def.fields,
        &data,
        &ValidationCtx::builder(&tx, "articles").build(),
    );
    assert!(
        result.is_ok(),
        "Valid positive number should pass validation"
    );

    // Invalid: negative number should fail
    let mut bad_data = std::collections::HashMap::new();
    bad_data.insert("title".to_string(), json!("Invalid Article"));
    bad_data.insert("word_count".to_string(), json!("-5"));

    let result = runner.validate_fields(
        &def.fields,
        &bad_data,
        &ValidationCtx::builder(&tx, "articles").build(),
    );
    assert!(result.is_err(), "Negative number should fail validation");
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("positive") || err_msg.contains("Must be"),
        "Error should mention positive, got: {}",
        err_msg
    );
}

// ── Group 6: Localization (Lua) ──────────────────────────────────────────────

fn setup_localized_lua_db(
    locales: Vec<&str>,
) -> (
    tempfile::TempDir,
    crap_cms::db::DbPool,
    SharedRegistry,
    HookRunner,
) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();

    std::fs::write(
        collections_dir.join("posts.lua"),
        r#"
crap.collections.define("posts", {
    labels = { singular = "Post", plural = "Posts" },
    fields = {
        { name = "title", type = "text", required = true, localized = true },
        { name = "body", type = "textarea" },
    },
})
        "#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let mut config = CrapConfig::default();
    config.locale.locales = locales.iter().map(|s| s.to_string()).collect();
    config.locale.default_locale = locales.first().unwrap_or(&"en").to_string();
    config.locale.fallback = true;

    let registry = hooks::init_lua(tmp.path(), &config).expect("init_lua");

    let mut db_config = config.clone();
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

#[test]
fn lua_crud_with_locale() {
    let (_tmp, pool, _reg, runner) = setup_localized_lua_db(vec!["en", "de"]);
    let conn = pool.get().expect("conn");
    let result = runner
        .eval_lua_with_conn(
            r#"
        -- Create with English locale
        local doc = crap.collections.create("posts", {
            title = "Hello",
            body = "English body",
        }, { locale = "en" })

        if doc == nil then return "CREATE_NIL" end

        -- Find with English locale
        local result = crap.collections.find("posts", { locale = "en" })
        if result.pagination.totalDocs ~= 1 then return "TOTAL:" .. tostring(result.pagination.totalDocs) end
        if result.documents[1].title ~= "Hello" then
            return "TITLE:" .. tostring(result.documents[1].title)
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
fn lua_crud_with_locale_fallback() {
    let (_tmp, pool, _reg, runner) = setup_localized_lua_db(vec!["en", "de"]);
    let conn = pool.get().expect("conn");
    let result = runner
        .eval_lua_with_conn(
            r#"
        -- Create English only
        crap.collections.create("posts", {
            title = "English Only",
        }, { locale = "en" })

        -- Find with German locale should fallback to English
        local result = crap.collections.find("posts", { locale = "de" })
        if result.pagination.totalDocs ~= 1 then return "TOTAL:" .. tostring(result.pagination.totalDocs) end
        if result.documents[1].title ~= "English Only" then
            return "TITLE:" .. tostring(result.documents[1].title)
        end
        return "ok"
    "#,
            &conn,
            None,
        )
        .expect("eval");
    assert_eq!(result, "ok");
}

// ── Group 7: Drafts (Lua) — find drafts only ────────────────────────────────

#[test]
fn lua_find_drafts_only() {
    let (_tmp, pool, _reg, runner) = setup_versioned_db();
    // Lua find() now auto-filters to _status="published" by default (matches gRPC).
    // Use draft=true to include all documents.
    let result = eval_versioned(
        &runner,
        &pool,
        r#"
        -- Create a published doc
        crap.collections.create("articles", {
            title = "Published",
            body = "Pub body",
        })

        -- Create a draft doc
        crap.collections.create("articles", {
            title = "Draft Only",
            body = "Draft body",
        }, { draft = true })

        -- Default find: only published (matches gRPC default)
        local published = crap.collections.find("articles", {})
        if published.pagination.totalDocs ~= 1 then
            return "DEFAULT_TOTAL:" .. tostring(published.pagination.totalDocs)
        end
        if published.documents[1].title ~= "Published" then
            return "DEFAULT_TITLE:" .. tostring(published.documents[1].title)
        end

        -- find with draft=true: returns ALL docs (both published and draft)
        local all = crap.collections.find("articles", { draft = true })
        if all.pagination.totalDocs ~= 2 then
            return "DRAFT_ALL_TOTAL:" .. tostring(all.pagination.totalDocs)
        end

        -- Can still filter by _status explicitly within draft=true
        local drafts = crap.collections.find("articles", {
            draft = true,
            where = { _status = "draft" },
        })
        if drafts.pagination.totalDocs ~= 1 then
            return "DRAFT_ONLY_TOTAL:" .. tostring(drafts.pagination.totalDocs)
        end
        if drafts.documents[1].title ~= "Draft Only" then
            return "DRAFT_TITLE:" .. tostring(drafts.documents[1].title)
        end

        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn lua_count_respects_draft_filtering() {
    let (_tmp, pool, _reg, runner) = setup_versioned_db();
    let result = eval_versioned(
        &runner,
        &pool,
        r#"
        crap.collections.create("articles", {
            title = "Published",
            body = "Pub body",
        })
        crap.collections.create("articles", {
            title = "Draft",
            body = "Draft body",
        }, { draft = true })

        -- count() default: only published
        local pub_count = crap.collections.count("articles", {})
        if pub_count ~= 1 then
            return "DEFAULT_COUNT:" .. tostring(pub_count)
        end

        -- count() with draft=true: all docs
        local all_count = crap.collections.count("articles", { draft = true })
        if all_count ~= 2 then
            return "DRAFT_COUNT:" .. tostring(all_count)
        end

        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

// ── Lua CRUD Lifecycle Parity Tests ─────────────────────────────────────────
// These tests verify that Lua CRUD operations run the same lifecycle hooks
// as gRPC/Admin: before_validate, before_change, after_change, validation,
// after_read, before_delete, after_delete.

#[test]
fn lua_create_runs_before_change_hook() {
    // The articles before_change hook sets default status to "draft" when empty,
    // and the field-level before_change on slug generates a slug from title.
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        local doc = crap.collections.create("articles", {
            title = "My Test Article",
        })
        -- before_change hook should have set status to "draft"
        if doc.status ~= "draft" then
            return "WRONG_STATUS:" .. tostring(doc.status)
        end
        -- field-level before_change should have generated slug from title
        if doc.slug ~= "my-test-article" then
            return "WRONG_SLUG:" .. tostring(doc.slug)
        end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn lua_create_runs_before_validate_hook() {
    // The articles before_validate hook trims title whitespace.
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        local doc = crap.collections.create("articles", {
            title = "  Padded Title  ",
            body = "some body",
        })
        if doc.title ~= "Padded Title" then
            return "NOT_TRIMMED:" .. tostring(doc.title)
        end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn lua_create_runs_after_change_hook() {
    // Set up a custom fixture with an after_change hook that creates a side-effect doc.
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();
    let hooks_dir = tmp.path().join("hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();

    std::fs::write(
        collections_dir.join("notes.lua"),
        r#"
crap.collections.define("notes", {
    fields = {
        { name = "title", type = "text", required = true },
        { name = "source", type = "text" },
    },
    hooks = {
        after_change = { "hooks.note_hooks.after_change" },
    },
})
        "#,
    )
    .unwrap();
    std::fs::write(
        collections_dir.join("audit.lua"),
        r#"
crap.collections.define("audit", {
    fields = {
        { name = "action", type = "text" },
        { name = "ref_id", type = "text" },
    },
})
        "#,
    )
    .unwrap();
    std::fs::write(
        hooks_dir.join("note_hooks.lua"),
        r#"
local M = {}
function M.after_change(ctx)
    crap.collections.create("audit", {
        action = ctx.operation,
        ref_id = ctx.data.id or "unknown",
    })
    return ctx
end
return M
        "#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();
    std::fs::write(tmp.path().join("crap.toml"), "").unwrap();

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
        local doc = crap.collections.create("notes", { title = "Test Note" })
        -- after_change hook should have created an audit doc
        local audits = crap.collections.find("audit", {})
        if audits.pagination.totalDocs ~= 1 then
            return "NO_AUDIT:" .. tostring(audits.pagination.totalDocs)
        end
        if audits.documents[1].action ~= "create" then
            return "WRONG_ACTION:" .. tostring(audits.documents[1].action)
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
fn lua_create_runs_validation() {
    // The articles collection has a custom validator on word_count (positive_number).
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let conn = pool.get().expect("conn");
    let result = runner
        .eval_lua_with_conn(
            r#"
        local ok, err = pcall(function()
            crap.collections.create("articles", {
                title = "Bad Article",
                word_count = "-5",
            })
        end)
        if ok then return "SHOULD_HAVE_FAILED" end
        local err_str = tostring(err)
        if err_str:find("positive") then return "ok" end
        return "WRONG_ERROR:" .. err_str
    "#,
            &conn,
            None,
        )
        .expect("eval");
    assert_eq!(result, "ok");
}

#[test]
fn lua_create_with_hooks_false() {
    // When hooks = false, before_change hook should NOT fire (no default status).
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        local doc = crap.collections.create("articles", {
            title = "No Hooks Article",
        }, { hooks = false })
        -- Without before_change hook, status should be empty/nil
        if doc.status ~= nil and doc.status ~= "" then
            return "HOOKS_RAN:" .. tostring(doc.status)
        end
        -- slug should NOT be generated (field-level hook skipped)
        if doc.slug ~= nil and doc.slug ~= "" then
            return "FIELD_HOOK_RAN:" .. tostring(doc.slug)
        end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn lua_delete_runs_hooks() {
    // Set up a fixture with after_delete hooks that log the deletion.
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();
    let hooks_dir = tmp.path().join("hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();

    std::fs::write(
        collections_dir.join("items.lua"),
        r#"
crap.collections.define("items", {
    fields = {
        { name = "name", type = "text", required = true },
    },
    hooks = {
        after_delete = { "hooks.item_hooks.after_delete" },
    },
})
        "#,
    )
    .unwrap();
    std::fs::write(
        collections_dir.join("deletelog.lua"),
        r#"
crap.collections.define("deletelog", {
    fields = {
        { name = "deleted_id", type = "text" },
    },
})
        "#,
    )
    .unwrap();
    std::fs::write(
        hooks_dir.join("item_hooks.lua"),
        r#"
local M = {}
function M.after_delete(ctx)
    crap.collections.create("deletelog", {
        deleted_id = ctx.data.id or "unknown",
    })
    return ctx
end
return M
        "#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();
    std::fs::write(tmp.path().join("crap.toml"), "").unwrap();

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
        local doc = crap.collections.create("items", { name = "To Delete" })
        local id = doc.id
        crap.collections.delete("items", id)

        -- after_delete hook should have logged the deletion
        local logs = crap.collections.find("deletelog", {})
        if logs.pagination.totalDocs ~= 1 then
            return "NO_LOG:" .. tostring(logs.pagination.totalDocs)
        end
        if logs.documents[1].deleted_id ~= id then
            return "WRONG_ID:" .. tostring(logs.documents[1].deleted_id) .. " expected:" .. id
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
fn lua_find_runs_after_read_hook() {
    // The articles after_read hook sets _was_read = "true" on each document.
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        crap.collections.create("articles", {
            title = "Readable Article",
            body = "Content",
        }, { hooks = false })

        -- find should run after_read hooks
        local result = crap.collections.find("articles", {})
        local doc = result.documents[1]
        if doc._was_read ~= "true" then
            return "NO_AFTER_READ_ON_FIND:" .. tostring(doc._was_read)
        end

        -- find_by_id should also run after_read hooks
        local found = crap.collections.find_by_id("articles", doc.id)
        if found._was_read ~= "true" then
            return "NO_AFTER_READ_ON_FIND_BY_ID:" .. tostring(found._was_read)
        end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn lua_hook_depth_exposed_in_context() {
    // Set up a fixture where a hook reads ctx.hook_depth.
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();

    std::fs::write(
        collections_dir.join("depthtest.lua"),
        r#"
crap.collections.define("depthtest", {
    fields = {
        { name = "name", type = "text", required = true },
        { name = "depth_seen", type = "text" },
    },
    hooks = {
        before_change = { "hooks.depth_hooks.record_depth" },
    },
})
        "#,
    )
    .unwrap();

    let hooks_dir = tmp.path().join("hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();
    std::fs::write(
        hooks_dir.join("depth_hooks.lua"),
        r#"
local M = {}
function M.record_depth(ctx)
    ctx.data.depth_seen = tostring(ctx.hook_depth or "nil")
    return ctx
end
return M
        "#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();
    std::fs::write(tmp.path().join("crap.toml"), "").unwrap();

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
        local doc = crap.collections.create("depthtest", { name = "test" })
        -- At Lua CRUD level, hook_depth should be 1 (incremented from 0)
        if doc.depth_seen ~= "1" then
            return "WRONG_DEPTH:" .. tostring(doc.depth_seen)
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
fn lua_hook_recursion_capped() {
    // Set up a fixture where a hook creates another document in the same collection,
    // which would trigger an infinite loop without depth capping.
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();

    std::fs::write(
        collections_dir.join("recursive.lua"),
        r#"
crap.collections.define("recursive", {
    fields = {
        { name = "name", type = "text", required = true },
        { name = "level", type = "text" },
    },
    hooks = {
        after_change = { "hooks.recursive_hooks.spawn" },
    },
})
        "#,
    )
    .unwrap();

    let hooks_dir = tmp.path().join("hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();
    std::fs::write(
        hooks_dir.join("recursive_hooks.lua"),
        r#"
local M = {}
function M.spawn(ctx)
    -- The depth system should cap this automatically
    local depth = ctx.hook_depth or 0
    crap.collections.create("recursive", {
        name = "spawned-at-depth-" .. tostring(depth),
        level = tostring(depth),
    })
    return ctx
end
return M
        "#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();
    std::fs::write(tmp.path().join("crap.toml"), "[hooks]\nmax_depth = 2\n").unwrap();

    let mut config = CrapConfig::default();
    config.hooks.max_depth = 2;
    let registry = hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let mut db_config = config.clone();
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
        crap.collections.create("recursive", { name = "root" })
        -- With max_depth=2: root creates at depth 0, hook fires at depth 1,
        -- which creates another doc, hook fires at depth 2 which creates
        -- another doc but hooks are skipped (depth >= max), so it stops.
        local result = crap.collections.find("recursive", {})
        -- The key thing is: this doesn't crash with infinite recursion
        if result.pagination.totalDocs < 2 then
            return "TOO_FEW:" .. tostring(result.pagination.totalDocs)
        end
        return "ok"
    "#,
            &conn,
            None,
        )
        .expect("eval");
    assert_eq!(result, "ok");
}
