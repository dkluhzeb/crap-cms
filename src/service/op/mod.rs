//! Operation core — every CRUD/bulk/global/version operation declared once
//! (see `docs/src/internals/operation-core-migration.md`).
//!
//! A surface becomes a **codec**: it decodes its wire format into an
//! [`Operation::Args`] + [`Principal`] + [`TargetRef`], calls
//! [`run_blocking`] (async surfaces) or [`run`] (sync callers), and encodes
//! the output / maps [`CoreError`] back onto its wire. Auth resolution, target
//! lookup, context assembly, and the operation body live HERE — once — so a
//! guard can no longer exist on three of four surfaces.
//!
//! The Lua CRUD surface stays outside the `run` entry by design: it runs
//! inside a hook transaction with a pre-resolved hook user and per-VM infra
//! (no `AppInfra`), so it builds its own transaction context and calls the
//! same operation bodies (`<Op>::run`) directly — the semantics live in the
//! bodies, the entry is only connection/principal/target assembly.

use std::collections::HashMap;

use anyhow::anyhow;
use tokio::task;
use tracing::error;

use crate::{
    core::{CollectionDefinition, Document, collection::Surface},
    db::BoxedConnection,
    service::{
        AppInfra, RunnerReadHooks, ServiceContext, ServiceError,
        auth::{AuthFailure, AuthRequest, EvaluateDeps, Resolution, evaluate},
    },
};

mod count;
mod create;
mod create_many;
mod delete;
mod delete_many;
mod find;
mod find_by_id;
mod get_global;
mod undelete;
mod unpublish;
mod update;
mod update_global;
mod update_many;
mod validate;
mod versions;
pub mod wire;
pub mod wire_doc;
pub mod wire_proto;

pub use count::{Count, CountArgs};
pub use create::{Create, CreateArgs};
pub use create_many::{CreateMany, CreateManyArgs};
pub use delete::{Delete, DeleteArgs};
pub use delete_many::{DeleteMany, DeleteManyArgs};
pub use find::{Find, FindArgs};
pub use find_by_id::{FindById, FindByIdArgs};
pub use get_global::{GetGlobal, GetGlobalArgs};
pub use undelete::{Undelete, UndeleteArgs};
pub use unpublish::{Unpublish, UnpublishArgs};
pub use update::{Update, UpdateArgs};
pub use update_global::{UnpublishGlobal, UnpublishGlobalArgs, UpdateGlobal, UpdateGlobalArgs};
pub use update_many::{UpdateMany, UpdateManyArgs};
pub use validate::{Validate, ValidateArgs, ValidateGlobal, ValidateOutput};
pub use versions::{ListVersions, ListVersionsArgs, RestoreVersion, RestoreVersionArgs};

/// A single canonical operation: owned per-call arguments plus the handler
/// over the existing service function. Declared once; every surface reuses it.
pub trait Operation {
    /// Owned, `Send + 'static` argument bundle — decoded by the surface
    /// codecs, moved into the blocking task by [`run_blocking`].
    type Args: Send + 'static;
    type Output: Send + 'static;

    /// Operation name for tracing and error context.
    const NAME: &'static str;

    /// Whether the operation reads through the context connection and read
    /// hooks (read ops). Write ops set `false`: their pool-mode bodies open
    /// their own write transaction and never touch `ctx.conn`/`ctx.read_hooks`,
    /// so the entry releases the read-pool checkout right after credential
    /// resolution (and skips it entirely for pre-resolved principals). Holding
    /// it across the write was a same-pool double acquisition on Postgres —
    /// deadlock-prone at pool saturation.
    const READS_VIA_CONTEXT: bool = true;

    /// Whether this operation publishes its own mutation events. Read ops
    /// keep the default; write ops return the request's `events` flag so the
    /// context is built with the right `emit_events`.
    ///
    /// This flag is applied by the [`run`] entry only — the operation bodies
    /// ignore their `events` arg and honor `ctx.emit_events`, so direct body
    /// callers (Lua, admin) must set `.emit_events(...)` on their contexts.
    fn emit_events(_args: &Self::Args) -> bool {
        true
    }

