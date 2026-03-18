//! Lua API tests for Array/Blocks join data CRUD, relationship depth
//! hydration, globals with join data, cross-collection hook transactions,
//! and locale-specific updates.

use std::path::PathBuf;

use crap_cms::config::CrapConfig;
use crap_cms::core::SharedRegistry;
use crap_cms::db::DbPool;
use crap_cms::hooks;
use crap_cms::hooks::lifecycle::HookRunner;

// ── Helpers ───────────────────────────────────────────────────────────────

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hook_tests")
}

fn setup_with_db() -> (tempfile::TempDir, DbPool, SharedRegistry, HookRunner) {
    let config_dir = fixture_dir();
    let config = CrapConfig::default();
    let registry = hooks::init_lua(&config_dir, &config).expect("init_lua failed");

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

fn eval_lua_db(runner: &HookRunner, pool: &DbPool, code: &str) -> String {
    let conn = pool.get().expect("conn");
    runner
        .eval_lua_with_conn(code, &conn, None)
        .expect("eval failed")
}

/// Set up a custom Lua environment with specified collection/global definitions.
fn setup_custom_db(
    collection_defs: &[(&str, &str)],
    global_defs: &[(&str, &str)],
    locales: Option<Vec<&str>>,
) -> (tempfile::TempDir, DbPool, SharedRegistry, HookRunner) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    let globals_dir = tmp.path().join("globals");
    std::fs::create_dir_all(&collections_dir).unwrap();
    std::fs::create_dir_all(&globals_dir).unwrap();

    for (name, lua_code) in collection_defs {
        std::fs::write(collections_dir.join(format!("{name}.lua")), lua_code).unwrap();
    }
    for (name, lua_code) in global_defs {
        std::fs::write(globals_dir.join(format!("{name}.lua")), lua_code).unwrap();
    }
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let mut config = CrapConfig::default();
    if let Some(l) = locales {
        config.locale.locales = l.iter().map(|s| s.to_string()).collect();
        config.locale.default_locale = l.first().unwrap_or(&"en").to_string();
        config.locale.fallback = true;
    }

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

/// Set up a custom Lua environment with hooks.
fn setup_custom_db_with_hooks(
    collection_defs: &[(&str, &str)],
    hook_files: &[(&str, &str)],
) -> (tempfile::TempDir, DbPool, SharedRegistry, HookRunner) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    let hooks_dir = tmp.path().join("hooks");
    std::fs::create_dir_all(&collections_dir).unwrap();
    std::fs::create_dir_all(&hooks_dir).unwrap();

    for (name, lua_code) in collection_defs {
        std::fs::write(collections_dir.join(format!("{name}.lua")), lua_code).unwrap();
    }
    for (name, lua_code) in hook_files {
        std::fs::write(hooks_dir.join(format!("{name}.lua")), lua_code).unwrap();
    }
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::default();
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

// ── Array/Blocks CRUD Tests ─────────────────────────────────────────────

#[test]
fn lua_create_with_array_data() {
    let (_tmp, pool, _reg, runner) = setup_with_db();

    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        local doc = crap.collections.create("products", {
            name = "Widget",
            variants = {
                { color = "red", dimensions = { width = "10", height = "20" } },
                { color = "blue", dimensions = { width = "30", height = "40" } },
            },
        })

        local found = crap.collections.find_by_id("products", doc.id)
        local count = 0
        if found.variants then
            for _ in ipairs(found.variants) do count = count + 1 end
        end

        return tostring(count) .. ":" .. tostring(found.variants[1].color)
            .. ":" .. tostring(found.variants[2].color)
        "#,
    );

    assert_eq!(result, "2:red:blue");
}

#[test]
fn lua_create_with_blocks_data() {
    let (_tmp, pool, _reg, runner) = setup_with_db();

    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        local doc = crap.collections.create("products", {
            name = "Gizmo",
            content = {
                { _block_type = "text", body = "Hello world" },
                { _block_type = "section", heading = "Intro", meta = { author = "Alice" } },
            },
        })

        local found = crap.collections.find_by_id("products", doc.id)
        local count = 0
        if found.content then
            for _ in ipairs(found.content) do count = count + 1 end
        end

        return tostring(count) .. ":" .. tostring(found.content[1]._block_type)
            .. ":" .. tostring(found.content[2]._block_type)
        "#,
    );

    assert_eq!(result, "2:text:section");
}

