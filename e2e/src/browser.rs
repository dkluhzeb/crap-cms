use std::{net::SocketAddr, time::Duration};

use chromiumoxide::{Browser, BrowserConfig, Element, Page};
use tokio::{
    net::TcpListener,
    task::JoinHandle,
    time::{sleep, timeout},
};
use tokio_stream::StreamExt;

use crap_cms::{
    config::CrapConfig,
    core::collection::{CollectionDefinition, GlobalDefinition},
};

use crate::helpers::{self, TestApp};

/// Spawn a real HTTP server bound to 127.0.0.1:0 and return the base URL,
/// a join handle for the server task, and the `TestApp`.
pub async fn spawn_server(
    collections: Vec<CollectionDefinition>,
    globals: Vec<GlobalDefinition>,
) -> (String, JoinHandle<()>, TestApp) {
    let app = helpers::setup_app(collections, globals);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
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
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
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
        sleep(Duration::from_millis(50)).await;
    }
    panic!("element not found after retry budget: {selector}");
}

/// Poll until `selector` matches exactly `count` elements, or a ~3s budget runs
/// out, then return the final matches. Use after an action that mutates the DOM
/// asynchronously (an array row clone/remove, a live re-render) instead of a
/// fixed `sleep` — the fixed delay races the browser under load and is the
/// source of intermittent count-assertion flakiness in the full suite. The
/// final query result is returned even when it isn't `count`, so the caller's
/// assertion still runs and reports the real mismatch.
pub async fn wait_for_element_count(page: &Page, selector: &str, count: usize) -> Vec<Element> {
    let mut last = Vec::new();

    for _ in 0..60 {
        if let Some(els) = poll_find(page, selector).await {
            if els.len() == count {
                return els;
            }
            last = els;
        }
        sleep(Duration::from_millis(50)).await;
    }

    last
}

/// Click `click_sel`, then poll for `expect_sel` to match exactly `count`
/// elements — re-clicking when the expected state hasn't appeared yet.
///
/// Exists for handlers bound on custom-element *upgrade* (e.g. the
/// `<crap-collapsible>` toggle): a click dispatched before the component's
/// module has loaded is silently swallowed. Re-clicking is safe even for
/// toggles, because a swallowed click leaves the observable state unchanged
/// and the loop stops on the first click that lands.
pub async fn click_until_element_count(
    page: &Page,
    click_sel: &str,
    expect_sel: &str,
    count: usize,
) -> Vec<Element> {
    let mut last = Vec::new();

    for _ in 0..20 {
        // Never re-click a satisfied toggle: the previous click may have
        // landed after its poll window closed, and clicking again would
        // toggle the state back.
        if let Some(els) = poll_find(page, expect_sel).await {
            if els.len() == count {
                return els;
            }
            last = els;
        }

        if let Ok(el) = page.find_element(click_sel).await {
            let _ = el.click().await;
        }

        // Give the click a short window to take effect before re-clicking.
        for _ in 0..6 {
            if let Some(els) = poll_find(page, expect_sel).await {
                if els.len() == count {
                    return els;
                }
                last = els;
            }
            sleep(Duration::from_millis(50)).await;
        }
    }

    last
}

/// Poll until `selector` matches at least one element, or a ~3s budget runs
/// out. Returns whether it appeared. Use for "this element should show up after
/// an async action" where the caller doesn't need the element handle itself
/// (otherwise use [`find_element_after_nav`], which returns it).
pub async fn wait_for_element(page: &Page, selector: &str) -> bool {
    for _ in 0..60 {
        if let Some(els) = poll_find(page, selector).await
            && !els.is_empty()
        {
            return true;
        }
        sleep(Duration::from_millis(50)).await;
    }
    false
}

/// Poll until `selector` matches no elements (a row removed, a toast dismissed,
/// a dialog closed), or the budget runs out. Returns whether it became empty.
pub async fn wait_for_element_gone(page: &Page, selector: &str) -> bool {
    for _ in 0..60 {
        if let Some(els) = poll_find(page, selector).await
            && els.is_empty()
        {
            return true;
        }
        sleep(Duration::from_millis(50)).await;
    }
    false
}

