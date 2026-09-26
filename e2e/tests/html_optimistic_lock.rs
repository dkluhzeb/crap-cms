#![allow(
    clippy::missing_panics_doc,
    clippy::similar_names,
    clippy::too_many_lines
)]

//! Concurrent editing in the admin: every edit form carries the revision it
//! was loaded at (`_revision`), and a save from a form another editor has
//! saved over since is refused with the conflict page instead of silently
//! overwriting that change. The conflict page keeps the editor's values and
//! carries the current revision, so saving it again overwrites on purpose.

use std::collections::HashMap;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use serde_json::json;
use tower::ServiceExt;

use crap_cms::core::DocumentFields;
use crap_cms::core::collection::*;
use crap_cms::core::field::*;
use crap_cms::db::query;
use crap_cms_e2e::helpers::*;

/// The English title of the revision-conflict notice.
const CONFLICT_TITLE: &str = "Someone else saved this document after you opened it.";

// ── collections ──────────────────────────────────────────────────────────

/// Two editors open the same document; the first save lands, the second —
/// from a form loaded before it — gets the conflict page with its own values
/// still in the form, and the document keeps the first editor's change.
/// Saving the conflict page again overwrites it on purpose.
#[tokio::test]
async fn a_save_from_a_stale_form_gets_the_conflict_page_and_can_overwrite() {
    let HtmlTestCtx { app, cookie, .. } = setup_html_test(
        vec![make_concurrent_posts_def(), make_users_def()],
        vec![],
        "concur@test.com",
        "pass1234",
    );
    let post_id = seed_post(&app, "Original Title");
    let path = format!("/admin/collections/posts/{post_id}");

    // Both editors load the form at the same revision.
    let revision_a = rendered_revision(&get_page(&app, &path, &cookie).await);
    let revision_b = rendered_revision(&get_page(&app, &path, &cookie).await);
    assert_eq!(revision_a, "0");
    assert_eq!(revision_b, "0");

    // Editor A saves first.
    let resp_a = post_form(&app, &path, &cookie, "Edit from A", Some(&revision_a)).await;
    assert!(
        resp_a.status().is_success() || resp_a.status().is_redirection(),
        "the first save lands, got: {}",
        resp_a.status()
    );
    assert!(!page_of(resp_a).await.contains(CONFLICT_TITLE));

    // Editor B saves from the form loaded before A's save.
    let resp_b = post_form(&app, &path, &cookie, "Edit from B", Some(&revision_b)).await;
    assert_eq!(resp_b.status(), StatusCode::OK, "the form is re-rendered");
    let conflict_page = page_of(resp_b).await;
    assert!(
        conflict_page.contains(CONFLICT_TITLE),
        "the conflict notice is shown"
    );
    assert!(
        conflict_page.contains(r#"value="Edit from B""#),
        "the refused editor keeps their unsaved value"
    );
    assert!(
        conflict_page.contains(&format!(r#"href="{path}""#)),
        "the notice offers a reload of the saved document"
    );
    assert_eq!(
        rendered_revision(&conflict_page),
        "1",
        "the re-rendered form carries the current revision"
    );

    let after_conflict = get_page(&app, &path, &cookie).await;
    assert!(
        after_conflict.contains(r#"value="Edit from A""#),
        "the refused save changed nothing"
    );

    // Overwrite: the conflict page's form submitted again, at the current
    // revision.
    let overwrite = rendered_revision(&conflict_page);
    let resp_overwrite = post_form(&app, &path, &cookie, "Edit from B", Some(&overwrite)).await;
    assert!(
        resp_overwrite.status().is_success() || resp_overwrite.status().is_redirection(),
        "the overwrite lands, got: {}",
        resp_overwrite.status()
    );
    assert!(!page_of(resp_overwrite).await.contains(CONFLICT_TITLE));

    let final_page = get_page(&app, &path, &cookie).await;
    assert!(final_page.contains(r#"value="Edit from B""#));
    assert!(!final_page.contains(r#"value="Edit from A""#));
    assert_eq!(rendered_revision(&final_page), "2");
}

/// An unpublish posts only the form's meta inputs, never its field values —
/// so a stale one must not come back as a re-rendered form (every field
/// blank, and a save from it would blank the document). It is refused with a
/// conflict toast and changes nothing.
#[tokio::test]
async fn a_stale_unpublish_is_refused_with_a_toast_and_changes_nothing() {
    let mut def = make_concurrent_posts_def();
    def.versions = Some(VersionsConfig::new(true, 10));
    let HtmlTestCtx { app, cookie, .. } = setup_html_test(
        vec![def, make_users_def()],
        vec![],
        "concur-unpublish@test.com",
        "pass1234",
    );
    let post_id = seed_post(&app, "Original Title");
    let path = format!("/admin/collections/posts/{post_id}");

    let stale = rendered_revision(&get_page(&app, &path, &cookie).await);

    let publish = post_form(&app, &path, &cookie, "Edit from A", Some(&stale)).await;
    assert!(
        publish.status().is_success() || publish.status().is_redirection(),
        "the first save lands, got: {}",
        publish.status()
    );

    let body = format!("_action=unpublish&_revision={stale}");
    let resp = post(&app, &path, &cookie, body).await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);

    let toast = resp
        .headers()
        .get("X-Crap-Toast")
        .expect("a conflict toast")
        .to_str()
        .unwrap()
        .to_string();
    assert!(toast.contains("unpublish again"), "{toast}");
    assert!(
        !page_of(resp).await.contains(CONFLICT_TITLE),
        "no blank form comes back"
    );

    let conn = app.pool.get().unwrap();
    assert_eq!(
        query::get_document_status(&conn, "posts", &post_id)
            .unwrap()
            .as_deref(),
        Some("published"),
        "the refused unpublish changed nothing"
    );
    assert!(
        get_page(&app, &path, &cookie)
            .await
            .contains(r#"value="Edit from A""#)
    );
}

/// A save carrying no revision (a hand-built form post, a template override
/// without the input) writes unconditionally, as every save did before.
#[tokio::test]
async fn a_save_without_a_revision_writes_unconditionally() {
    let HtmlTestCtx { app, cookie, .. } = setup_html_test(
        vec![make_concurrent_posts_def(), make_users_def()],
        vec![],
        "concur-blind@test.com",
        "pass1234",
    );
    let post_id = seed_post(&app, "Original Title");
    let path = format!("/admin/collections/posts/{post_id}");

    for title in ["Edit from A", "Edit from B"] {
        let resp = post_form(&app, &path, &cookie, title, None).await;
        assert!(
            resp.status().is_success() || resp.status().is_redirection(),
            "{title}: {}",
            resp.status()
        );
    }

    let body = get_page(&app, &path, &cookie).await;
    assert!(body.contains(r#"value="Edit from B""#));
    assert_eq!(rendered_revision(&body), "2");
}

// ── globals ──────────────────────────────────────────────────────────────

/// The global edit form carries its revision the same way, and a save from a
/// stale global form is refused with the conflict page.
#[tokio::test]
async fn a_save_from_a_stale_global_form_gets_the_conflict_page() {
    let HtmlTestCtx { app, cookie, .. } = setup_html_test(
        vec![make_users_def()],
        vec![make_settings_def()],
        "concur-global@test.com",
        "pass1234",
    );
    let path = "/admin/globals/settings";

    let loaded = rendered_revision(&get_page(&app, path, &cookie).await);

    let first = post_global_form(&app, path, &cookie, "Site A", &loaded).await;
    assert!(
        first.status().is_success() || first.status().is_redirection(),
        "the first save lands, got: {}",
        first.status()
    );

    let second = post_global_form(&app, path, &cookie, "Site B", &loaded).await;
    assert_eq!(second.status(), StatusCode::OK);
    let conflict_page = page_of(second).await;
    assert!(conflict_page.contains(CONFLICT_TITLE));
    assert!(conflict_page.contains(r#"value="Site B""#));

    let current = rendered_revision(&conflict_page);
    assert_ne!(
        current, loaded,
        "the conflict page carries the new revision"
    );

    let overwrite = post_global_form(&app, path, &cookie, "Site B", &current).await;
    assert!(
        overwrite.status().is_success() || overwrite.status().is_redirection(),
        "the overwrite lands, got: {}",
        overwrite.status()
    );
    assert!(
        get_page(&app, path, &cookie)
            .await
            .contains(r#"value="Site B""#)
    );
}

// ── helpers ──────────────────────────────────────────────────────────────

fn make_concurrent_posts_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Post".to_string())),
        plural: Some(LocalizedString::Plain("Posts".to_string())),
    };
    def.timestamps = true;
    def.admin = AdminConfig {
        use_as_title: Some("title".to_string()),
        ..AdminConfig::default()
    };
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
    ];
    def
}

fn seed_post(app: &TestApp, title: &str) -> String {
    let def = app.registry.get_collection("posts").unwrap().clone();
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data: DocumentFields = HashMap::from([("title".to_string(), json!(title))]).into();
    let doc = query::create(&tx, "posts", &def, &data, None).unwrap();
    tx.commit().unwrap();
    doc.id.to_string()
}

/// The value of the form's hidden `_revision` input.
fn rendered_revision(body: &str) -> String {
    let at = body
        .find(r#"name="_revision""#)
        .expect("the edit form renders a _revision input");
    let rest = &body[at..];
    let value_at = rest.find(r#"value=""#).expect("the input has a value") + r#"value=""#.len();
    let value = &rest[value_at..];

    value[..value.find('"').expect("closing quote")].to_string()
}

/// Encode one form value (`application/x-www-form-urlencoded`).
fn form_value(value: &str) -> String {
    value.replace(' ', "+")
}

async fn post_form(
    app: &TestApp,
    path: &str,
    cookie: &str,
    title: &str,
    revision: Option<&str>,
) -> Response {
    let title = form_value(title);

    let body = match revision {
        Some(revision) => format!("title={title}&_revision={revision}"),
        None => format!("title={title}"),
    };

    post(app, path, cookie, body).await
}

async fn post_global_form(
    app: &TestApp,
    path: &str,
    cookie: &str,
    site_name: &str,
    revision: &str,
) -> Response {
    let body = format!("site_name={}&_revision={revision}", form_value(site_name));

    post(app, path, cookie, body).await
}

async fn post(app: &TestApp, path: &str, cookie: &str, body: String) -> Response {
    app.router
        .clone()
        .oneshot(
            Request::post(path)
                .header("Cookie", auth_and_csrf(cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn page_of(resp: Response) -> String {
    body_string(resp.into_body()).await
}

async fn get_page(app: &TestApp, path: &str, cookie: &str) -> String {
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get(path)
                .header("Cookie", auth_and_csrf(cookie))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    body_string(resp.into_body()).await
}
