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

// ── Helper: setup with real DB tables ────────────────────────────────────────

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

// ── Lua CRUD Functions ───────────────────────────────────────────────────────

#[test]
fn lua_crud_create_and_find() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        local doc = crap.collections.create("articles", {
            title = "Test Article",
            body = "Some content here",
        })
        if doc.id == nil then return "NO_ID" end

        local result = crap.collections.find("articles", {})
        if result.pagination.totalDocs ~= 1 then
            return "WRONG_TOTAL:" .. tostring(result.pagination.totalDocs)
        end
        local found = result.documents[1]
        -- after_read field hook uppercases title
        if found.title ~= "TEST ARTICLE" then
            return "WRONG_TITLE:" .. tostring(found.title)
        end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn lua_crud_find_by_id() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        local doc = crap.collections.create("articles", {
            title = "Find Me By ID",
            body = "Body text",
        })
        local id = doc.id

        local found = crap.collections.find_by_id("articles", id)
        if found == nil then return "NOT_FOUND" end
        -- after_read field hook uppercases title
        if found.title ~= "FIND ME BY ID" then
            return "WRONG_TITLE:" .. tostring(found.title)
        end
        if found.body ~= "Body text" then
            return "WRONG_BODY:" .. tostring(found.body)
        end
        if found.id ~= id then
            return "WRONG_ID:" .. tostring(found.id)
        end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn lua_crud_update() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        local doc = crap.collections.create("articles", {
            title = "Original Title",
            body = "Original body",
        })
        local id = doc.id

        local updated = crap.collections.update("articles", id, {
            title = "Updated Title",
        })
        -- update response does NOT run after_read hooks, so title is original case
        if updated.title ~= "Updated Title" then
            return "UPDATE_FAILED:" .. tostring(updated.title)
        end

        -- Verify via find_by_id (after_read field hook uppercases title)
        local found = crap.collections.find_by_id("articles", id)
        if found.title ~= "UPDATED TITLE" then
            return "FIND_AFTER_UPDATE_FAILED:" .. tostring(found.title)
        end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn lua_crud_delete() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        local doc = crap.collections.create("articles", {
            title = "To Be Deleted",
            body = "Goodbye",
        })
        local id = doc.id

        crap.collections.delete("articles", id)

        local result = crap.collections.find("articles", {})
        if result.pagination.totalDocs ~= 0 then
            return "NOT_DELETED:total=" .. tostring(result.pagination.totalDocs)
        end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn lua_crud_find_pagination_fields() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        for i = 1, 5 do
            crap.collections.create("articles", {
                title = "Article " .. i,
                body = "Body " .. i,
            })
        end

        -- Page 1, limit 2 → page 1 of 3
        local r = crap.collections.find("articles", { limit = 2, page = 1 })
        local p = r.pagination
        if p.totalDocs ~= 5 then return "TOTAL:" .. tostring(p.totalDocs) end
        if p.limit ~= 2 then return "LIMIT:" .. tostring(p.limit) end
        if p.totalPages ~= 3 then return "TOTAL_PAGES:" .. tostring(p.totalPages) end
        if p.page ~= 1 then return "PAGE:" .. tostring(p.page) end
        if p.hasNextPage ~= true then return "HAS_NEXT:false" end
        if p.hasPrevPage ~= false then return "HAS_PREV:true" end
        if p.nextPage ~= 2 then return "NEXT_PAGE:" .. tostring(p.nextPage) end
        if p.prevPage ~= nil then return "PREV_PAGE:" .. tostring(p.prevPage) end
        if #r.documents ~= 2 then return "DOCS:" .. tostring(#r.documents) end

        -- Page 3 (last) → has prev, no next
        local r2 = crap.collections.find("articles", { limit = 2, page = 3 })
        local p2 = r2.pagination
        if p2.page ~= 3 then return "P2_PAGE:" .. tostring(p2.page) end
        if p2.hasPrevPage ~= true then return "P2_HAS_PREV:false" end
        if p2.hasNextPage ~= false then return "P2_HAS_NEXT:true" end
        if p2.prevPage ~= 2 then return "P2_PREV:" .. tostring(p2.prevPage) end
        if p2.nextPage ~= nil then return "P2_NEXT:" .. tostring(p2.nextPage) end
        if #r2.documents ~= 1 then return "P2_DOCS:" .. tostring(#r2.documents) end

        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn lua_crud_find_with_where() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
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
            where = { status = "published" },
        })
        if result.pagination.totalDocs ~= 2 then
            return "WRONG_TOTAL:" .. tostring(result.pagination.totalDocs)
        end

        -- Filter by status = draft
        local drafts = crap.collections.find("articles", {
            where = { status = "draft" },
        })
        if drafts.pagination.totalDocs ~= 1 then
            return "WRONG_DRAFT_TOTAL:" .. tostring(drafts.pagination.totalDocs)
        end
        -- after_read field hook uppercases title
        if drafts.documents[1].title ~= "BETA ARTICLE" then
            return "WRONG_DRAFT_TITLE:" .. tostring(drafts.documents[1].title)
        end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn lua_globals_config_get_and_update() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
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
    "#,
    );
    assert_eq!(result, "ok");
}