#[test]
fn lua_create_with_all_join_types() {
    let (_tmp, pool, _reg, runner) = setup_with_db();

    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        local doc = crap.collections.create("products", {
            name = "Full Product",
            seo = { meta_title = "Buy Full Product" },
            variants = {
                { color = "green", dimensions = { width = "5", height = "10" } },
            },
            content = {
                { _block_type = "text", body = "Description" },
            },
        })

        local found = crap.collections.find_by_id("products", doc.id)

        local has_seo = found.seo and found.seo.meta_title == "Buy Full Product"
        local has_variants = found.variants and #found.variants == 1
        local has_content = found.content and #found.content == 1

        return tostring(has_seo) .. ":" .. tostring(has_variants) .. ":" .. tostring(has_content)
        "#,
    );

    assert_eq!(result, "true:true:true");
}

#[test]
fn lua_update_replaces_array() {
    let (_tmp, pool, _reg, runner) = setup_with_db();

    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        local doc = crap.collections.create("products", {
            name = "Updatable",
            variants = {
                { color = "red", dimensions = { width = "1", height = "2" } },
                { color = "blue", dimensions = { width = "3", height = "4" } },
            },
        })

        -- Verify initial count
        local before = crap.collections.find_by_id("products", doc.id)
        local before_count = #before.variants

        -- Update: replace with single variant
        crap.collections.update("products", doc.id, {
            variants = {
                { color = "green", dimensions = { width = "5", height = "6" } },
            },
        })

        local after = crap.collections.find_by_id("products", doc.id)
        local after_count = #after.variants

        return tostring(before_count) .. ":" .. tostring(after_count)
            .. ":" .. tostring(after.variants[1].color)
        "#,
    );

    assert_eq!(result, "2:1:green");
}

#[test]
fn lua_update_replaces_blocks() {
    let (_tmp, pool, _reg, runner) = setup_with_db();

    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        local doc = crap.collections.create("products", {
            name = "BlockUpdate",
            content = {
                { _block_type = "text", body = "Original" },
            },
        })

        -- Update: replace text block with section block
        crap.collections.update("products", doc.id, {
            content = {
                { _block_type = "section", heading = "New Section", meta = { author = "Bob" } },
            },
        })

        local found = crap.collections.find_by_id("products", doc.id)
        return tostring(found.content[1]._block_type)
            .. ":" .. tostring(found.content[1].heading)
            .. ":" .. tostring(found.content[1].meta.author)
        "#,
    );

    assert_eq!(result, "section:New Section:Bob");
}

#[test]
fn lua_find_hydrates_group_in_array() {
    let (_tmp, pool, _reg, runner) = setup_with_db();

    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        local doc = crap.collections.create("products", {
            name = "Nested Group",
            variants = {
                { color = "red", dimensions = { width = "5", height = "10" } },
            },
        })

        local found = crap.collections.find_by_id("products", doc.id)
        local dims = found.variants[1].dimensions

        return tostring(dims.width) .. ":" .. tostring(dims.height)
        "#,
    );

    assert_eq!(result, "5:10");
}

// ── Relationship Depth Tests ────────────────────────────────────────────

#[test]
fn lua_find_with_depth_populates_relationship() {
    let (_tmp, pool, _reg, runner) = setup_custom_db(
        &[
            (
                "categories",
                r#"
                crap.collections.define("categories", {
                    labels = { singular = "Category", plural = "Categories" },
                    fields = {
                        { name = "name", type = "text", required = true },
                    },
                })
                "#,
            ),
            (
                "posts",
                r#"
                crap.collections.define("posts", {
                    labels = { singular = "Post", plural = "Posts" },
                    fields = {
                        { name = "title", type = "text", required = true },
                        { name = "category", type = "relationship",
                          relationship = { collection = "categories" } },
                    },
                })
                "#,
            ),
        ],
        &[],
        None,
    );

    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        local cat = crap.collections.create("categories", { name = "Tech" })
        local post = crap.collections.create("posts", {
            title = "My Post",
            category = cat.id,
        })

        -- depth=1: relationship should be populated
        local found = crap.collections.find_by_id("posts", post.id, { depth = 1 })
        local cat_type = type(found.category)
        local cat_name = ""
        if cat_type == "table" then
            cat_name = found.category.name or ""
        end

        return cat_type .. ":" .. cat_name
        "#,
    );

    assert_eq!(result, "table:Tech");
}

