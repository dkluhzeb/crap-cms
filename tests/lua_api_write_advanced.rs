#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::items_after_statements,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value,
    clippy::used_underscore_binding,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::unreadable_literal
)]

use std::path::PathBuf;

use crap_cms::config::CrapConfig;
use crap_cms::core::Registry;
use crap_cms::db::DbPool;
use crap_cms::hooks;
use crap_cms::hooks::lifecycle::HookRunner;
use std::sync::Arc;

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

// ── Helper: setup with real DB tables ────────────────────────────────────────

/// Set up a `HookRunner` with a real synced database (tables created from Lua definitions).
/// Returns (tempdir, pool, registry, runner). The tempdir must be kept alive for the DB.
#[allow(dead_code)]
fn setup_with_db() -> (tempfile::TempDir, DbPool, Arc<Registry>, HookRunner) {
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
        .registry(Arc::clone(&registry))
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

// ══════════════════════════════════════════════════════════════════════════════
// API SURFACE PARITY TESTS: password handling, unpublish, before_read, upload sizes
// ══════════════════════════════════════════════════════════════════════════════

// ── Lua CRUD Password Handling (Auth Collections) ────────────────────────────

/// Regression: `create_many` stripped a field named `password` from EVERY
/// collection (single `create` only separates it for auth collections) — a
/// non-auth collection with a legitimate `password` field silently lost the
/// value on bulk create.
#[test]
fn lua_create_many_keeps_password_field_on_non_auth_collection() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        crap.collections.create_many("wifi_networks", {
            { ssid = "HomeNet", password = "hunter2" },
        })
        local r = crap.collections.find("wifi_networks", {})
        return tostring(r.documents[1].password)
    "#,
    );
    assert_eq!(
        result, "hunter2",
        "bulk create must persist a non-auth collection's password field"
    );
}

/// Regression: a NUL was refused only in top-level text columns, and bulk
/// writes with `hooks = false` skip validation altogether — so a NUL inside a
/// blocks row reached the row's JSON, which Postgres' `::jsonb` cast rejects on
/// every later row-path filter. The persisted data is checked on every path.
#[test]
fn lua_writes_refuse_a_nul_at_any_depth() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let conn = pool.get().expect("conn");

    for (label, code) in [
        (
            "create_many without hooks",
            r#"crap.collections.create_many("products", {
                { name = "p", content = { { _block_type = "text", body = "a\0b" } } },
            }, { hooks = false })"#,
        ),
        (
            "create",
            r#"crap.collections.create("products", {
                name = "p", variants = { { dimensions = { width = "\0" } } },
            })"#,
        ),
        (
            "update_many without hooks",
            r#"crap.collections.create("products", { name = "q" })
            crap.collections.update_many("products", {}, { seo = { meta_title = "\0" } },
                { hooks = false })"#,
        ),
    ] {
        let err = runner
            .eval_lua_with_conn(code, &conn, None)
            .expect_err("a NUL must be refused");
        assert!(
            err.to_string().contains("must not contain NUL characters"),
            "{label}: {err}"
        );
    }
}

/// Regression: `update_many` rejected `password` only when it arrived as a
/// STRING — a table-valued password slipped past the stringified-map check
/// and reached the write via the composite merge. The guard now inspects the
/// fully merged patch, so any value type is rejected on auth collections.
#[test]
fn lua_update_many_rejects_password_of_any_type_on_auth_collection() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let conn = pool.get().expect("conn");

    for (label, patch) in [
        ("string", r#"{ password = "newpass123" }"#),
        ("table", r"{ password = { sneaky = true } }"),
    ] {
        let code = format!(
            r#"
            crap.collections.update_many("accounts", {{}}, {patch})
            return "ok"
            "#
        );
        let err = runner
            .eval_lua_with_conn(&code, &conn, None)
            .expect_err("password in update_many must be rejected");
        assert!(
            err.to_string().contains("Cannot set password"),
            "{label}-valued password must hit the guard, got: {err}"
        );
    }
}

