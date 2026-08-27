//! Mounting of custom routes: builds one Axum route per registration, with the
//! route's [`MountedRoute`] injected as a request extension and a per-route body
//! limit + optional rate limiter applied.

use std::sync::Arc;

use axum::{
    Extension, Router,
    extract::DefaultBodyLimit,
    routing::{MethodFilter, MethodRouter},
};
use tower::ServiceBuilder;
use tracing::info;

use crate::{
    admin::{
        AdminState,
        custom_routes::{RouteAccess, RouteDefinition},
        handlers::custom_route::dispatch_custom_route,
    },
    core::rate_limit::LoginRateLimiter,
};

/// A registered route plus the runtime state mounting needs: its definition, an
/// optional per-route rate limiter, and the resolved body-size limit. Injected
/// into each route's requests as an extension so the dispatcher can read it.
pub struct MountedRoute {
    pub def: RouteDefinition,
    pub limiter: Option<Arc<LoginRateLimiter>>,
    pub body_limit: usize,
}

/// Combine a route's validated method names into an Axum [`MethodFilter`].
/// Returns `None` if the list is empty (the route is skipped). A `GET` route
/// also answers `HEAD` (HTTP norm) — a bare `MethodFilter` doesn't do this
/// automatically the way `routing::get` does, so OR it in explicitly.
fn methods_to_filter(methods: &[String]) -> Option<MethodFilter> {
    let mut filter: Option<MethodFilter> = None;
    let mut has_get = false;
    for m in methods {
        let f = match m.as_str() {
            "GET" => {
                has_get = true;
                MethodFilter::GET
            }
            "HEAD" => MethodFilter::HEAD,
            "POST" => MethodFilter::POST,
            "PUT" => MethodFilter::PUT,
            "PATCH" => MethodFilter::PATCH,
            "DELETE" => MethodFilter::DELETE,
            "OPTIONS" => MethodFilter::OPTIONS,
            _ => continue,
        };
        filter = Some(filter.map_or(f, |acc| acc.or(f)));
    }
    if has_get {
        filter = filter.map(|acc| acc.or(MethodFilter::HEAD));
    }
    filter
}

/// Build a router mounting every route registered via `crap.routes.register`,
/// under the configured `routes.prefix`. Each route gets its own body-size limit
/// (`max_body`, falling back to `routes.max_body`) and, if configured, a rate
/// limiter on the shared backend (so a Redis backend shares state across
/// instances). Returns an empty router when no routes are registered. Logs each
/// mounted route (with its access stance) so operators can see what is exposed.
pub fn custom_routes_router(state: &AdminState) -> Router<AdminState> {
    let prefix = state.config.routes.prefix.trim_end_matches('/').to_string();
    let default_max_body = state.config.routes.max_body;

    let mut router = Router::new();
    for def in state.infra.hook_runner.extract_routes() {
        let Some(filter) = methods_to_filter(&def.methods) else {
            continue;
        };
        let full_path = format!("{prefix}{}", def.path);

        info!(
            "custom route mounted: {} {} (access: {})",
            def.methods.join(","),
            full_path,
            access_label(&def.access),
        );

        // Per-route rate limiter on the SHARED backend (keyed distinctly from
        // login), so a configured Redis backend shares state across instances.
        let limiter = def.rate_limit.map(|rl| {
            Arc::new(LoginRateLimiter::with_backend(
                state.login_limiter.backend(),
                &format!("route:{}", def.id()),
                rl.max,
                rl.window,
            ))
        });

        let body_limit =
            usize::try_from(def.max_body.unwrap_or(default_max_body)).unwrap_or(usize::MAX);
        let mounted = Arc::new(MountedRoute {
            def,
            limiter,
            body_limit,
        });

        // Both layers applied as one `ServiceBuilder` so the chained-`layer`
        // error type stays inferrable (`MethodRouter`'s default `Infallible`).
        let method_router: MethodRouter<AdminState> =
            MethodRouter::new().on(filter, dispatch_custom_route).layer(
                ServiceBuilder::new()
                    .layer(DefaultBodyLimit::max(body_limit))
                    .layer(Extension(mounted)),
            );

        router = router.route(&full_path, method_router);
    }

    router
}

fn access_label(access: &RouteAccess) -> &'static str {
    match access {
        RouteAccess::Public => "public",
        RouteAccess::Disabled => "disabled",
        RouteAccess::Gated(_) => "gated",
    }
}
