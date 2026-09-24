//! Hook recursion depth through Lua CRUD: `ctx.hook_depth` counts the
//! hook → CRUD → hook chain, and `[hooks] max_depth` stops it — for every
//! Lua CRUD entry point that runs hooks.

use std::{path::Path, sync::Arc};

use crap_cms::{
    config::CrapConfig,
    db::{DbPool, migrate, pool},
    hooks::{self, lifecycle::HookRunner},
};

/// A config dir with one definition file (`(path, source)`) and one hook
/// module (`(module name, source)`).
struct Project<'a> {
    definition: (&'a str, &'a str),
    hooks: (&'a str, &'a str),
}

impl<'a> Project<'a> {
    fn new(definition: (&'a str, &'a str), hooks: (&'a str, &'a str)) -> Self {
        Self { definition, hooks }
    }
}

/// Write `project` into `dir` with `[hooks] max_depth = max_depth`, load it,
/// sync the schema, and return the pool and the hook runner.
fn load(dir: &Path, project: &Project<'_>, max_depth: u32) -> (DbPool, HookRunner) {
    let (def_path, def_src) = project.definition;
    let (hooks_name, hooks_src) = project.hooks;

    let def_file = dir.join(def_path);
    std::fs::create_dir_all(def_file.parent().expect("definition dir")).unwrap();
    std::fs::write(def_file, def_src).unwrap();

    std::fs::create_dir_all(dir.join("hooks")).unwrap();
    std::fs::write(
        dir.join("hooks").join(format!("{hooks_name}.lua")),
        hooks_src,
    )
    .unwrap();
    std::fs::write(dir.join("init.lua"), "").unwrap();
    std::fs::write(
        dir.join("crap.toml"),
        format!("[hooks]\nmax_depth = {max_depth}\n"),
    )
    .unwrap();

    let mut config = CrapConfig::test_default();
    config.hooks.max_depth = max_depth;
    let registry = hooks::init_lua(dir, &config).expect("init_lua");

    let mut db_config = config.clone();
    db_config.database.path = "test.db".to_string();
    let pool = pool::create_pool(dir, &db_config).expect("pool");
    migrate::sync_all(&pool, &registry, &config.locale).expect("sync");

    let runner = HookRunner::builder()
        .config_dir(dir)
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .expect("runner");

    (pool, runner)
}

/// Run `code` against the project with a connection of its own.
fn eval(pool: &DbPool, runner: &HookRunner, code: &str) -> String {
    let conn = pool.get().expect("conn");

    runner.eval_lua_with_conn(code, &conn, None).expect("eval")
}

#[test]
fn lua_hook_depth_exposed_in_context() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (pool, runner) = load(
        tmp.path(),
        &Project::new(
            (
                "collections/depthtest.lua",
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
            ),
            (
                "depth_hooks",
                r#"
local M = {}
function M.record_depth(ctx)
    ctx.data.depth_seen = tostring(ctx.hook_depth or "nil")
    return ctx
end
return M
"#,
            ),
        ),
        3,
    );

    let result = eval(
        &pool,
        &runner,
        r#"
        local doc = crap.collections.create("depthtest", { name = "test" })
        -- At Lua CRUD level, hook_depth should be 1 (incremented from 0)
        if doc.depth_seen ~= "1" then
            return "WRONG_DEPTH:" .. tostring(doc.depth_seen)
        end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn lua_hook_recursion_capped() {
    // A hook creates another document in the same collection, which would
    // loop forever without the depth cap.
    let tmp = tempfile::tempdir().expect("tempdir");
    let (pool, runner) = load(
        tmp.path(),
        &Project::new(
            (
                "collections/recursive.lua",
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
            ),
            (
                "recursive_hooks",
                r#"
local M = {}
function M.spawn(ctx)
    local depth = ctx.hook_depth or 0
    crap.collections.create("recursive", {
        name = "spawned-at-depth-" .. tostring(depth),
        level = tostring(depth),
    })
    return ctx
end
return M
"#,
            ),
        ),
        2,
    );

    let result = eval(
        &pool,
        &runner,
        r#"
        crap.collections.create("recursive", { name = "root" })
        -- With max_depth=2: root creates at depth 0, hook fires at depth 1,
        -- which creates another doc, hook fires at depth 2 which creates
        -- another doc but hooks are skipped (depth >= max), so it stops.
        local result = crap.collections.find("recursive", {})
        if result.pagination.total_docs < 2 then
            return "TOO_FEW:" .. tostring(result.pagination.total_docs)
        end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

/// A `before_validate` hook that validates again, counting its calls in the
/// VM global `CALLS` (and giving up after ten, so a missing cap ends the
/// test instead of the stack).
const NESTING_VALIDATE_HOOK: &str = r"
local M = {}
function M.nest(ctx)
    CALLS = (CALLS or 0) + 1
    if CALLS < 10 then
        VALIDATE()
    end
    return ctx
end
return M
";

/// Regression: `crap.collections.validate` ran the dry-run's
/// `before_validate` hooks without the hook-depth check every other Lua CRUD
/// call makes, so a hook that validated recursed until the stack ran out.
/// It stops at `max_depth` now.
#[test]
fn lua_collection_validate_respects_the_hook_depth_cap() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (pool, runner) = load(
        tmp.path(),
        &Project::new(
            (
                "collections/loopy.lua",
                r#"
crap.collections.define("loopy", {
    fields = { { name = "name", type = "text" } },
    hooks = { before_validate = { "hooks.nesting.nest" } },
})
"#,
            ),
            ("nesting", NESTING_VALIDATE_HOOK),
        ),
        2,
    );

    let result = eval(
        &pool,
        &runner,
        r#"
        CALLS = 0
        VALIDATE = function() crap.collections.validate("loopy", { name = "x" }) end
        VALIDATE()
        return tostring(CALLS)
    "#,
    );
    assert_eq!(
        result, "2",
        "hooks run at depth 0 and 1, never at max_depth"
    );
}

/// Regression: `crap.globals.validate` skipped the hook-depth check the same
/// way `crap.collections.validate` did.
#[test]
fn lua_global_validate_respects_the_hook_depth_cap() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (pool, runner) = load(
        tmp.path(),
        &Project::new(
            (
                "globals/loopy.lua",
                r#"
crap.globals.define("loopy", {
    fields = { { name = "name", type = "text" } },
    hooks = { before_validate = { "hooks.nesting.nest" } },
})
"#,
            ),
            ("nesting", NESTING_VALIDATE_HOOK),
        ),
        2,
    );

    let result = eval(
        &pool,
        &runner,
        r#"
        CALLS = 0
        VALIDATE = function() crap.globals.validate("loopy", { name = "x" }) end
        VALIDATE()
        return tostring(CALLS)
    "#,
    );
    assert_eq!(
        result, "2",
        "hooks run at depth 0 and 1, never at max_depth"
    );
}
