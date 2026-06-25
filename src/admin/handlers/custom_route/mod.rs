//! Custom HTTP routes declared via `crap.routes.register`. Each registered
//! route is mounted ([`mount`]) as its own Axum route whose handler is the
//! generic [`dispatch_custom_route`], with the route's [`MountedRoute`] injected
//! as a request extension.

mod dispatch;
mod mount;

pub use dispatch::dispatch_custom_route;
pub use mount::{MountedRoute, custom_routes_router};
