#![allow(
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value,
    clippy::too_many_lines,
    clippy::used_underscore_binding
)]

//! `before_render` hooks and `crap.template_data` functions against real
//! rendered admin pages.
//!
//! The unit tests in `tests/before_render_hooks.rs` drive the hook runner
//! directly. These drive the actual HTTP render path, so they also cover the
//! wiring: that each page hands the hook the right template name, that the
//! mutated context reaches Handlebars, and that the read-only database
//! contract holds on a live page.

use std::fs;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crap_cms::config::CrapConfig;
use crap_cms_e2e::helpers::*;

/// A slot file that echoes whatever the hook stashed on the context into a
/// `<meta>` tag, so the test can read it back out of the rendered HTML.
const PROBE_SLOT: &str = r#"<meta name="render-probe" content="{{render_probe}}" />"#;

/// Write `init.lua` + the probe slot into a config dir.
///
/// The probe goes into both `head_extras` (declared by `layout/base.hbs`,
/// which every authenticated page uses) and `login_extras` (declared by the
/// login page, which renders `layout/auth.hbs` instead).
fn write_config_dir(tmp: &tempfile::TempDir, init_lua: &str) {
    fs::write(tmp.path().join("init.lua"), init_lua).expect("write init.lua");

    for slot in ["head_extras", "login_extras"] {
        let slot_dir = tmp.path().join("templates").join("slots").join(slot);
        fs::create_dir_all(&slot_dir).expect("mkdir slot dir");
        fs::write(slot_dir.join("probe.hbs"), PROBE_SLOT).expect("write slot");
    }
}

fn test_config() -> CrapConfig {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.admin.require_auth = false;
    config.admin.dev_mode = true;

    config
}

/// Render `path` as a signed-in admin and return the body.
async fn render(init_lua: &str, email: &str, path: &str) -> String {
    let tmp = tempfile::tempdir().expect("tempdir");
    write_config_dir(&tmp, init_lua);

    let app = setup_app_at(vec![make_users_def()], vec![], test_config(), tmp);
    let user_id = create_test_user(&app, email, "pass123");
    let cookie = make_auth_cookie(&app, &user_id, email);

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get(path)
                .header("Cookie", auth_and_csrf(&cookie))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK, "{path} should render");

    body_string(resp.into_body()).await
}