// ── 4D. Lua CRUD edge cases ──────────────────────────────────────────────────

#[test]
fn lua_find_with_where_clause() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        crap.collections.create("articles", {
            title = "Where Test Alpha",
            status = "published",
        })
        crap.collections.create("articles", {
            title = "Where Test Beta",
            status = "draft",
        })

        local result = crap.collections.find("articles", {
            where = { status = { equals = "published" } },
        })
        if result.pagination.totalDocs ~= 1 then
            return "WRONG_TOTAL:" .. tostring(result.pagination.totalDocs)
        end
        -- after_read field hook uppercases title
        if result.documents[1].title ~= "WHERE TEST ALPHA" then
            return "WRONG_TITLE:" .. tostring(result.documents[1].title)
        end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn lua_find_with_limit_offset() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
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
        if result.pagination.totalDocs ~= 5 then
            return "WRONG_TOTAL:" .. tostring(result.pagination.totalDocs)
        end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn lua_find_by_id_with_depth() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        local doc = crap.collections.create("articles", {
            title = "Depth Test",
            body = "Content",
        })

        -- find_by_id with depth=0 opts
        local found = crap.collections.find_by_id("articles", doc.id, { depth = 0 })
        if found == nil then return "NOT_FOUND" end
        -- after_read field hook uppercases title
        if found.title ~= "DEPTH TEST" then
            return "WRONG_TITLE:" .. tostring(found.title)
        end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
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

    let config = CrapConfig::test_default();
    let registry = hooks::init_lua(tmp.path(), &config).expect("init_lua");

    let mut db_config = CrapConfig::test_default();
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