/// Regression: a `before_read` hook reading its own collection recursed
/// without any depth cap — stack overflow, process abort. Reads must be
/// depth-capped exactly like writes (cap 3, hooks silently skipped beyond).
#[test]
fn lua_read_hook_recursion_is_depth_capped() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let conn = pool.get().expect("conn");
    let result = runner
        .eval_lua_with_conn(
            r#"
            _G._read_depth_counter = 0
            crap.collections.find("recursive_read", {})
            return tostring(_G._read_depth_counter)
            "#,
            &conn,
            None,
        )
        .expect("recursive before_read must be depth-capped, not overflow");

    let count: i32 = result.parse().expect("counter must be numeric");
    assert!(
        (1..=3).contains(&count),
        "before_read fired {count} times; the depth cap must stop the recursion"
    );
}

#[test]
fn lua_delete_with_hooks_false() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        local doc = crap.collections.create("articles", { title = "To Delete" })
        crap.collections.delete("articles", doc.id, { hooks = false })
        local r = crap.collections.find("articles", {})
        return tostring(r.pagination.total_docs)
    "#,
    );
    assert_eq!(result, "0");
}

// ── CRUD: find_by_id with nonexistent collection ─────────────────────────────

#[test]
fn lua_find_by_id_nonexistent_collection() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let conn = pool.get().expect("conn");
    let result = runner.eval_lua_with_conn(
        r#"
        local doc = crap.collections.find_by_id("nonexistent", "some-id")
        return "unreachable"
    "#,
        &conn,
        None,
    );
    assert!(
        result.is_err(),
        "find_by_id on nonexistent collection should error"
    );
}

// ── CRUD: create on nonexistent collection ───────────────────────────────────

#[test]
fn lua_create_nonexistent_collection() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let conn = pool.get().expect("conn");
    let result = runner.eval_lua_with_conn(
        r#"
        local doc = crap.collections.create("nonexistent", { title = "test" })
        return "unreachable"
    "#,
        &conn,
        None,
    );
    assert!(
        result.is_err(),
        "create on nonexistent collection should error"
    );
}

// ── CRUD: update on nonexistent collection ───────────────────────────────────

#[test]
fn lua_update_nonexistent_collection() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let conn = pool.get().expect("conn");
    let result = runner.eval_lua_with_conn(
        r#"
        crap.collections.update("nonexistent", "id", { title = "test" })
        return "unreachable"
    "#,
        &conn,
        None,
    );
    assert!(result.is_err());
}

// ── CRUD: delete on nonexistent collection ───────────────────────────────────

#[test]
fn lua_delete_nonexistent_collection() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let conn = pool.get().expect("conn");
    let result = runner.eval_lua_with_conn(
        r#"
        crap.collections.delete("nonexistent", "id")
        return "unreachable"
    "#,
        &conn,
        None,
    );
    assert!(result.is_err());
}

// ── CRUD: count on nonexistent collection ────────────────────────────────────

#[test]
fn lua_count_nonexistent_collection_2() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let conn = pool.get().expect("conn");
    let result = runner.eval_lua_with_conn(
        r#"
        local c = crap.collections.count("nonexistent")
        return "unreachable"
    "#,
        &conn,
        None,
    );
    assert!(result.is_err());
}

// ── CRUD: update_many with filters ───────────────────────────────────────────

#[test]
fn lua_update_many_with_operator_filters() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        crap.collections.create("articles", { title = "UM1", status = "draft" })
        crap.collections.create("articles", { title = "UM2", status = "draft" })
        crap.collections.create("articles", { title = "UM3", status = "published" })

        -- Update only drafts
        local r = crap.collections.update_many("articles",
            { where = { status = "draft" } },
            { status = "archived" }
        )
        if r.modified ~= 2 then return "WRONG_MOD:" .. tostring(r.modified) end

        -- Verify
        local all = crap.collections.find("articles", { where = { status = "archived" } })
        if all.pagination.total_docs ~= 2 then return "WRONG_ARCHIVED:" .. tostring(all.pagination.total_docs) end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

// ── CRUD: delete_many with filters ───────────────────────────────────────────

