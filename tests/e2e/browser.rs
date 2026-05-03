use std::net::SocketAddr;
use std::time::Duration;

use chromiumoxide::Browser;
use chromiumoxide::BrowserConfig;
use chromiumoxide::Element;
use chromiumoxide::Page;
use tokio::task::JoinHandle;
use tokio_stream::StreamExt;

use crap_cms::config::CrapConfig;
use crap_cms::core::collection::{CollectionDefinition, GlobalDefinition};

use crate::helpers::{self, TestApp};

/// Spawn a real HTTP server bound to 127.0.0.1:0 and return the base URL,
/// a join handle for the server task, and the TestApp.
pub async fn spawn_server(
    collections: Vec<CollectionDefinition>,
    globals: Vec<GlobalDefinition>,
) -> (String, JoinHandle<()>, TestApp) {
    let app = helpers::setup_app(collections, globals);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    let base_url = format!("http://{addr}");

    let router = app.router.clone();
    let handle = tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });

    (base_url, handle, app)
}

/// Like `spawn_server` but with a custom `CrapConfig` (e.g. for locale tests).
pub async fn spawn_server_with_config(
    collections: Vec<CollectionDefinition>,
    globals: Vec<GlobalDefinition>,
    config: CrapConfig,
) -> (String, JoinHandle<()>, TestApp) {
    let app = helpers::setup_app_with_config(collections, globals, config);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    let base_url = format!("http://{addr}");

    let router = app.router.clone();
    let handle = tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });

    (base_url, handle, app)
}

/// Find an element by selector, retrying briefly while the document
/// settles. Use right after `page.goto()` where chromiumoxide can
/// transiently report stale-node errors against the previous frame's
/// DOM before the new document has finished installing. Sleeps between
/// attempts so the CDP transport has time to process navigation events.
/// Panics with the selector after exhausting the retry budget — the
/// caller's intent ("this element must be present") is unchanged.
pub async fn find_element_after_nav(page: &Page, selector: &str) -> Element {
    for _ in 0..40 {
        if let Ok(el) = page.find_element(selector).await {
            return el;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("element not found after retry budget: {selector}");
}

/// Evaluate JS that returns a string from within a shadow root.
pub async fn shadow_eval(page: &Page, host_selector: &str, js: &str) -> String {
    let script = format!(
        "() => {{ const host = document.querySelector('{host_selector}'); \
         if (!host || !host.shadowRoot) return ''; \
         return (function(root) {{ {js} }})(host.shadowRoot); }}"
    );
    let result = page.evaluate(script).await.unwrap();
    result.into_value::<String>().unwrap_or_default()
}

/// Launch a headless Chrome browser. Returns the browser and a join handle
/// for the websocket event loop.
pub async fn launch_browser() -> (Browser, JoinHandle<()>) {
    let (browser, mut handler) = Browser::launch(
        BrowserConfig::builder()
            .no_sandbox()
            .arg("--headless=new")
            .build()
            .unwrap(),
    )
    .await
    .unwrap();

    let handle = tokio::spawn(async move { while handler.next().await.is_some() {} });

    (browser, handle)
}

/// Log in via the browser by navigating to the login page, filling
/// email/password, and submitting.
pub async fn browser_login(page: &Page, base_url: &str, email: &str, password: &str) {
    page.goto(format!("{base_url}/admin/login")).await.unwrap();
    // Wait for page to fully load
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    page.find_element("input[name=\"email\"]")
        .await
        .unwrap()
        .click()
        .await
        .unwrap()
        .type_str(email)
        .await
        .unwrap();

    page.find_element("input[name=\"password\"]")
        .await
        .unwrap()
        .click()
        .await
        .unwrap()
        .type_str(password)
        .await
        .unwrap();

    page.find_element("button[type=\"submit\"]")
        .await
        .unwrap()
        .click()
        .await
        .unwrap();

    // Wait for login redirect to complete
    tokio::time::sleep(std::time::Duration::from_millis(2000)).await;
}
