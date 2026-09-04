//! `before_render` admin-render hooks: the page-identity argument, the
//! single context round trip, and the read-only database access the hooks
//! get on authenticated pages.

use std::sync::Arc;

use crap_cms::{
    config::CrapConfig,
    db::{DbConnection as _, DbPool, migrate, pool, query},
    hooks::{
        self,
        lifecycle::{HookRunner, RenderCrud, RenderInfo, RenderParams},
    },
};
use serde_json::{Value, json};

/// Build a config dir with one `articles` collection plus the given
/// `init.lua`, then a runner and a migrated pool over it.
fn setup(init_lua: &str) -> (tempfile::TempDir, DbPool, HookRunner) {
    setup_with_collection(
        r#"crap.collections.define("articles", { fields = { { name = "title", type = "text" } } })"#,
        init_lua,
    )
}

/// [`setup`] with a caller-supplied `collections/articles.lua`, for tests that
/// need the collection to carry hooks of its own.
fn setup_with_collection(
    articles_lua: &str,
    init_lua: &str,
) -> (tempfile::TempDir, DbPool, HookRunner) {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let collections = tmp.path().join("collections");
    std::fs::create_dir_all(&collections).unwrap();
    std::fs::write(collections.join("articles.lua"), articles_lua).unwrap();
    std::fs::write(tmp.path().join("init.lua"), init_lua).unwrap();

    let config = CrapConfig::test_default();
    let registry = hooks::init_lua(tmp.path(), &config).expect("init_lua");

    let mut pool_config = CrapConfig::test_default();
    pool_config.database.path = "test.db".to_string();
    let db_pool = pool::create_pool(tmp.path(), &pool_config).expect("create_pool");
    migrate::sync_all(&db_pool, &registry, &config.locale).expect("sync_all");

    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .expect("HookRunner");

    (tmp, db_pool, runner)
}

fn run(runner: &HookRunner, template: &str, context: Value, crud: RenderCrud) -> Value {
    runner.run_before_render(RenderParams {
        info: RenderInfo::from_context(template, &context),
        context,
        crud,
    })
}

fn no_crud() -> RenderCrud {
    RenderCrud::None {
        user: None,
        ui_locale: None,
    }
}

fn read_only(pool: &DbPool) -> RenderCrud {
    RenderCrud::ReadOnly {
        pool: pool.clone(),
        user: None,
        ui_locale: None,
    }
}

// ── page identity (the second hook argument) ─────────────────────────────

#[test]
fn hook_receives_the_page_identity_as_its_second_argument() {
    let (_tmp, _pool, runner) = setup(
        r#"
        crap.hooks.register("before_render", function(ctx, info)
            ctx.seen_page = info.page
            ctx.seen_template = info.template
            ctx.seen_collection = info.collection
            return ctx
        end)
        "#,
    );

    let ctx = json!({
        "page": { "type": "collection_list" },
        "collection": { "slug": "articles" },
    });
    let out = run(&runner, "collections/items", ctx, no_crud());

    assert_eq!(out["seen_page"], json!("collection_list"));
    assert_eq!(out["seen_template"], json!("collections/items"));
    assert_eq!(out["seen_collection"], json!("articles"));
}

/// The whole point of `info`: a hook can scope itself to one page in a line,
/// instead of guessing from which context keys happen to exist.
#[test]
fn hook_can_bail_out_on_pages_it_does_not_target() {
    let (_tmp, _pool, runner) = setup(
        r#"
        crap.hooks.register("before_render", function(ctx, info)
            if info.page ~= "dashboard" then return end
            ctx.widget = "only on the dashboard"
            return ctx
        end)
        "#,
    );

    let on_list = run(
        &runner,
        "collections/items",
        json!({ "page": { "type": "collection_list" } }),
        no_crud(),
    );
    assert_eq!(on_list.get("widget"), None);

    let on_dashboard = run(
        &runner,
        "dashboard/index",
        json!({ "page": { "type": "dashboard" } }),
        no_crud(),
    );
    assert_eq!(on_dashboard["widget"], json!("only on the dashboard"));
}

// ── one round trip, many hooks ───────────────────────────────────────────

/// Registered hooks share ONE Lua table: the second hook must observe what
/// the first one wrote without the context being converted back to JSON in
/// between.
#[test]
fn later_hooks_observe_earlier_in_place_mutations() {
    let (_tmp, _pool, runner) = setup(
        r#"
        crap.hooks.register("before_render", function(ctx)
            ctx.chain = "first"
        end)
        crap.hooks.register("before_render", function(ctx)
            ctx.chain = ctx.chain .. "+second"
            return ctx
        end)
        "#,
    );

    let out = run(&runner, "dashboard/index", json!({}), no_crud());

    assert_eq!(out["chain"], json!("first+second"));
}

/// A hook that returns a *different* table replaces the context for every
/// hook after it, not just for the renderer.
#[test]
fn a_returned_table_replaces_the_context_for_later_hooks() {
    let (_tmp, _pool, runner) = setup(
        r#"
        crap.hooks.register("before_render", function(ctx)
            return { replaced = true }
        end)
        crap.hooks.register("before_render", function(ctx)
            ctx.saw_replacement = ctx.replaced == true
            return ctx
        end)
        "#,
    );

    let out = run(
        &runner,
        "dashboard/index",
        json!({ "original": 1 }),
        no_crud(),
    );

    assert_eq!(out["replaced"], json!(true));
    assert_eq!(out["saw_replacement"], json!(true));
    assert_eq!(out.get("original"), None, "the replacement table wins");
}