#[test]
fn lua_create_with_draft_option() {
    let (_tmp, pool, _reg, runner) = setup_versioned_db();
    let result = eval_versioned(
        &runner,
        &pool,
        r#"
        local doc = crap.collections.create("articles", {
            title = "Draft Article",
            body = "Some content",
        }, { draft = true })

        if doc == nil then return "CREATE_NIL" end
        if doc.id == nil then return "NO_ID" end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn lua_create_draft_skips_required_validation() {
    let (_tmp, pool, _reg, runner) = setup_versioned_db();
    let result = eval_versioned(
        &runner,
        &pool,
        r#"
        -- title is required, but draft=true should skip validation
        local ok, err = pcall(function()
            crap.collections.create("articles", {
                body = "No title, just body",
            }, { draft = true })
        end)
        if ok then return "ok" end
        return "FAILED:" .. tostring(err)
    "#,
    );
    assert_eq!(
        result, "ok",
        "Draft create should skip required field validation"
    );
}

#[test]
fn lua_create_publish_enforces_required_validation() {
    let (_tmp, pool, _reg, runner) = setup_versioned_db();
    let result = eval_versioned(
        &runner,
        &pool,
        r#"
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
    "#,
    );
    assert_eq!(
        result, "ok",
        "Publish create should enforce required validation"
    );
}

#[test]
fn lua_update_with_draft_option() {
    let (_tmp, pool, _reg, runner) = setup_versioned_db();
    let result = eval_versioned(
        &runner,
        &pool,
        r#"
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
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn lua_update_publish_modifies_main_table() {
    let (_tmp, pool, _reg, runner) = setup_versioned_db();
    let result = eval_versioned(
        &runner,
        &pool,
        r#"
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
    "#,
    );
    assert_eq!(result, "ok");
}

// ══════════════════════════════════════════════════════════════════════════════
// NEW FEATURES: crap.collections.count, update_many, delete_many
// ══════════════════════════════════════════════════════════════════════════════

#[test]
fn lua_count_empty_collection() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        local count = crap.collections.count("articles")
        return tostring(count)
    "#,
    );
    assert_eq!(result, "0");
}

#[test]
fn lua_count_with_documents() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        crap.collections.create("articles", { title = "A", body = "1" })
        crap.collections.create("articles", { title = "B", body = "2" })
        crap.collections.create("articles", { title = "C", body = "3" })
        return tostring(crap.collections.count("articles"))
    "#,
    );
    assert_eq!(result, "3");
}

#[test]
fn lua_count_with_where() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        crap.collections.create("articles", { title = "A", status = "published" })
        crap.collections.create("articles", { title = "B", status = "draft" })
        crap.collections.create("articles", { title = "C", status = "published" })
        local count = crap.collections.count("articles", {
            where = { status = "published" },
        })
        return tostring(count)
    "#,
    );
    assert_eq!(result, "2");
}

#[test]
fn lua_count_nonexistent_collection() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let conn = pool.get().expect("conn");
    let result = runner
        .eval_lua_with_conn(
            r#"
        local ok, err = pcall(function()
            crap.collections.count("nonexistent")
        end)
        if ok then return "SHOULD_HAVE_FAILED" end
        if tostring(err):find("not found") then return "ok" end
        return "UNEXPECTED:" .. tostring(err)
    "#,
            &conn,
            None,
        )
        .expect("eval");
    assert_eq!(result, "ok");
}