/// Poll until the JS expression `predicate` evaluates truthy, or a ~3s budget
/// runs out. Returns whether it became true. The general-purpose "wait until the
/// page reaches a state" helper — a value changed, a class toggled, an element
/// became visible/hidden — replacing a fixed sleep with an observable signal.
/// `predicate` is a JS expression (not a statement), e.g.
/// `document.querySelectorAll('.row').length === 2`.
pub async fn wait_for_js(page: &Page, predicate: &str) -> bool {
    let script =
        format!("() => {{ try {{ return !!({predicate}); }} catch (e) {{ return false; }} }}");

    for _ in 0..60 {
        // A per-call timeout is essential: while the page is navigating (e.g. a
        // redirect after save), the JS execution context is torn down and a
        // pending `evaluate` can never resolve. Bounding each call lets the loop
        // retry against the new context instead of deadlocking on one await.
        let truthy = match timeout(Duration::from_secs(1), page.evaluate(script.clone())).await {
            Ok(Ok(v)) => v.into_value::<bool>().unwrap_or(false),
            _ => false,
        };
        if truthy {
            return true;
        }
        sleep(Duration::from_millis(50)).await;
    }
    false
}

/// One bounded `find_elements` poll. `None` = the call errored or timed out
/// (the execution context is churning during a navigation); the caller retries.
/// The timeout is what prevents a wedged CDP call from deadlocking a poll loop.
async fn poll_find(page: &Page, selector: &str) -> Option<Vec<Element>> {
    match timeout(Duration::from_secs(1), page.find_elements(selector)).await {
        Ok(Ok(els)) => Some(els),
        _ => None,
    }
}

/// Evaluate JS that returns a string from within a shadow root. Returns an empty
/// string on error or timeout (rather than panicking) so it's safe to call in a
/// poll loop — a transient failure while the page is settling just retries.
pub async fn shadow_eval(page: &Page, host_selector: &str, js: &str) -> String {
    let script = format!(
        "() => {{ const host = document.querySelector('{host_selector}'); \
         if (!host || !host.shadowRoot) return ''; \
         return (function(root) {{ {js} }})(host.shadowRoot); }}"
    );
    match timeout(Duration::from_secs(1), page.evaluate(script)).await {
        Ok(Ok(v)) => v.into_value::<String>().unwrap_or_default(),
        _ => String::new(),
    }
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
    // The login form's DOM may not be queryable the instant `goto`
    // resolves — use the retry helper instead of a fixed sleep.
    find_element_after_nav(page, "input[name=\"email\"]")
        .await
        .click()
        .await
        .unwrap()
        .type_str(email)
        .await
        .unwrap();

    find_element_after_nav(page, "input[name=\"password\"]")
        .await
        .click()
        .await
        .unwrap()
        .type_str(password)
        .await
        .unwrap();

    find_element_after_nav(page, "button[type=\"submit\"]")
        .await
        .click()
        .await
        .unwrap();

    // Wait for the login → /admin redirect to settle. A fixed sleep
    // was racy (CI hit < 2s of post-submit time and the next
    // navigation landed back on /admin/login). Poll the URL instead;
    // we know we've left /admin/login once the path changes.
    for _ in 0..60 {
        let path = page
            .evaluate("() => location.pathname")
            .await
            .ok()
            .and_then(|v| v.into_value::<String>().ok())
            .unwrap_or_default();
        if !path.starts_with("/admin/login") {
            return;
        }
        sleep(Duration::from_millis(100)).await;
    }
    panic!("browser_login did not redirect away from /admin/login after 6s");
}

pub struct BrowserTestCtx {
    pub app: TestApp,
    pub base_url: String,
    pub server_handle: JoinHandle<()>,
    pub user_id: String,
    pub browser: Browser,
    pub _browser_handle: JoinHandle<()>,
    pub page: Page,
}

pub async fn setup_browser_test(
    collections: Vec<CollectionDefinition>,
    globals: Vec<GlobalDefinition>,
    email: &str,
    password: &str,
) -> BrowserTestCtx {
    let (base_url, server_handle, app) = spawn_server(collections, globals).await;
    let user_id = helpers::create_test_user(&app, email, password);
    let (browser, _browser_handle) = launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();
    browser_login(&page, &base_url, email, password).await;
    BrowserTestCtx {
        app,
        base_url,
        server_handle,
        user_id,
        browser,
        _browser_handle,
        page,
    }
}

pub async fn setup_browser_test_with_config(
    collections: Vec<CollectionDefinition>,
    globals: Vec<GlobalDefinition>,
    config: CrapConfig,
    email: &str,
    password: &str,
) -> BrowserTestCtx {
    let (base_url, server_handle, app) =
        spawn_server_with_config(collections, globals, config).await;
    let user_id = helpers::create_test_user(&app, email, password);
    let (browser, _browser_handle) = launch_browser().await;
    let page = browser.new_page("about:blank").await.unwrap();
    browser_login(&page, &base_url, email, password).await;
    BrowserTestCtx {
        app,
        base_url,
        server_handle,
        user_id,
        browser,
        _browser_handle,
        page,
    }
}