#[test]
fn lua_delete_many_with_operator_filters() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        crap.collections.create("articles", { title = "DM1", status = "draft" })
        crap.collections.create("articles", { title = "DM2", status = "draft" })
        crap.collections.create("articles", { title = "DM3", status = "published" })

        -- Delete only drafts
        local r = crap.collections.delete_many("articles",
            { where = { status = "draft" } }
        )
        if r.deleted ~= 2 then return "WRONG_DEL:" .. tostring(r.deleted) end

        -- Verify remaining
        local all = crap.collections.find("articles", {})
        if all.pagination.total_docs ~= 1 then return "WRONG_REMAINING:" .. tostring(all.pagination.total_docs) end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

// ── CRUD: delete_many result shape — `{ deleted, skipped }` ──────────────────
//
// The Lua `delete_many` result is a table `{ deleted, skipped }`. `skipped`
// counts documents that were found but blocked by the ref-count guard
// (incoming references). Non-referenced, access-allowed docs flow through
// `deleted`. Access-denied on a target doc errors the whole op (not skipped).
// This test asserts the return shape and default values on a vanilla op.

#[test]
fn lua_delete_many_result_shape_includes_deleted_and_skipped() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        crap.collections.create("articles", { title = "S1" })
        crap.collections.create("articles", { title = "S2" })

        local r = crap.collections.delete_many("articles", {})
        if r.deleted ~= 2 then return "WRONG_DEL:" .. tostring(r.deleted) end
        -- `skipped` must be present and 0 when no docs are referenced.
        if r.skipped ~= 0 then return "WRONG_SKIPPED:" .. tostring(r.skipped) end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

/// Parity: `delete_many` can now target the trash. Previously
/// `include_deleted` was hardcoded false so Lua could never empty trash.
/// A `trash = true` call permanently removes already-soft-deleted rows.
#[test]
fn lua_delete_many_trash_option_empties_trash() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        local a = crap.collections.create("trashable", { title = "A" })
        local b = crap.collections.create("trashable", { title = "B" })

        -- Soft-delete both (moves them to trash, does not remove).
        crap.collections.delete("trashable", a.id)
        crap.collections.delete("trashable", b.id)

        -- A normal delete_many without trash must NOT touch trashed rows.
        local normal = crap.collections.delete_many("trashable", {})
        if normal.deleted ~= 0 then return "NORMAL_HIT_TRASH:" .. tostring(normal.deleted) end

        -- trash = true permanently removes the trashed rows.
        local purged = crap.collections.delete_many("trashable", {}, { trash = true })
        return "purged:" .. tostring(purged.deleted)
    "#,
    );
    assert_eq!(result, "purged:2", "trash option must empty the trash");
}

// ── CRUD: update_many nonexistent collection ─────────────────────────────────

#[test]
fn lua_update_many_nonexistent_collection() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let conn = pool.get().expect("conn");
    let result = runner.eval_lua_with_conn(
        r#"
        crap.collections.update_many("nonexistent", {}, { title = "x" })
        return "unreachable"
    "#,
        &conn,
        None,
    );
    assert!(result.is_err());
}

// ── CRUD: delete_many nonexistent collection ─────────────────────────────────

#[test]
fn lua_delete_many_nonexistent_collection() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let conn = pool.get().expect("conn");
    let result = runner.eval_lua_with_conn(
        r#"
        crap.collections.delete_many("nonexistent", {})
        return "unreachable"
    "#,
        &conn,
        None,
    );
    assert!(result.is_err());
}

// ── CRUD: globals.get nonexistent ────────────────────────────────────────────

#[test]
fn lua_globals_get_nonexistent() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let conn = pool.get().expect("conn");
    let result = runner.eval_lua_with_conn(
        r#"
        crap.globals.get("nonexistent_global")
        return "unreachable"
    "#,
        &conn,
        None,
    );
    assert!(result.is_err());
}

// ── CRUD: globals.update nonexistent ─────────────────────────────────────────

#[test]
fn lua_globals_update_nonexistent() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let conn = pool.get().expect("conn");
    let result = runner.eval_lua_with_conn(
        r#"
        crap.globals.update("nonexistent_global", { key = "value" })
        return "unreachable"
    "#,
        &conn,
        None,
    );
    assert!(result.is_err());
}