// ── read-only database access ────────────────────────────────────────────

#[test]
fn read_only_crud_lets_a_hook_query_for_page_data() {
    let (_tmp, pool, runner) = setup(
        r#"
        crap.hooks.register("before_render", function(ctx)
            ctx.article_count = crap.collections.articles.count({ override_access = true })
            return ctx
        end)
        "#,
    );

    let out = run(&runner, "dashboard/index", json!({}), read_only(&pool));

    assert_eq!(
        out["article_count"],
        json!(0),
        "a read must succeed under read-only render access; got {out}"
    );
}

#[test]
fn read_only_crud_sees_committed_rows() {
    let (tmp, pool, runner) = setup(
        r#"
        crap.hooks.register("before_render", function(ctx)
            local res = crap.collections.articles.find({ override_access = true })
            ctx.titles = {}
            for _, doc in ipairs(res.documents) do
                table.insert(ctx.titles, doc.title)
            end
            return ctx
        end)
        "#,
    );

    let config = CrapConfig::test_default();
    let registry = hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let def = registry.get_collection("articles").unwrap().clone();
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    query::create(
        &tx,
        "articles",
        &def,
        &serde_json::from_value(json!({ "title": "Hello" })).unwrap(),
        None,
    )
    .expect("create");
    tx.commit().expect("commit");

    let out = run(&runner, "dashboard/index", json!({}), read_only(&pool));

    assert_eq!(out["titles"], json!(["Hello"]));
}

/// The security half of the contract: a page render must not be able to
/// mutate anything. The write is refused with a message that names the
/// alternative, and the context comes back unmodified.
#[test]
fn a_write_from_a_render_hook_is_refused() {
    let (_tmp, pool, runner) = setup(
        r#"
        crap.hooks.register("before_render", function(ctx)
            local ok, err = pcall(function()
                crap.collections.articles.create({ title = "sneaky" }, { override_access = true })
            end)
            ctx.write_ok = ok
            ctx.write_err = tostring(err)
            return ctx
        end)
        "#,
    );

    let out = run(&runner, "dashboard/index", json!({}), read_only(&pool));

    assert_eq!(out["write_ok"], json!(false), "the write must not succeed");
    let err = out["write_err"].as_str().unwrap_or_default();
    assert!(
        err.contains("read-only"),
        "the error should explain that render hooks are read-only, got: {err}"
    );

    // And nothing was persisted.
    let conn = pool.get().expect("conn");
    let rows = conn
        .query_all("SELECT id FROM articles", &[])
        .expect("select");
    assert!(rows.is_empty(), "a refused write must leave no row behind");
}

/// `crap.transaction` wraps a write transaction, so it is refused for the
/// same reason a bare write is — rather than silently downgrading to a read.
#[test]
fn crap_transaction_is_refused_in_a_render_hook() {
    let (_tmp, pool, runner) = setup(
        r#"
        crap.hooks.register("before_render", function(ctx)
            local ok, err = pcall(function()
                crap.transaction(function() end)
            end)
            ctx.tx_ok = ok
            ctx.tx_err = tostring(err)
            return ctx
        end)
        "#,
    );

    let out = run(&runner, "dashboard/index", json!({}), read_only(&pool));

    assert_eq!(out["tx_ok"], json!(false));
    assert!(
        out["tx_err"]
            .as_str()
            .unwrap_or_default()
            .contains("read-only"),
        "got: {}",
        out["tx_err"]
    );
}

/// Unauthenticated pages (login, password reset) and error pages get no
/// database at all — there is no signed-in user to scope a read by, and an
/// error page has to render when the database is what failed.
#[test]
fn pages_without_a_viewer_get_no_database_access() {
    let (_tmp, _pool, runner) = setup(
        r#"
        crap.hooks.register("before_render", function(ctx)
            local ok, err = pcall(function()
                crap.collections.articles.count({ override_access = true })
            end)
            ctx.read_ok = ok
            ctx.read_err = tostring(err)
            return ctx
        end)
        "#,
    );

    let out = run(&runner, "auth/login", json!({}), no_crud());

    assert_eq!(out["read_ok"], json!(false));
    assert!(
        out["read_err"]
            .as_str()
            .unwrap_or_default()
            .contains("require a transaction or pool"),
        "got: {}",
        out["read_err"]
    );
}

/// A hook that raises must not take the page down: the render continues with
/// whatever the surviving hooks produced.
#[test]
fn a_failing_hook_does_not_lose_the_other_hooks_work() {
    let (_tmp, _pool, runner) = setup(
        r#"
        crap.hooks.register("before_render", function(ctx)
            ctx.before_the_error = true
            return ctx
        end)
        crap.hooks.register("before_render", function(ctx)
            error("boom")
        end)
        crap.hooks.register("before_render", function(ctx)
            ctx.after_the_error = true
            return ctx
        end)
        "#,
    );

    let out = run(&runner, "dashboard/index", json!({}), no_crud());

    assert_eq!(out["before_the_error"], json!(true));
    assert_eq!(out["after_the_error"], json!(true));
}