#[test]
fn lua_find_depth_zero_returns_id() {
    let (_tmp, pool, _reg, runner) = setup_custom_db(
        &[
            (
                "categories",
                r#"
                crap.collections.define("categories", {
                    labels = { singular = "Category", plural = "Categories" },
                    fields = {
                        { name = "name", type = "text", required = true },
                    },
                })
                "#,
            ),
            (
                "posts",
                r#"
                crap.collections.define("posts", {
                    labels = { singular = "Post", plural = "Posts" },
                    fields = {
                        { name = "title", type = "text", required = true },
                        { name = "category", type = "relationship",
                          relationship = { collection = "categories" } },
                    },
                })
                "#,
            ),
        ],
        &[],
        None,
    );

    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        local cat = crap.collections.create("categories", { name = "Science" })
        local post = crap.collections.create("posts", {
            title = "Science Post",
            category = cat.id,
        })

        -- depth=0: relationship should be a string ID
        local found = crap.collections.find_by_id("posts", post.id, { depth = 0 })
        local cat_type = type(found.category)

        return cat_type .. ":" .. tostring(found.category == cat.id)
        "#,
    );

    assert_eq!(result, "string:true");
}

// ── Globals with Join Data ──────────────────────────────────────────────

#[test]
fn lua_globals_update_with_array() {
    let (_tmp, pool, _reg, runner) = setup_custom_db(
        &[],
        &[(
            "nav",
            r#"
            crap.globals.define("nav", {
                labels = { singular = "Navigation" },
                fields = {
                    { name = "items", type = "array", fields = {
                        { name = "label", type = "text" },
                        { name = "url", type = "text" },
                    }},
                },
            })
            "#,
        )],
        None,
    );

    // Step 1: verify globals.get works at all
    let check = eval_lua_db(
        &runner,
        &pool,
        r#"
        local found = crap.globals.get("nav")
        return type(found)
        "#,
    );
    assert_eq!(check, "table", "globals.get should return a table");

    // Step 2: update
    let update_result = eval_lua_db(
        &runner,
        &pool,
        r#"
        local updated = crap.globals.update("nav", {
            items = {
                { label = "Home", url = "/" },
                { label = "About", url = "/about" },
            },
        })
        return type(updated)
        "#,
    );
    assert_eq!(
        update_result, "table",
        "globals.update should return a table"
    );

    // Step 3: verify
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        local found = crap.globals.get("nav")
        if not found.items then return "NO_ITEMS" end

        local count = 0
        for _ in ipairs(found.items) do count = count + 1 end

        if count == 0 then return "EMPTY" end
        return tostring(count) .. ":" .. tostring(found.items[1].label)
            .. ":" .. tostring(found.items[2].url)
        "#,
    );

    assert_eq!(result, "2:Home:/about");
}

#[test]
fn lua_globals_update_with_blocks() {
    let (_tmp, pool, _reg, runner) = setup_custom_db(
        &[],
        &[(
            "page_layout",
            r#"
            crap.globals.define("page_layout", {
                labels = { singular = "Page Layout" },
                fields = {
                    { name = "sections", type = "blocks", blocks = {
                        { type = "hero", label = "Hero", fields = {
                            { name = "heading", type = "text" },
                        }},
                        { type = "cta", label = "CTA", fields = {
                            { name = "button_text", type = "text" },
                        }},
                    }},
                },
            })
            "#,
        )],
        None,
    );

    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        crap.globals.update("page_layout", {
            sections = {
                { _block_type = "hero", heading = "Welcome" },
                { _block_type = "cta", button_text = "Sign Up" },
            },
        })

        local found = crap.globals.get("page_layout")
        local count = 0
        if found.sections then
            for _ in ipairs(found.sections) do count = count + 1 end
        end

        return tostring(count) .. ":" .. tostring(found.sections[1]._block_type)
            .. ":" .. tostring(found.sections[2].button_text)
        "#,
    );

    assert_eq!(result, "2:hero:Sign Up");
}

// ── Cross-Collection Hooks + Transaction Integrity ──────────────────────

#[test]
fn lua_hook_creates_related_document() {
    let (_tmp, pool, _reg, runner) = setup_custom_db_with_hooks(
        &[
            (
                "orders",
                r#"
                crap.collections.define("orders", {
                    labels = { singular = "Order", plural = "Orders" },
                    fields = {
                        { name = "item", type = "text", required = true },
                    },
                    hooks = {
                        after_change = { "hooks.order_hooks.create_log" },
                    },
                })
                "#,
            ),
            (
                "logs",
                r#"
                crap.collections.define("logs", {
                    labels = { singular = "Log", plural = "Logs" },
                    fields = {
                        { name = "message", type = "text" },
                    },
                })
                "#,
            ),
        ],
        &[(
            "order_hooks",
            r#"
            local M = {}
            function M.create_log(ctx)
                crap.collections.create("logs", {
                    message = "order-created:" .. (ctx.data.item or "unknown"),
                })
                return ctx
            end
            return M
            "#,
        )],
    );

    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        crap.collections.create("orders", { item = "Widget" })

        local result = crap.collections.find("logs", {})
        local logs = result.documents
        local count = 0
        for _ in ipairs(logs) do count = count + 1 end

        local msg = ""
        if count > 0 then msg = logs[1].message end

        return tostring(count) .. ":" .. msg
        "#,
    );

    assert_eq!(result, "1:order-created:Widget");
}