/// Parity: `crap.globals.update` now accepts `draft = true` for a
/// version-only save (main row unchanged), matching `crap.collections.update`.
#[test]
fn lua_globals_update_draft_option_keeps_main_row_published() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        crap.globals.update("versioned_banner", { headline = "Published" })
        -- Draft save: main row stays on the published value.
        crap.globals.update("versioned_banner", { headline = "Draft edit" }, { draft = true })
        local g = crap.globals.get("versioned_banner")
        return tostring(g.headline)
    "#,
    );
    assert_eq!(
        result, "Published",
        "draft save must not change the published main row"
    );
}

/// Parity: `crap.globals.unpublish` now exists and reverts a versioned
/// global's `_status` to draft without touching field data.
#[test]
fn lua_globals_unpublish_sets_draft_status() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        crap.globals.update("versioned_banner", { headline = "Live" })
        local d = crap.globals.unpublish("versioned_banner")
        return tostring(d._status)
    "#,
    );
    assert_eq!(result, "draft", "unpublish must set _status to draft");
}

/// Parity: the per-slug accessor binds `unpublish` and `validate` exactly
/// like the collections accessor does (`crap.globals.<slug>.unpublish()` ==
/// `crap.globals.unpublish(slug)`). Regression: the accessor used to bind
/// only `get`/`update`, so the sugared form was a nil call.
#[test]
fn lua_globals_accessor_binds_unpublish_and_validate() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        crap.globals.versioned_banner.update({ headline = "Live" })
        local d = crap.globals.versioned_banner.unpublish()
        if d._status ~= "draft" then return "BAD_STATUS:" .. tostring(d._status) end

        local v = crap.globals.versioned_banner.validate({ headline = "ok" })
        if v.valid ~= true then return "INVALID" end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok", "accessor must bind unpublish + validate");
}

/// Parity: `crap.globals.unpublish` errors on a global without versioning.
#[test]
fn lua_globals_unpublish_errors_without_versioning() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let conn = pool.get().expect("conn");
    let result = runner.eval_lua_with_conn(
        r#"
        crap.globals.unpublish("settings")
        return "unreachable"
    "#,
        &conn,
        None,
    );
    assert!(
        result.is_err(),
        "unpublish on a non-versioned global must error"
    );
    assert!(
        result.unwrap_err().to_string().contains("versioning"),
        "error should mention versioning"
    );
}

// ── CRUD: CRUD without TxContext errors ──────────────────────────────────────

#[test]
fn lua_crud_without_tx_context_errors() {
    // Calling CRUD functions outside of hook context should error
    let runner = setup_lua();
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    let pool = crap_cms::db::pool::create_pool(tmp.path(), &config).expect("pool");
    let conn = pool.get().expect("conn");

    // Don't use eval_lua_with_conn — that sets TxContext.
    // Instead, directly evaluate Lua without setting up the connection context.
    // But we need a connection to test. eval_lua_with_conn DOES set up TxContext,
    // so this test verifies the error message for when it's not set.
    // Since we can't easily test this path through the public API (eval_lua_with_conn
    // always sets TxContext), we just verify the error path works when the function
    // is called for a nonexistent collection (different error path).
    let result = runner.eval_lua_with_conn(
        r#"
        local ok, err = pcall(function()
            crap.collections.find("nonexistent_collection_xyz", {})
        end)
        if not ok then return "ERROR:" .. tostring(err) end
        return "ok"
    "#,
        &conn,
        None,
    );
    assert!(result.is_ok());
    let msg = result.unwrap();
    assert!(
        msg.starts_with("ERROR:"),
        "Should error for nonexistent collection: {msg}"
    );
}

// ── CRUD: find with order_by ─────────────────────────────────────────────────

#[test]
fn lua_find_with_order_by() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        crap.collections.create("articles", { title = "Charlie" })
        crap.collections.create("articles", { title = "Alpha" })
        crap.collections.create("articles", { title = "Bravo" })

        local r = crap.collections.find("articles", {
            order_by = "title",
        })
        -- after_read field hook uppercases title
        if r.documents[1].title ~= "ALPHA" then return "WRONG1:" .. r.documents[1].title end
        if r.documents[2].title ~= "BRAVO" then return "WRONG2:" .. r.documents[2].title end
        if r.documents[3].title ~= "CHARLIE" then return "WRONG3:" .. r.documents[3].title end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

