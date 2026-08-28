//! Operation core — the single dispatch entry every pool-mode surface shares
//! (Op Core Stage 2, see `docs/src/internals/operation-core-migration.md`).
//!
//! A surface becomes a **codec**: it decodes its wire format into an
//! [`Operation::Args`] + [`Principal`] + [`TargetRef`], calls
//! [`run_blocking`] (async surfaces) or [`run`] (sync callers), and encodes
//! the output / maps [`CoreError`] back onto its wire. Auth resolution, target
//! lookup, context assembly, and the operation body live HERE — once — so a
//! guard can no longer exist on three of four surfaces.
//!
//! The Lua CRUD surface stays outside this entry by design: it runs inside a
//! hook transaction with a pre-resolved hook user and per-VM infra (no
//! `AppInfra`), so it keeps calling the same `service::*` functions directly.
//! The operation body (`Operation::run` over the service fn) is still shared.

use std::collections::HashMap;

use anyhow::anyhow;
use tokio::task;
use tracing::error;

use crate::{
    core::{Document, collection::Surface},
    db::BoxedConnection,
    service::{
        AppInfra, RunnerReadHooks, ServiceContext, ServiceError,
        auth::{AuthFailure, AuthRequest, EvaluateDeps, Resolution, evaluate},
    },
};

mod find_by_id;

pub use find_by_id::{FindById, FindByIdArgs};

/// A single canonical operation: owned per-call arguments plus the handler
/// over the existing service function. Declared once; every surface reuses it.
pub trait Operation {
    /// Owned, `Send + 'static` argument bundle — decoded by the surface
    /// codecs, moved into the blocking task by [`run_blocking`].
    type Args: Send + 'static;
    type Output: Send + 'static;

    /// Operation name for tracing and error context.
    const NAME: &'static str;

    /// Execute against an assembled context. Implementations borrow from
    /// `args` to build the service-layer input struct and call the canonical
    /// `service::*` function.
    ///
    /// # Errors
    ///
    /// Propagates the service function's error.
    fn run(ctx: &ServiceContext<'_>, args: &Self::Args) -> Result<Self::Output, ServiceError>;
}

/// Wire credentials, surface-neutral. Each surface reads these off its own
/// transport (gRPC metadata, admin cookies/headers) and hands them over;
/// resolution happens once inside the core via the unified evaluator.
pub struct Credentials {
    pub surface: Surface,
    pub bearer: Option<String>,
    pub session_cookie: Option<String>,
    pub headers: HashMap<String, String>,
}

/// Who is performing the operation.
pub enum Principal {
    /// Unresolved wire credentials — the core resolves them via
    /// `service::auth::evaluate` on the operation's connection.
    Credentials(Credentials),
    /// A pre-resolved actor (the admin middleware already ran the evaluator
    /// for the whole request; re-running it per operation would double the
    /// cost and split the cookie-clearing semantics).
    Resolved(Option<Document>),
    /// Trusted system caller (MCP): bypasses access checks — the context is
    /// built with `override_access` and override-aware read hooks.
    Override,
}

/// The operation target: a collection or global slug. Definition lookup
/// happens inside the core against the registry, so a stale or mismatched
/// definition can never be smuggled in by a codec.
pub struct TargetRef {
    pub slug: String,
    pub kind: TargetKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetKind {
    Collection,
    Global,
}

impl TargetRef {
    #[must_use]
    pub fn collection(slug: impl Into<String>) -> Self {
        Self {
            slug: slug.into(),
            kind: TargetKind::Collection,
        }
    }