/// Pull the probe value back out of the rendered `<meta>` tag.
///
/// Accepts either attribute quoting: a slot whose value itself contains a
/// double-quoted helper argument (`{{data "name"}}`) has to be written with
/// single-quoted attributes.
fn probe(body: &str) -> String {
    for (marker, close) in [
        (r#"<meta name="render-probe" content=""#, '"'),
        (r"<meta name='render-probe' content='", '\''),
    ] {
        let Some(at) = body.find(marker) else {
            continue;
        };
        let rest = &body[at + marker.len()..];
        let end = rest.find(close).expect("unterminated probe attribute");

        return rest[..end].to_string();
    }

    panic!("probe meta tag missing from the rendered page");
}

const PROBE_HOOK: &str = r#"
crap.hooks.register("before_render", function(ctx, info)
    ctx.render_probe = table.concat({
        info.page,
        info.template,
        tostring(info.collection),
        tostring(info.global),
    }, "|")
    return ctx
end)
"#;

#[tokio::test]
async fn dashboard_render_reports_its_own_page_and_template() {
    let body = render(PROBE_HOOK, "rh1@test.com", "/admin").await;

    assert_eq!(probe(&body), "dashboard|dashboard/index|nil|nil");
}

#[tokio::test]
async fn a_collection_list_render_reports_its_collection_slug() {
    let body = render(PROBE_HOOK, "rh2@test.com", "/admin/collections/users").await;

    assert_eq!(
        probe(&body),
        "collection_items|collections/items|users|nil",
        "a collection-scoped page must name its collection"
    );
}

/// The point of the read-only tier: a hook can put real content on a page.
#[tokio::test]
async fn a_hook_can_query_the_database_and_render_the_result() {
    let hook = r#"
crap.hooks.register("before_render", function(ctx, info)
    if info.page ~= "dashboard" then return end
    ctx.render_probe = "users:" .. crap.collections.users.count({ override_access = true })
    return ctx
end)
"#;

    let body = render(hook, "rh3@test.com", "/admin").await;

    assert_eq!(
        probe(&body),
        "users:1",
        "the hook's query result should reach the template"
    );
}

/// And the other half of it: the same hook cannot mutate anything.
#[tokio::test]
async fn a_write_from_a_render_hook_is_refused_on_a_live_page() {
    let hook = r#"
crap.hooks.register("before_render", function(ctx, info)
    if info.page ~= "dashboard" then return end
    local ok = pcall(function()
        crap.collections.users.create(
            { email = "smuggled@test.com", password = "pass123" },
            { override_access = true }
        )
    end)
    ctx.render_probe = "write_ok:" .. tostring(ok)
    return ctx
end)
"#;

    let body = render(hook, "rh4@test.com", "/admin").await;

    assert_eq!(probe(&body), "write_ok:false");
    assert!(
        !body.contains("smuggled@test.com"),
        "the refused write must not have created a user"
    );
}

/// A hook that raises must never take an admin page down.
#[tokio::test]
async fn a_raising_hook_still_renders_the_page() {
    let hook = r#"
crap.hooks.register("before_render", function(ctx, info)
    error("hook exploded")
end)
"#;

    let tmp = tempfile::tempdir().expect("tempdir");
    write_config_dir(&tmp, hook);

    let app = setup_app_at(vec![make_users_def()], vec![], test_config(), tmp);
    let user_id = create_test_user(&app, "rh5@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "rh5@test.com");

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin")
                .header("Cookie", auth_and_csrf(&cookie))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "a failing before_render hook must not fail the page"
    );
}

/// The login page runs the hook (so branding/banners still work) but hands
/// it no database — there is no viewer to scope a read by.
#[tokio::test]
async fn the_login_page_runs_the_hook_without_database_access() {
    let hook = r#"
crap.hooks.register("before_render", function(ctx, info)
    if info.page ~= "auth_login" then return end
    local ok = pcall(function()
        crap.collections.users.count({ override_access = true })
    end)
    ctx.render_probe = info.template .. "|read_ok:" .. tostring(ok)
    return ctx
end)
"#;

    let tmp = tempfile::tempdir().expect("tempdir");
    write_config_dir(&tmp, hook);

    let mut config = test_config();
    config.admin.require_auth = true;

    let app = setup_app_at(vec![make_users_def()], vec![], config, tmp);

    let resp = app
        .router
        .clone()
        .oneshot(Request::get("/admin/login").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;

    assert_eq!(probe(&body), "auth/login|read_ok:false");
}

// ── crap.template_data — the sibling render-time extension point ─────────

/// A `{{data "name"}}` function gets the same read-only database access the
/// page's `before_render` hook gets. The two run at the same moment on the
/// same page; giving one a database and not the other would send anyone
/// reaching for the purpose-built helper down a dead end.
#[tokio::test]
async fn a_template_data_function_can_query_the_database() {
    let tmp = tempfile::tempdir().expect("tempdir");
    fs::write(
        tmp.path().join("init.lua"),
        r#"
crap.template_data.register("user_count", function(ctx)
    return "users:" .. crap.collections.users.count({ override_access = true })
end)
"#,
    )
    .expect("write init.lua");

    // This slot calls the helper instead of reading a hook-set field.
    let slot_dir = tmp
        .path()
        .join("templates")
        .join("slots")
        .join("head_extras");
    fs::create_dir_all(&slot_dir).expect("mkdir slot dir");
    fs::write(
        slot_dir.join("probe.hbs"),
        r#"<meta name='render-probe' content='{{data "user_count"}}' />"#,
    )
    .expect("write slot");

    let app = setup_app_at(vec![make_users_def()], vec![], test_config(), tmp);
    let user_id = create_test_user(&app, "td1@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "td1@test.com");

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin")
                .header("Cookie", auth_and_csrf(&cookie))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;

    assert_eq!(
        probe(&body),
        "users:1",
        "a template_data function should reach the database like before_render does"
    );
}

/// And is refused a write for the same reasons.
#[tokio::test]
async fn a_write_from_a_template_data_function_is_refused() {
    let tmp = tempfile::tempdir().expect("tempdir");
    fs::write(
        tmp.path().join("init.lua"),
        r#"
crap.template_data.register("sneaky", function(ctx)
    local ok = pcall(function()
        crap.collections.users.create(
            { email = "smuggled@test.com", password = "pass123" },
            { override_access = true }
        )
    end)
    return "write_ok:" .. tostring(ok)
end)
"#,
    )
    .expect("write init.lua");

    let slot_dir = tmp
        .path()
        .join("templates")
        .join("slots")
        .join("head_extras");
    fs::create_dir_all(&slot_dir).expect("mkdir slot dir");
    fs::write(
        slot_dir.join("probe.hbs"),
        r#"<meta name='render-probe' content='{{data "sneaky"}}' />"#,
    )
    .expect("write slot");

    let app = setup_app_at(vec![make_users_def()], vec![], test_config(), tmp);
    let user_id = create_test_user(&app, "td2@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "td2@test.com");

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin")
                .header("Cookie", auth_and_csrf(&cookie))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let body = body_string(resp.into_body()).await;

    assert_eq!(probe(&body), "write_ok:false");
    assert!(
        !body.contains("smuggled@test.com"),
        "the refused write must not have created a user"
    );
}

/// The unauthenticated login page gives `template_data` no database either —
/// the same carve-out `before_render` gets, for the same reason.
#[tokio::test]
async fn template_data_on_the_login_page_has_no_database_access() {
    let tmp = tempfile::tempdir().expect("tempdir");
    fs::write(
        tmp.path().join("init.lua"),
        r#"
crap.template_data.register("probe", function(ctx)
    local ok = pcall(function()
        crap.collections.users.count({ override_access = true })
    end)
    return "read_ok:" .. tostring(ok)
end)
"#,
    )
    .expect("write init.lua");

    let slot_dir = tmp
        .path()
        .join("templates")
        .join("slots")
        .join("login_extras");
    fs::create_dir_all(&slot_dir).expect("mkdir slot dir");
    fs::write(
        slot_dir.join("probe.hbs"),
        r#"<meta name='render-probe' content='{{data "probe"}}' />"#,
    )
    .expect("write slot");

    let mut config = test_config();
    config.admin.require_auth = true;

    let app = setup_app_at(vec![make_users_def()], vec![], config, tmp);

    let resp = app
        .router
        .clone()
        .oneshot(Request::get("/admin/login").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;

    assert_eq!(probe(&body), "read_ok:false");
}

// ── the form-error re-render path ────────────────────────────────────────

/// `page_with_toast` is its own blocking render — the path a form takes when
/// validation fails — and it re-renders the page context through the same
/// hook. Worth its own test: if only the happy path worked, submitting an
/// invalid form would break for everyone with a `before_render` hook, and
/// nothing else here would notice.
#[tokio::test]
async fn a_failed_form_submit_still_runs_the_hook_and_keeps_its_toast() {
    let tmp = tempfile::tempdir().expect("tempdir");
    write_config_dir(&tmp, PROBE_HOOK);

    let mut def = crap_cms::core::CollectionDefinition::new("articles");
    def.fields = vec![
        crap_cms::core::field::FieldDefinition::builder(
            "title",
            crap_cms::core::field::FieldType::Text,
        )
        .required(true)
        .build(),
    ];

    let app = setup_app_at(vec![make_users_def(), def], vec![], test_config(), tmp);
    let user_id = create_test_user(&app, "rh6@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "rh6@test.com");

    // `title` is required and empty — the handler re-renders the form.
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/collections/articles")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title="))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "a validation failure re-renders the form rather than redirecting"
    );
    assert!(
        resp.headers().contains_key("X-Crap-Toast"),
        "the re-render must keep its toast header"
    );

    let body = body_string(resp.into_body()).await;

    assert_eq!(
        probe(&body),
        "collection_create|collections/edit|articles|nil",
        "the hook should run on the re-rendered form, with the page identity"
    );
}