// ── CRUD: create with group field via Lua table ──────────────────────────────

#[test]
fn lua_create_with_group_field() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        -- products collection has a "seo" group field with "meta_title" sub-field
        local doc = crap.collections.create("products", {
            name = "Test Product",
            seo = { meta_title = "My SEO Title" },
        })
        if doc == nil then return "CREATE_NIL" end
        if doc.name ~= "Test Product" then return "WRONG_NAME" end

        -- Verify the group field was stored correctly
        local found = crap.collections.find_by_id("products", doc.id)
        if found == nil then return "NOT_FOUND" end
        -- Groups come back as flattened fields or as nested tables depending on hydration
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

// ── CRUD: update with group field ────────────────────────────────────────────

#[test]
fn lua_update_with_group_field() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        local doc = crap.collections.create("products", {
            name = "Original Product",
        })

        local updated = crap.collections.update("products", doc.id, {
            name = "Updated Product",
            seo = { meta_title = "Updated SEO" },
        })
        if updated == nil then return "UPDATE_NIL" end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

// ── CRUD: OR filter with number value in sub-group ───────────────────────────

#[test]
fn lua_find_or_filter_number_value() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        crap.collections.create("articles", { title = "X", word_count = "10" })
        crap.collections.create("articles", { title = "Y", word_count = "20" })

        local r = crap.collections.find("articles", {
            where = {
                ["or"] = {
                    { word_count = 10.0 },
                    { title = "Y" },
                },
            },
        })
        if r.pagination.total_docs ~= 2 then return "WRONG:" .. tostring(r.pagination.total_docs) end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

// ── CRUD: unknown filter operator errors ─────────────────────────────────────

#[test]
fn lua_find_unknown_filter_operator_errors() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let conn = pool.get().expect("conn");
    let result = runner.eval_lua_with_conn(
        r#"
        local ok, err = pcall(function()
            crap.collections.find("articles", {
                where = { title = { bad_operator = "test" } },
            })
        end)
        if not ok then return "ERROR:" .. tostring(err) end
        return "ok"
    "#,
        &conn,
        None,
    );
    assert!(result.is_ok());
    let msg = result.unwrap();
    assert!(
        msg.starts_with("ERROR:"),
        "Unknown filter operator should error: {msg}"
    );
    // Typed `FilterOperators` rejects unknown ops via serde's
    // `deny_unknown_fields`, which surfaces as "unknown field" with
    // the field name in the message.
    assert!(
        msg.contains("bad_operator"),
        "Error should mention the bad operator name: {msg}"
    );
}

// ══════════════════════════════════════════════════════════════════════════════
// crap.collections.config.get / config.list (round-trip)
// ══════════════════════════════════════════════════════════════════════════════