    #[must_use]
    pub fn global(slug: impl Into<String>) -> Self {
        Self {
            slug: slug.into(),
            kind: TargetKind::Global,
        }
    }
}

/// Core-level error — what a codec maps back onto its wire. One enum so the
/// auth/not-found/service error translation exists once per surface instead
/// of once per handler.
#[derive(Debug)]
pub enum CoreError {
    /// Credential resolution failed (definite failure — bad token, locked,
    /// stale session, …). `Anonymous` is NOT an error; access enforcement is
    /// the service layer's job.
    Auth(AuthFailure),
    /// The target slug names no known collection/global.
    UnknownTarget { slug: String, kind: TargetKind },
    /// The operation itself failed.
    Service(ServiceError),
    /// Infrastructure failure outside the service layer (pool acquisition,
    /// task join).
    Internal(anyhow::Error),
}

impl From<ServiceError> for CoreError {
    fn from(e: ServiceError) -> Self {
        CoreError::Service(e)
    }
}

impl CoreError {
    /// Collapse onto [`ServiceError`] for surfaces that already speak it
    /// (admin response mapping, MCP's anyhow tail). `Auth` becomes
    /// `AccessDenied` — surfaces that resolve credentials per-operation
    /// (gRPC) map [`CoreError::Auth`] to their precise wire statuses instead
    /// of calling this.
    #[must_use]
    pub fn into_service_error(self) -> ServiceError {
        match self {
            CoreError::Service(e) => e,
            CoreError::Internal(e) => ServiceError::Internal(e),
            CoreError::UnknownTarget { slug, kind } => ServiceError::NotFound(match kind {
                TargetKind::Collection => format!("Collection '{slug}' not found"),
                TargetKind::Global => format!("Global '{slug}' not found"),
            }),
            CoreError::Auth(failure) => {
                ServiceError::AccessDenied(format!("Authentication failed: {failure:?}"))
            }
        }
    }
}

/// Run an operation on a blocking thread: acquire a connection, resolve the
/// principal, look up the target definition, assemble the context (all infra
/// via `ServiceContextBuilder::infra`), and execute.
///
/// # Errors
///
/// Returns [`CoreError`] — see its variants for the failure classes.
pub async fn run_blocking<O: Operation>(
    infra: std::sync::Arc<AppInfra>,
    principal: Principal,
    target: TargetRef,
    args: O::Args,
) -> Result<O::Output, CoreError> {
    task::spawn_blocking(move || run::<O>(&infra, principal, &target, &args))
        .await
        .inspect_err(|e| error!("{} task join error: {e}", O::NAME))
        .map_err(|e| CoreError::Internal(anyhow!("{} task join error: {e}", O::NAME)))?
}

/// Synchronous core entry — the body of [`run_blocking`], callable directly
/// by surfaces that already are on a blocking thread (MCP tools).
///
/// # Errors
///
/// Returns [`CoreError`] — see its variants for the failure classes.
pub fn run<O: Operation>(
    infra: &AppInfra,
    principal: Principal,
    target: &TargetRef,
    args: &O::Args,
) -> Result<O::Output, CoreError> {
    // The pool error stays UNWRAPPED: codecs push `CoreError::Internal`
    // through `ServiceError::classify`, which matches on `to_string()` — an
    // anyhow context layer would hide the cause ("Timed out waiting…",
    // SQLITE_BUSY) and misclassify a transient error as internal.
    let conn = infra.pool.get().map_err(CoreError::Internal)?;

    let (user_doc, override_access) = resolve_principal(infra, principal, &conn)?;

    let read_hooks = {
        let hooks = RunnerReadHooks::new(&infra.hook_runner, &conn, user_doc.as_ref(), None);
        if override_access {
            hooks.with_override_access()
        } else {
            hooks
        }
    };

    match target.kind {
        TargetKind::Collection => {
            let Some(def) = infra.registry.get_collection(&target.slug) else {
                return Err(CoreError::UnknownTarget {
                    slug: target.slug.clone(),
                    kind: target.kind,
                });
            };

            let ctx = ServiceContext::collection(&target.slug, def)
                .infra(infra)
                .conn(&conn)
                .read_hooks(&read_hooks)
                .user(user_doc.as_ref())
                .override_access(override_access)
                .build();

            O::run(&ctx, args).map_err(CoreError::Service)
        }
        TargetKind::Global => {
            let Some(def) = infra.registry.get_global(&target.slug) else {
                return Err(CoreError::UnknownTarget {
                    slug: target.slug.clone(),
                    kind: target.kind,
                });
            };

            let ctx = ServiceContext::global(&target.slug, def)
                .infra(infra)
                .conn(&conn)
                .read_hooks(&read_hooks)
                .user(user_doc.as_ref())
                .override_access(override_access)
                .build();

            O::run(&ctx, args).map_err(CoreError::Service)
        }
    }
}

/// Resolve a [`Principal`] into `(user document, override flag)` on the
/// operation's connection. Credentials go through the unified evaluator —
/// the same path the gRPC handlers and admin middleware use.
fn resolve_principal(
    infra: &AppInfra,
    principal: Principal,
    conn: &BoxedConnection,
) -> Result<(Option<Document>, bool), CoreError> {
    match principal {
        Principal::Resolved(user) => Ok((user, false)),
        Principal::Override => Ok((None, true)),
        Principal::Credentials(c) => {
            let request = AuthRequest {
                surface: c.surface,
                bearer_token: c.bearer.as_deref(),
                session_cookie_token: c.session_cookie.as_deref(),
                headers: &c.headers,
            };
            let deps = EvaluateDeps {
                registry: &infra.registry,
                token_provider: infra.token_provider.as_ref(),
                hook_runner: &infra.hook_runner,
                conn,
            };

            match evaluate(&request, &deps) {
                Resolution::Authenticated(auth) => Ok((Some(auth.user.user_doc), false)),
                Resolution::Anonymous => Ok((None, false)),
                Resolution::Invalid(failure) => Err(CoreError::Auth(failure)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use r2d2_sqlite::SqliteConnectionManager;

    use super::*;
    use crate::{
        admin::test_support::test_infra,
        config::{CrapConfig, UploadConfig},
        core::{Registry, SharedTokenProvider, auth::JwtTokenProvider, upload::create_storage},
        db::DbPool,
        hooks::HookRunner,
    };

    /// Regression: a pool failure surfaced through `run` must classify as
    /// TRANSIENT at the codec (gRPC unavailable/503), not internal (500).
    /// This held two bugs: `DbPool::get` wraps the cause in an anyhow context
    /// ("Failed to get DB connection"), and `ServiceError::classify` used to
    /// match `to_string()` — which shows only the outermost context — so the
    /// r2d2 timeout text never matched. `classify` now matches the full
    /// `{:#}` chain.
    #[test]
    fn pool_error_stays_classifiable() {
        let tmp = tempfile::tempdir().unwrap();
        let config = CrapConfig::test_default();

        // A pool whose connections can never be established, with a short
        // timeout so `get()` fails fast with r2d2's timeout error.
        let manager = SqliteConnectionManager::file("/nonexistent-dir/no.db");
        let pool = DbPool::from_pool(
            r2d2::Pool::builder()
                .max_size(1)
                .connection_timeout(Duration::from_millis(100))
                .build_unchecked(manager),
        );

        let registry: Arc<Registry> = Arc::new(Registry::default());
        let hook_runner = HookRunner::builder()
            .config_dir(tmp.path())
            .registry(Arc::clone(&registry))
            .config(&config)
            .build()
            .unwrap();
        let storage = create_storage(tmp.path(), &UploadConfig::default()).unwrap();
        let token_provider: SharedTokenProvider = Arc::new(JwtTokenProvider::new("test-secret"));
        let infra = test_infra(
            pool,
            registry,
            hook_runner,
            storage,
            token_provider,
            &config,
            tmp.path(),
        );

        let args = FindByIdArgs::builder("x").build();
        let err = run::<FindById>(
            &infra,
            Principal::Override,
            &TargetRef::collection("posts"),
            &args,
        )
        .expect_err("broken pool must error");

        let CoreError::Internal(e) = err else {
            panic!("expected CoreError::Internal for a pool failure");
        };
        assert!(
            matches!(
                ServiceError::classify(e, "sqlite"),
                ServiceError::Transient(_)
            ),
            "a pool timeout must classify as Transient (503-class), not Internal"
        );
    }
}