    /// Optionally derive an adjusted collection definition for this call.
    /// The one user is delete's `force_hard_delete`, which disables
    /// soft-delete on a local clone so the service routes to a permanent
    /// delete — previously copy-pasted on every surface. Default: use the
    /// registry's definition as-is.
    fn adjust_collection_def(
        _args: &Self::Args,
        _def: &CollectionDefinition,
    ) -> Option<CollectionDefinition> {
        None
    }

    /// Execute against an assembled context. Implementations consume `args`
    /// to build the service-layer input struct and call the canonical
    /// `service::*` function.
    ///
    /// # Errors
    ///
    /// Propagates the service function's error.
    fn run(ctx: &ServiceContext<'_>, args: Self::Args) -> Result<Self::Output, ServiceError>;
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
    /// cost and split the cookie-clearing semantics). `ui_locale` is the
    /// admin editor's UI locale, threaded into the read hooks for
    /// translated hook context; API surfaces have none.
    Resolved {
        user: Option<Document>,
        ui_locale: Option<String>,
    },
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
    /// Infrastructure failure outside the service layer (task join; pool
    /// errors are classified into `Service` at the entry).
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
    task::spawn_blocking(move || run::<O>(&infra, principal, &target, args))
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
    args: O::Args,
) -> Result<O::Output, CoreError> {
    // Pool errors are CLASSIFIED here, at the one entry — `classify` matches
    // the raw error text ("Timed out waiting…", SQLITE_BUSY), so a transient
    // pool failure reaches every codec as `ServiceError::Transient`
    // (503-class) instead of an opaque internal error. Previously only the
    // gRPC codec re-classified; admin and MCP reported a plain 500.
    let classify_pool =
        |e: anyhow::Error| CoreError::Service(ServiceError::classify(e, infra.pool.kind()));

    // The read-pool connection exists only as long as something needs it:
    // credential resolution (the evaluator's DB lookups) and, for read ops,
    // the context's connection + read hooks. A write op with a pre-resolved
    // principal never checks one out at all.
    let (actor, ctx_conn) = match principal {
        Principal::Credentials(_) => {
            let conn = infra.pool.get().map_err(classify_pool)?;
            let actor = resolve_principal(infra, principal, Some(&conn))?;
            (actor, O::READS_VIA_CONTEXT.then_some(conn))
        }
        resolved => {
            let actor = resolve_principal(infra, resolved, None)?;
            let conn = if O::READS_VIA_CONTEXT {
                Some(infra.pool.get().map_err(classify_pool)?)
            } else {
                None
            };
            (actor, conn)
        }
    };
    let user_doc = actor.user;

    let read_hooks = ctx_conn.as_ref().map(|conn| {
        let hooks = RunnerReadHooks::new(
            &infra.hook_runner,
            conn,
            user_doc.as_ref(),
            actor.ui_locale.as_deref(),
        );
        if actor.override_access {
            hooks.with_override_access()
        } else {
            hooks
        }
    });
    let emit_events = O::emit_events(&args);

    // Keeps an op-adjusted definition clone alive for the context's lifetime.
    let adjusted_def;

    let builder = match target.kind {
        TargetKind::Collection => {
            let Some(def) = infra.registry.get_collection(&target.slug) else {
                return Err(CoreError::UnknownTarget {
                    slug: target.slug.clone(),
                    kind: target.kind,
                });
            };

            let def = match O::adjust_collection_def(&args, def) {
                Some(d) => {
                    adjusted_def = d;
                    &adjusted_def
                }
                None => def,
            };

            ServiceContext::collection(&target.slug, def)
        }
        TargetKind::Global => {
            let Some(def) = infra.registry.get_global(&target.slug) else {
                return Err(CoreError::UnknownTarget {
                    slug: target.slug.clone(),
                    kind: target.kind,
                });
            };
            ServiceContext::global(&target.slug, def)
        }
    };

    let builder = builder
        .infra(infra)
        .user(user_doc.as_ref())
        .ui_locale(actor.ui_locale.clone())
        .override_access(actor.override_access)
        .emit_events(emit_events);

    let builder = match (&ctx_conn, &read_hooks) {
        (Some(conn), Some(hooks)) => builder.conn(conn).read_hooks(hooks),
        _ => builder,
    };

    let ctx = builder.build();

    O::run(&ctx, args).map_err(CoreError::Service)
}

/// The resolved actor for one operation.
struct ResolvedActor {
    user: Option<Document>,
    ui_locale: Option<String>,
    override_access: bool,
}

/// Resolve a [`Principal`] into a [`ResolvedActor`]. `conn` is required only
/// for [`Principal::Credentials`] — they go through the unified evaluator (the
/// same path the gRPC handlers and admin middleware use), which performs DB
/// lookups; pre-resolved principals need no connection.
fn resolve_principal(
    infra: &AppInfra,
    principal: Principal,
    conn: Option<&BoxedConnection>,
) -> Result<ResolvedActor, CoreError> {
    match principal {
        Principal::Resolved { user, ui_locale } => Ok(ResolvedActor {
            user,
            ui_locale,
            override_access: false,
        }),
        Principal::Override => Ok(ResolvedActor {
            user: None,
            ui_locale: None,
            override_access: true,
        }),
        Principal::Credentials(c) => {
            let conn = conn.ok_or_else(|| {
                CoreError::Internal(anyhow!("credential resolution requires a connection"))
            })?;
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
                // The evaluator resolves the user's stored UI-locale
                // preference; thread it through so write inputs and hook
                // contexts see it uniformly (the gRPC write path always did;
                // its read path used to drop it).
                Resolution::Authenticated(auth) => Ok(ResolvedActor {
                    ui_locale: Some(auth.user.ui_locale),
                    user: Some(auth.user.user_doc),
                    override_access: false,
                }),
                Resolution::Anonymous => Ok(ResolvedActor {
                    user: None,
                    ui_locale: None,
                    override_access: false,
                }),
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

    /// Regression: a pool failure surfaced through `run` must arrive as
    /// TRANSIENT (503-class on every codec), not internal (500). This held
    /// two bugs historically: `DbPool::get` wraps the cause in an anyhow
    /// context ("Failed to get DB connection"), and `ServiceError::classify`
    /// used to match `to_string()` — which shows only the outermost context —
    /// so the r2d2 timeout text never matched. `classify` matches the full
    /// `{:#}` chain and now runs at the op entry itself.
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
            args,
        )
        .expect_err("broken pool must error");

        // The entry classifies pool failures itself now — every codec
        // (admin/MCP included, not just gRPC) receives the typed Transient.
        assert!(
            matches!(err, CoreError::Service(ServiceError::Transient(_))),
            "a pool timeout must arrive pre-classified as Transient (503-class), got {err:?}"
        );

        // Regression for the write-path connection lifecycle: a write op with
        // a pre-resolved principal must not check out a read-pool connection
        // at all (`READS_VIA_CONTEXT = false`). With the same broken pool it
        // must reach the registry lookup (UnknownTarget), not fail on pool
        // acquisition — holding an idle read conn across the write was a
        // same-pool double acquisition on Postgres.
        let err = run::<Delete>(
            &infra,
            Principal::Resolved {
                user: None,
                ui_locale: None,
            },
            &TargetRef::collection("posts"),
            DeleteArgs::builder("x").build(),
        )
        .expect_err("unknown collection must error");

        assert!(
            matches!(err, CoreError::UnknownTarget { .. }),
            "a write op with a resolved principal must not touch the read pool"
        );
    }
}