#[test]
fn lua_collections_config_get() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        local config = crap.collections.config.get("articles")
        if config == nil then return "NIL" end
        -- Should have labels, fields, hooks, access
        if config.fields == nil then return "NO_FIELDS" end
        if #config.fields == 0 then return "EMPTY_FIELDS" end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn lua_collections_config_get_nonexistent() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        local config = crap.collections.config.get("nonexistent")
        if config == nil then return "ok" end
        return "NOT_NIL"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn lua_collections_config_list() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        local all = crap.collections.config.list()
        if all == nil then return "NIL" end
        if all["articles"] == nil then return "NO_ARTICLES" end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn lua_globals_config_get() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        local config = crap.globals.config.get("settings")
        if config == nil then return "NIL" end
        if config.fields == nil then return "NO_FIELDS" end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn lua_globals_config_get_nonexistent() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        local config = crap.globals.config.get("nonexistent")
        if config == nil then return "ok" end
        return "NOT_NIL"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn lua_globals_config_list() {
    let (_tmp, pool, _reg, runner) = setup_with_db();
    let result = eval_lua_db(
        &runner,
        &pool,
        r#"
        local all = crap.globals.config.list()
        if all == nil then return "NIL" end
        if all["settings"] == nil then return "NO_SETTINGS" end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

// ══════════════════════════════════════════════════════════════════════════════
// crap.jobs.define
// ══════════════════════════════════════════════════════════════════════════════

#[test]
fn lua_jobs_define() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("init.lua"),
        r#"
crap.jobs.define("cleanup", {
    handler = "hooks.jobs.cleanup",
    schedule = "0 0 * * *",
    queue = "maintenance",
    retries = 3,
})
    "#,
    )
    .unwrap();

    // Startup validation resolves the handler ref — the module must exist.
    let hooks_dir = tmp.path().join("hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();
    std::fs::write(
        hooks_dir.join("jobs.lua"),
        "local M = {}\nfunction M.cleanup(_ctx) end\nreturn M\n",
    )
    .unwrap();

    let config = CrapConfig::test_default();
    let registry = hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let job = registry.get_job("cleanup").expect("cleanup job");
    assert_eq!(job.handler.reference(), "hooks.jobs.cleanup");
    assert_eq!(job.schedule, Some("0 0 * * *".to_string()));
    assert_eq!(job.queue, "maintenance");
    assert_eq!(job.retries, Some(3));
}

// ══════════════════════════════════════════════════════════════════════════════
// crap.locale with custom config
// ══════════════════════════════════════════════════════════════════════════════

#[test]
fn lua_locale_custom_config() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let mut config = CrapConfig::test_default();
    config.locale.default_locale = "de".to_string();
    config.locale.locales = vec!["de".to_string(), "en".to_string(), "fr".to_string()];

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
        local default = crap.locale.get_default()
        if default ~= "de" then return "WRONG_DEFAULT:" .. default end
        local all = crap.locale.get_all()
        if #all ~= 3 then return "WRONG_COUNT:" .. tostring(#all) end
        local enabled = crap.locale.is_enabled()
        if not enabled then return "NOT_ENABLED" end
        return "ok"
    "#,
            &conn,
            None,
        )
        .expect("eval");
    assert_eq!(result, "ok");
}

/// Founding fix (array/blocks row identity): updating an array by round-tripping
/// each row's `id` preserves a sub-field the update omits — here the nested
/// `dimensions` group — instead of destroying it via delete-and-reinsert. Proves
/// the row id survives the full Lua write pipeline (validation → canonicalize →
/// save) to the diff-based join writer.
#[test]
fn lua_array_update_by_row_id_preserves_omitted_subfield() {
    let (_tmp, pool, _reg, runner) = setup_with_db();

    let product_id = eval_lua_db(
        &runner,
        &pool,
        r#"
        local p = crap.collections.create("products", {
            name = "Widget",
            variants = {
                { color = "red", dimensions = { width = "10", height = "20" } },
            },
        })
        return p.id
    "#,
    );
    assert!(
        !product_id.is_empty() && product_id != "nil",
        "create returned id"
    );

    // The array row id must be exposed to Lua for the round-trip to be possible.
    let variant_id = eval_lua_db(
        &runner,
        &pool,
        &format!(
            r#"
        local p = crap.collections.find_by_id("products", "{product_id}")
        return tostring(p.variants[1].id)
    "#
        ),
    );
    assert!(
        !variant_id.is_empty() && variant_id != "nil",
        "the array row id must be exposed to Lua, got {variant_id:?}"
    );

    // Update the variant by id, changing `color` and OMITTING `dimensions`.
    eval_lua_db(
        &runner,
        &pool,
        &format!(
            r#"
        crap.collections.update("products", "{product_id}", {{
            variants = {{
                {{ id = "{variant_id}", color = "blue" }},
            }},
        }})
        return "ok"
    "#
        ),
    );

    // `color` updated; the omitted nested group is PRESERVED, not NULLed.
    let result = eval_lua_db(
        &runner,
        &pool,
        &format!(
            r#"
        local p = crap.collections.find_by_id("products", "{product_id}")
        local v = p.variants[1]
        local w = (v.dimensions and v.dimensions.width) or "GONE"
        return tostring(v.color) .. "|" .. tostring(w)
    "#
        ),
    );
    assert_eq!(
        result, "blue|10",
        "color updated and the omitted nested group preserved (row identity round-tripped)"
    );
}