#[test]
fn lua_hook_error_rolls_back_inner_crud() {
    use std::collections::HashMap;

    use crap_cms::core::collection::Hooks;
    use crap_cms::hooks::lifecycle::{HookContext, HookEvent};

    let (_tmp, pool, _reg, runner) = setup_custom_db_with_hooks(
        &[
            (
                "orders",
                r#"
                crap.collections.define("orders", {
                    labels = { singular = "Order", plural = "Orders" },
                    fields = {
                        { name = "item", type = "text", required = true },
                    },
                })
                "#,
            ),
            (
                "logs",
                r#"
                crap.collections.define("logs", {
                    labels = { singular = "Log", plural = "Logs" },
                    fields = {
                        { name = "message", type = "text" },
                    },
                })
                "#,
            ),
        ],
        &[(
            "order_hooks",
            r#"
            local M = {}
            function M.create_and_error(ctx)
                crap.collections.create("logs", {
                    message = "should-be-rolled-back",
                })
                error("intentional error for rollback test")
            end
            return M
            "#,
        )],
    );

    let reg = _reg.read().unwrap();
    let def = reg.get_collection("orders").unwrap().clone();
    drop(reg);

    let hooks = Hooks {
        after_change: vec!["hooks.order_hooks.create_and_error".to_string()],
        ..Default::default()
    };

    // Create order in a transaction
    let mut conn = pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = [("item".to_string(), "Widget".to_string())]
        .iter()
        .cloned()
        .collect::<HashMap<_, _>>();
    let doc = crap_cms::db::query::create(&tx, "orders", &def, &data, None).unwrap();
    let doc_id = doc.id.clone();

    // Run after_change hook — it should create a log then error
    let ctx = HookContext {
        collection: "orders".to_string(),
        operation: "create".to_string(),
        data: doc.fields.clone(),
        locale: None,
        draft: None,
        context: HashMap::new(),
        user: None,
        ui_locale: None,
    };
    let result = runner.run_after_write(&hooks, &def.fields, HookEvent::AfterChange, ctx, &tx);
    assert!(result.is_err(), "hook error should propagate");

    // Drop tx without committing (rollback)
    drop(tx);

    // Verify neither order nor log was persisted
    let conn2 = pool.get().unwrap();
    let found = crap_cms::db::query::find_by_id(&conn2, "orders", &def, &doc_id, None).unwrap();
    assert!(found.is_none(), "order should NOT exist after rollback");

    let log_reg = _reg.read().unwrap();
    let log_def = log_reg.get_collection("logs").unwrap().clone();
    drop(log_reg);

    let logs = crap_cms::db::query::find(
        &conn2,
        "logs",
        &log_def,
        &crap_cms::db::query::FindQuery::default(),
        None,
    )
    .unwrap();
    assert!(logs.is_empty(), "log should NOT exist after rollback");
}

// ── Locale ──────────────────────────────────────────────────────────────

#[test]
fn lua_update_with_locale() {
    let (_tmp, pool, _reg, runner) = setup_custom_db(
        &[(
            "posts",
            r#"
            crap.collections.define("posts", {
                labels = { singular = "Post", plural = "Posts" },
                fields = {
                    { name = "title", type = "text", required = true, localized = true },
                    { name = "body", type = "textarea", localized = true },
                },
            })
            "#,
        )],
        &[],
        Some(vec!["en", "de"]),
    );

    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        -- Create with English locale
        local doc = crap.collections.create("posts", {
            title = "Hello",
            body = "English body",
        }, { locale = "en" })

        -- Update German locale
        crap.collections.update("posts", doc.id, {
            title = "Hallo",
            body = "Deutscher Text",
        }, { locale = "de" })

        -- Find German
        local de = crap.collections.find_by_id("posts", doc.id, { locale = "de" })
        -- Find English
        local en = crap.collections.find_by_id("posts", doc.id, { locale = "en" })

        return de.title .. ":" .. de.body .. "|" .. en.title .. ":" .. en.body
        "#,
    );

    assert_eq!(result, "Hallo:Deutscher Text|Hello:English body");
}