#[test]
fn lua_update_many_basic() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        crap.collections.create("articles", { title = "A", status = "draft" })
        crap.collections.create("articles", { title = "B", status = "draft" })
        crap.collections.create("articles", { title = "C", status = "published" })

        local result = crap.collections.update_many("articles",
            { where = { status = "draft" } },
            { status = "published" }
        )
        if result.modified ~= 2 then
            return "WRONG_MODIFIED:" .. tostring(result.modified)
        end

        -- Verify all are now published
        local count = crap.collections.count("articles", {
            where = { status = "published" },
        })
        if count ~= 3 then
            return "WRONG_COUNT:" .. tostring(count)
        end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn lua_update_many_no_matches() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        crap.collections.create("articles", { title = "A", status = "published" })

        local result = crap.collections.update_many("articles",
            { where = { status = "archived" } },
            { status = "published" }
        )
        if result.modified ~= 0 then
            return "WRONG_MODIFIED:" .. tostring(result.modified)
        end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn lua_delete_many_basic() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        crap.collections.create("articles", { title = "A", status = "draft" })
        crap.collections.create("articles", { title = "B", status = "draft" })
        crap.collections.create("articles", { title = "C", status = "published" })

        local result = crap.collections.delete_many("articles",
            { where = { status = "draft" } }
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
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn lua_delete_many_no_matches() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        crap.collections.create("articles", { title = "A", status = "published" })

        local result = crap.collections.delete_many("articles",
            { where = { status = "archived" } }
        )
        if result.deleted ~= 0 then
            return "WRONG_DELETED:" .. tostring(result.deleted)
        end

        local count = crap.collections.count("articles")
        if count ~= 1 then
            return "WRONG_REMAINING:" .. tostring(count)
        end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn lua_delete_many_all() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        crap.collections.create("articles", { title = "A" })
        crap.collections.create("articles", { title = "B" })
        crap.collections.create("articles", { title = "C" })

        -- Empty filter matches all
        local result = crap.collections.delete_many("articles", {})
        if result.deleted ~= 3 then
            return "WRONG_DELETED:" .. tostring(result.deleted)
        end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

// ══════════════════════════════════════════════════════════════════════════════
// NEW FEATURES: crap.util -- pure Lua table helpers
// ══════════════════════════════════════════════════════════════════════════════

#[test]
fn util_deep_merge() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
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
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn util_pick() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local t = { a = 1, b = 2, c = 3, d = 4 }
        local picked = crap.util.pick(t, { "a", "c" })
        if picked.a ~= 1 then return "A" end
        if picked.c ~= 3 then return "C" end
        if picked.b ~= nil then return "B_PRESENT" end
        if picked.d ~= nil then return "D_PRESENT" end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn util_omit() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local t = { a = 1, b = 2, c = 3, d = 4 }
        local result = crap.util.omit(t, { "b", "d" })
        if result.a ~= 1 then return "A" end
        if result.c ~= 3 then return "C" end
        if result.b ~= nil then return "B_PRESENT" end
        if result.d ~= nil then return "D_PRESENT" end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn util_keys_and_values() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
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
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn util_map() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local arr = { 1, 2, 3, 4 }
        local doubled = crap.util.map(arr, function(v) return v * 2 end)
        if #doubled ~= 4 then return "LEN:" .. #doubled end
        if doubled[1] ~= 2 then return "V1:" .. doubled[1] end
        if doubled[4] ~= 8 then return "V4:" .. doubled[4] end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn util_filter() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local arr = { 1, 2, 3, 4, 5 }
        local evens = crap.util.filter(arr, function(v) return v % 2 == 0 end)
        if #evens ~= 2 then return "LEN:" .. #evens end
        if evens[1] ~= 2 then return "V1:" .. evens[1] end
        if evens[2] ~= 4 then return "V2:" .. evens[2] end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn util_find() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local arr = { 10, 20, 30, 40 }
        local found = crap.util.find(arr, function(v) return v > 25 end)
        if found ~= 30 then return "FOUND:" .. tostring(found) end
        local not_found = crap.util.find(arr, function(v) return v > 100 end)
        if not_found ~= nil then return "SHOULD_BE_NIL" end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn util_includes() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local arr = { "a", "b", "c" }
        if not crap.util.includes(arr, "b") then return "MISSING_B" end
        if crap.util.includes(arr, "z") then return "HAS_Z" end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn util_is_empty() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        if not crap.util.is_empty({}) then return "EMPTY_NOT_EMPTY" end
        if crap.util.is_empty({ 1 }) then return "NON_EMPTY_IS_EMPTY" end
        if crap.util.is_empty({ x = 1 }) then return "MAP_IS_EMPTY" end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

// ── Deep nesting validation via Lua CRUD ────────────────────────────────

#[test]
fn lua_create_rejects_empty_required_in_nested_array() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        -- Array > Tabs > Row > required field must be validated
        local ok, err = pcall(function()
            crap.collections.create("team", {
                name = "Test Team",
                members = {
                    { first_name = "", last_name = "" },
                },
            })
        end)
        if ok then return "SHOULD_HAVE_FAILED" end
        local err_str = tostring(err)
        if err_str:find("required") or err_str:find("first_name") then
            return "ok"
        end
        return "UNEXPECTED_ERROR:" .. err_str
    "#,
    );
    assert_eq!(
        result, "ok",
        "Lua create must reject empty required fields inside Array > Tabs > Row"
    );
}

#[test]
fn lua_create_accepts_valid_nested_array() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        local doc = crap.collections.create("team", {
            name = "Test Team",
            members = {
                { first_name = "Jane", last_name = "Doe", email = "jane@example.com" },
            },
        })
        if not doc or not doc.id then return "NO_DOC" end
        return "ok"
    "#,
    );
    assert_eq!(
        result, "ok",
        "Lua create should accept valid nested array data"
    );
}
