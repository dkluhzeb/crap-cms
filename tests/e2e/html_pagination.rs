//! Pagination e2e tests — cursor-based and page-based navigation consistency.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use crap_cms::config::{CrapConfig, PaginationMode};
use crap_cms::core::collection::*;
use crap_cms::core::field::*;
use crap_cms::db::DbConnection;

use crate::helpers::*;
use crate::html;

fn make_posts_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Post".to_string())),
        plural: Some(LocalizedString::Plain("Posts".to_string())),
    };
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("published_at", FieldType::Date).build(),
    ];
    def.admin = AdminConfig {
        use_as_title: Some("title".to_string()),
        default_sort: Some("-published_at".to_string()),
        ..AdminConfig::default()
    };
    def
}

struct Ctx {
    app: TestApp,
    cookie: String,
}

fn setup_cursor(limit: i64) -> Ctx {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.admin.require_auth = true;
    config.pagination.default_limit = limit;
    config.pagination.mode = PaginationMode::Cursor;
    let app = setup_app_with_config(vec![make_posts_def(), make_users_def()], vec![], config);
    let user_id = create_test_user(&app, "test@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "test@test.com");
    Ctx { app, cookie }
}

fn setup_paged(limit: i64) -> Ctx {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.admin.require_auth = true;
    config.pagination.default_limit = limit;
    config.pagination.mode = PaginationMode::Page;
    let app = setup_app_with_config(vec![make_posts_def(), make_users_def()], vec![], config);
    let user_id = create_test_user(&app, "test@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "test@test.com");
    Ctx { app, cookie }
}

fn seed_posts(app: &TestApp, count: usize) {
    let conn = app.pool.get().unwrap();
    for i in 1..=count {
        conn.execute(
            &format!(
                "INSERT INTO posts (id, title, published_at, created_at, updated_at) \
                 VALUES ('p{i:03}', 'Post {i}', '2024-{m:02}-{d:02}T10:00:00.000Z', \
                 '2024-{m:02}-{d:02}T10:00:00.000Z', '2024-{m:02}-{d:02}T10:00:00.000Z')",
                m = (i - 1) / 28 + 1,
                d = (i - 1) % 28 + 1,
            ),
            &[],
        )
        .unwrap();
    }
}

async fn get(ctx: &Ctx, url: &str) -> (String, StatusCode) {
    let resp = ctx
        .app
        .router
        .clone()
        .oneshot(
            Request::get(url)
                .header("cookie", &ctx.cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (String::from_utf8_lossy(&bytes).to_string(), status)
}

fn count_rows(body: &str) -> usize {
    let doc = html::parse(body);
    html::select_all(&doc, ".items-row__link").len()
}

/// Extract a pagination link containing the given cursor parameter.
fn cursor_link(body: &str, param: &str) -> Option<String> {
    let decoded = body.replace("&#x3D;", "=").replace("&amp;", "&");
    for chunk in decoded.split("href=\"/admin/collections/posts?").skip(1) {
        let end = chunk.find('"').unwrap_or(chunk.len());
        let url = format!("/admin/collections/posts?{}", &chunk[..end]);
        if url.contains(param) {
            return Some(url);
        }
    }
    None
}

// ── Cursor-based pagination ──────────────────────────────────────────────

#[tokio::test]
async fn cursor_first_page_has_next_no_prev() {
    let ctx = setup_cursor(5);
    seed_posts(&ctx.app, 12);

    let (body, status) = get(&ctx, "/admin/collections/posts").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(count_rows(&body), 5);
    assert!(
        cursor_link(&body, "after_cursor").is_some(),
        "Should have Next"
    );
    assert!(
        cursor_link(&body, "before_cursor").is_none(),
        "Should NOT have Prev"
    );
}

#[tokio::test]
async fn cursor_last_page_has_prev_no_next() {
    let ctx = setup_cursor(5);
    seed_posts(&ctx.app, 12);

    let (p1, _) = get(&ctx, "/admin/collections/posts").await;
    let next1 = cursor_link(&p1, "after_cursor").unwrap();
    let (p2, _) = get(&ctx, &next1).await;
    let next2 = cursor_link(&p2, "after_cursor").unwrap();
    let (p3, _) = get(&ctx, &next2).await;

    assert_eq!(count_rows(&p3), 2);
    assert!(
        cursor_link(&p3, "after_cursor").is_none(),
        "Last page: no Next"
    );
    assert!(
        cursor_link(&p3, "before_cursor").is_some(),
        "Last page: has Prev"
    );
}

#[tokio::test]
async fn cursor_single_page_no_navigation() {
    let ctx = setup_cursor(20);
    seed_posts(&ctx.app, 5);

    let (body, _) = get(&ctx, "/admin/collections/posts").await;
    assert_eq!(count_rows(&body), 5);
    assert!(cursor_link(&body, "after_cursor").is_none(), "No Next");
    assert!(cursor_link(&body, "before_cursor").is_none(), "No Prev");
}

#[tokio::test]
async fn cursor_forward_back_forward_consistent() {
    let ctx = setup_cursor(10);
    seed_posts(&ctx.app, 25);

    // Page 1 → Page 2 → Page 3
    let (p1, _) = get(&ctx, "/admin/collections/posts").await;
    assert_eq!(count_rows(&p1), 10);

    let (p2, _) = get(&ctx, &cursor_link(&p1, "after_cursor").unwrap()).await;
    let p2_count = count_rows(&p2);
    assert_eq!(p2_count, 10);

    let (p3, _) = get(&ctx, &cursor_link(&p2, "after_cursor").unwrap()).await;
    let p3_count = count_rows(&p3);
    assert_eq!(p3_count, 5);

    // Back to page 2, then forward to page 3 again
    let (p2b, _) = get(&ctx, &cursor_link(&p3, "before_cursor").unwrap()).await;
    assert_eq!(count_rows(&p2b), p2_count, "Page 2 should be consistent");

    let (p3b, _) = get(&ctx, &cursor_link(&p2b, "after_cursor").unwrap()).await;
    assert_eq!(
        count_rows(&p3b),
        p3_count,
        "Page 3 should be consistent after round-trip"
    );
}

#[tokio::test]
async fn cursor_back_to_first_page_no_prev() {
    let ctx = setup_cursor(10);
    seed_posts(&ctx.app, 15);

    let (p1, _) = get(&ctx, "/admin/collections/posts").await;
    let (p2, _) = get(&ctx, &cursor_link(&p1, "after_cursor").unwrap()).await;
    let (p1b, _) = get(&ctx, &cursor_link(&p2, "before_cursor").unwrap()).await;

    assert_eq!(count_rows(&p1b), 10);
    assert!(
        cursor_link(&p1b, "before_cursor").is_none(),
        "First page: no Prev"
    );
    assert!(
        cursor_link(&p1b, "after_cursor").is_some(),
        "First page: has Next"
    );
}

#[tokio::test]
async fn cursor_urls_have_no_page_param() {
    let ctx = setup_cursor(5);
    seed_posts(&ctx.app, 12);

    let (body, _) = get(&ctx, "/admin/collections/posts").await;
    let next = cursor_link(&body, "after_cursor").unwrap();
    assert!(
        !next.contains("page="),
        "Cursor URL should not contain page=: {next}"
    );
}

// ── Page-based pagination ────────────────────────────────────────────────

fn page_link(body: &str, param: &str) -> Option<String> {
    let decoded = body.replace("&#x3D;", "=").replace("&amp;", "&");
    for chunk in decoded.split("href=\"/admin/collections/posts?").skip(1) {
        let end = chunk.find('"').unwrap_or(chunk.len());
        let url = format!("/admin/collections/posts?{}", &chunk[..end]);
        if url.contains(param) {
            return Some(url);
        }
    }
    None
}

#[tokio::test]
async fn page_first_page_has_next_no_prev() {
    let ctx = setup_paged(5);
    seed_posts(&ctx.app, 12);

    let (body, _) = get(&ctx, "/admin/collections/posts").await;
    assert_eq!(count_rows(&body), 5);
    assert!(
        page_link(&body, "page=2").is_some(),
        "First page: has Next (page=2)"
    );
    // No page=0 link
    assert!(page_link(&body, "page=0").is_none(), "First page: no Prev");
}

#[tokio::test]
async fn page_last_page_no_next() {
    let ctx = setup_paged(5);
    seed_posts(&ctx.app, 12);

    let (body, _) = get(&ctx, "/admin/collections/posts?page=3").await;
    assert_eq!(count_rows(&body), 2);
    assert!(page_link(&body, "page=4").is_none(), "Last page: no Next");
    assert!(page_link(&body, "page=2").is_some(), "Last page: has Prev");
}

#[tokio::test]
async fn page_single_page_no_navigation() {
    let ctx = setup_paged(20);
    seed_posts(&ctx.app, 5);

    let (body, _) = get(&ctx, "/admin/collections/posts").await;
    assert_eq!(count_rows(&body), 5);
    // No pagination links at all
    assert!(page_link(&body, "page=2").is_none(), "No page=2");
    assert!(page_link(&body, "page=0").is_none(), "No page=0");
}
