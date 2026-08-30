//! `status --check` — best-practice audit for project configuration.
//!
//! Inspects the loaded config, registry, and database state for common
//! misconfigurations, performance risks, and missing best practices.
//! Returns the number of warnings emitted (0 = clean bill of health).

use std::path::Path;

use crate::core::collection::Auth;
use crate::{
    cli,
    config::{CacheBackend, CompressionMode, CrapConfig, EmailProvider},
    core::{Hooks, LiveMode, Registry},
    db::{DbConnection, DbPool, migrate},
};

/// A single check result.
struct Finding {
    message: String,
    hint: Option<String>,
}

impl Finding {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            hint: None,
        }
    }

    fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

/// Gather all findings without printing. Used by both `run_checks` and `count_warnings`.
fn gather_findings(
    cfg: &CrapConfig,
    reg: &Registry,
    conn: &dyn DbConnection,
    pool: &DbPool,
    config_dir: &Path,
) -> Vec<Finding> {
    let mut findings = Vec::new();

    check_dev_mode(cfg, &mut findings);
    check_depth(cfg, &mut findings);
    check_cache(cfg, reg, &mut findings);
    check_pool(cfg, &mut findings);
    check_compression(cfg, &mut findings);
    check_auth(cfg, reg, &mut findings);
    check_auth_methods(reg, &mut findings);
    check_access(cfg, reg, &mut findings);
    check_cors(cfg, &mut findings);
    check_rate_limiting(cfg, reg, &mut findings);
    check_email(cfg, reg, &mut findings);
    check_hooks(reg, &mut findings);
    check_live(cfg, reg, &mut findings);
    check_pagination(cfg, &mut findings);
    check_migrations(pool, config_dir, &mut findings);
    check_collections(reg, conn, &mut findings);

    findings
}

/// Run all checks and print results. Returns the number of warnings.
pub(super) fn run_checks(
    cfg: &CrapConfig,
    reg: &Registry,
    conn: &dyn DbConnection,
    pool: &DbPool,
    config_dir: &Path,
) -> usize {
    let findings = gather_findings(cfg, reg, conn, pool, config_dir);

    println!();
    cli::header("Health Check");

    if findings.is_empty() {
        cli::success("All checks passed — no issues found.");
        return 0;
    }

    for f in &findings {
        cli::warning(&f.message);

        if let Some(ref hint) = f.hint {
            cli::hint(hint);
        }
    }

    println!();
    cli::kv_status("Result", &format!("{} warning(s)", findings.len()), false);

    findings.len()
}

/// Count warnings without printing. Used for startup nudge.
pub fn count_warnings(
    cfg: &CrapConfig,
    reg: &Registry,
    conn: &dyn DbConnection,
    pool: &DbPool,
    config_dir: &Path,
) -> usize {
    gather_findings(cfg, reg, conn, pool, config_dir).len()
}

// ── Individual checks ──────────────────────────────────────────────────

fn check_dev_mode(cfg: &CrapConfig, findings: &mut Vec<Finding>) {
    if cfg.admin.dev_mode {
        findings.push(
            Finding::new("dev_mode is enabled — templates reload on every request")
                .with_hint("Set `admin.dev_mode = false` for production deployments."),
        );
    }
}

fn check_depth(cfg: &CrapConfig, findings: &mut Vec<Finding>) {
    if cfg.depth.max_depth > 3 {
        findings.push(
            Finding::new(format!(
                "max_depth = {} — deep population causes N+1 query growth",
                cfg.depth.max_depth
            ))
            .with_hint("Keep max_depth <= 3 to avoid exponential query counts."),
        );
    }

    if cfg.depth.default_depth > cfg.depth.max_depth {
        findings.push(Finding::new(format!(
            "default_depth ({}) exceeds max_depth ({})",
            cfg.depth.default_depth, cfg.depth.max_depth
        )));
    }
}

fn check_cache(cfg: &CrapConfig, reg: &Registry, findings: &mut Vec<Finding>) {
    let has_relationships = reg.collections.values().any(|def| {
        def.fields
            .iter()
            .any(|f| f.field_type.as_str() == "relationship")
    });

    if cfg.cache.backend == CacheBackend::None && has_relationships && reg.collections.len() > 3 {
        findings.push(
            Finding::new(
                "Cache is disabled but collections use relationships — populate results are recomputed on every read",
            )
            .with_hint("Set `cache.backend = \"memory\"` (default) or `\"redis\"` for better read performance."),
        );
    }
}

fn check_pool(cfg: &CrapConfig, findings: &mut Vec<Finding>) {
    if cfg.database.pool_max_size < 4 {
        findings.push(
            Finding::new(format!(
                "pool_max_size = {} — too few connections for concurrent requests",
                cfg.database.pool_max_size
            ))
            .with_hint("Recommended minimum: 8 for SQLite, 16 for PostgreSQL."),
        );
    }

    if cfg.database.connection_timeout < 2 {
        findings.push(
            Finding::new(format!(
                "connection_timeout = {}s — very aggressive, may cause spurious failures under load",
                cfg.database.connection_timeout
            ))
            .with_hint("Recommended: 5s or higher."),
        );
    }
}

fn check_compression(cfg: &CrapConfig, findings: &mut Vec<Finding>) {
    if cfg.server.compression == CompressionMode::Off {
        findings.push(
            Finding::new("Response compression is disabled")
                .with_hint(
                    "Set `server.compression = \"gzip\"` or `\"all\"` unless a reverse proxy handles compression.",
                ),
        );
    }
}

fn check_auth(cfg: &CrapConfig, reg: &Registry, findings: &mut Vec<Finding>) {
    let auth_collections: Vec<_> = reg
        .collections
        .values()
        .filter(|d| d.is_auth_collection())
        .collect();

    if auth_collections.is_empty() {
        return;
    }

    if cfg.auth.secret.len() < 32 {
        findings.push(
            Finding::new("Auth secret is shorter than 32 characters")
                .with_hint("Use a cryptographically random secret of at least 64 characters."),
        );
    }

    // Detect scaffold default secret (starts with a known pattern)
    let secret_str: &str = cfg.auth.secret.as_ref();

    if secret_str.starts_with("CHANGE_ME") || secret_str == "secret" {
        findings.push(
            Finding::new("Auth secret looks like a placeholder — not safe for production")
                .with_hint("Generate a random secret: `openssl rand -base64 48`"),
        );
    }

    if cfg.auth.max_login_attempts == 0 {
        findings.push(
            Finding::new("Login brute-force protection is disabled (max_login_attempts = 0)")
                .with_hint("Set `auth.max_login_attempts` to limit failed login attempts."),
        );
    }
}

/// Surface auth-method shape smells the startup validator would let
/// through silently. Hard errors (enabled + empty methods, duplicate
/// `password_login` / bearer) are already rejected at startup —
/// `status` only flags configurations that *work* but probably
/// aren't what the operator intended.
fn check_auth_methods(reg: &Registry, findings: &mut Vec<Finding>) {
    use crate::core::collection::{Activation, AuthMethod, Surface};
    use std::collections::HashMap;

    // Tracks every `Always`-activated strategy, keyed by the surface
    // it covers. Used to flag the cross-collection race where two
    // such strategies compete on the same surface.
    let mut always_strategies_by_surface: HashMap<Surface, Vec<String>> = HashMap::new();
    // Tracks every `Header`-activated strategy by `(lowercase header,
    // surface)`. Two strategies sharing both fields race for the
    // request — HashMap iteration order picks the winner.
    let mut header_strategies_by_key: HashMap<(String, Surface), Vec<String>> = HashMap::new();

    for (slug, def) in &reg.collections {
        let Some(auth) = def.auth.as_ref() else {
            continue;
        };
        if !auth.enabled {
            continue;
        }

        let has_password = auth.password_login_enabled();
        let has_bearer_anywhere =
            auth.accepts_bearer(Surface::Admin) || auth.accepts_bearer(Surface::Grpc);

        // Password-login without bearer: the Login RPC will issue a JWT
        // that no subsequent request can use (every surface's bearer
        // check fails). Almost always a config mistake.
        if has_password && !has_bearer_anywhere {
            findings.push(
                Finding::new(format!(
                    "Collection '{slug}' has password_login but no bearer method — \
                     Login will issue a JWT that no future request can authenticate",
                ))
                .with_hint(
                    "Add `{ type = \"bearer\", surfaces = { \"grpc\", \"admin\" } }` \
                     to the methods list, or use crap.auth.with_defaults({...}).",
                ),
            );
        }

        for method in &auth.methods {
            if let AuthMethod::Strategy {
                name,
                activates_on,
                surfaces,
                ..
            } = method
            {
                let owner = format!("{slug}.{name}");
                match activates_on {
                    Activation::Always { .. } => {
                        for surface in surfaces {
                            always_strategies_by_surface
                                .entry(*surface)
                                .or_default()
                                .push(owner.clone());
                        }
                    }
                    Activation::Header { header } => {
                        let key = header.to_ascii_lowercase();
                        for surface in surfaces {
                            header_strategies_by_key
                                .entry((key.clone(), *surface))
                                .or_default()
                                .push(owner.clone());
                        }
                    }
                }
            }
        }
    }

    // Multiple Always-active strategies on the same surface: ordering
    // of `registry.collections` determines which one wins. Operators
    // rarely want this; prefer a header discriminator to bind each
    // strategy to its own request signal.
    for (surface, owners) in &always_strategies_by_surface {
        if owners.len() > 1 {
            findings.push(
                Finding::new(format!(
                    "Multiple always-active auth strategies on surface {surface:?}: {} — \
                     request authentication depends on registration order",
                    owners.join(", ")
                ))
                .with_hint(
                    "Bind each strategy to its own request signal via \
                     `activates_on = { header = \"x-...\" }`.",
                ),
            );
        }
    }

    // Multiple header-activated strategies bound to the *same* header
    // on the *same* surface: identical situation as the always case
    // — HashMap iteration order picks the winner non-deterministically.
    // Detected separately because a header discriminator that's used
    // by exactly one strategy is fine; the collision is the issue.
    for ((header, surface), owners) in &header_strategies_by_key {
        if owners.len() > 1 {
            findings.push(
                Finding::new(format!(
                    "Multiple auth strategies bound to header '{header}' on surface \
                     {surface:?}: {} — request authentication depends on registration order",
                    owners.join(", ")
                ))
                .with_hint(
                    "Use distinct header names per strategy, or scope each strategy \
                     to a different `surfaces` list.",
                ),
            );
        }
    }
}

fn check_cors(cfg: &CrapConfig, findings: &mut Vec<Finding>) {
    if cfg.cors.allowed_origins.iter().any(|o| o == "*") && cfg.cors.allow_credentials {
        findings.push(
            Finding::new("CORS allows all origins with credentials — browsers will reject this")
                .with_hint(
                    "Wildcard origin and allow_credentials are mutually exclusive per the CORS spec. List specific origins instead.",
                ),
        );
    }
}

fn check_rate_limiting(cfg: &CrapConfig, reg: &Registry, findings: &mut Vec<Finding>) {
    let has_auth = reg.collections.values().any(|d| d.is_auth_collection());

    if has_auth && cfg.server.grpc_rate_limit_requests == 0 {
        findings.push(
            Finding::new("gRPC rate limiting is disabled with auth collections present")
                .with_hint(
                    "Set `server.grpc_rate_limit_requests` to protect login/register endpoints from abuse.",
                ),
        );
    }
}

fn check_email(cfg: &CrapConfig, reg: &Registry, findings: &mut Vec<Finding>) {
    let has_verify_email = reg
        .collections
        .values()
        .any(|d| d.auth.as_ref().is_some_and(Auth::requires_verify_email));

    if has_verify_email && cfg.email.provider == EmailProvider::Log {
        findings.push(
            Finding::new(
                "Email provider is \"log\" but auth collections have verify_email enabled — verification emails will not be delivered",
            )
            .with_hint("Configure an SMTP or webhook email provider in [email]."),
        );
    }
}

fn check_access(cfg: &CrapConfig, reg: &Registry, findings: &mut Vec<Finding>) {
    if !cfg.access.default_deny {
        findings.push(
            Finding::new(
                "default_deny is false — collections without access functions are publicly accessible",
            )
            .with_hint("Set `access.default_deny = true` for production deployments."),
        );
    }

    let unprotected: Vec<&str> = reg
        .collections
        .iter()
        .filter(|(_, def)| {
            def.access.read.is_none()
                && def.access.create.is_none()
                && def.access.update.is_none()
                && def.access.delete.is_none()
        })
        .map(|(slug, _)| slug.as_ref())
        .collect();

    if !unprotected.is_empty() && !cfg.access.default_deny {
        findings.push(
            Finding::new(format!(
                "{} collection(s) have no access rules: {}",
                unprotected.len(),
                unprotected.join(", ")
            ))
            .with_hint("Add access functions or enable default_deny."),
        );
    }

    // With default_deny = false, a draft/trash view with no own key and no
    // `update` fallback is world-readable even when `read` is set — so this
    // catches collections the coarse "no access rules" check above misses.
    for (slug, def) in &reg.collections {
        let views = def.publicly_exposed_lifecycle_views(cfg.access.default_deny);
        if views.is_empty() {
            continue;
        }

        findings.push(
            Finding::new(format!(
                "Collection '{slug}': {} view(s) are publicly readable ({})",
                views.join(" and "),
                views
                    .iter()
                    .map(|v| format!("no access.{v} or access.update rule"))
                    .collect::<Vec<_>>()
                    .join("; ")
            ))
            .with_hint(
                "Unpublished/soft-deleted documents are visible to everyone. Add the matching \
                 access rule or set access.default_deny = true.",
            ),
        );
    }

    // Globals have no soft-delete, but the same footgun applies to their draft
    // view: drafts enabled with no `draft`/`update` rule under default_deny=false.
    for (slug, def) in &reg.globals {
        if def.draft_view_publicly_exposed(cfg.access.default_deny) {
            findings.push(
                Finding::new(format!(
                    "Global '{slug}': the draft view is publicly readable \
                     (no access.draft or access.update rule)"
                ))
                .with_hint(
                    "Unpublished documents are visible to everyone. Add the matching access \
                     rule or set access.default_deny = true.",
                ),
            );
        }
    }
}

fn check_hooks(reg: &Registry, findings: &mut Vec<Finding>) {
    for (slug, def) in &reg.collections {
        let hook_count = count_hooks(&def.hooks);

        if hook_count > 10 {
            findings.push(Finding::new(format!(
                "Collection '{slug}' has {hook_count} hooks — may impact write latency"
            )));
        }

        let before_change_count = def.hooks.before_change.len();

        if before_change_count > 3 {
            findings.push(
                Finding::new(format!(
                    "Collection '{slug}' has {before_change_count} before_change hooks — runs sequentially per write",
                ))
                .with_hint("Consider consolidating into fewer hooks for better write performance."),
            );
        }
    }
}

fn check_live(cfg: &CrapConfig, reg: &Registry, findings: &mut Vec<Finding>) {
    if !cfg.live.enabled {
        return;
    }

    let full_mode_collections: Vec<&str> = reg
        .collections
        .iter()
        .filter(|(_, def)| def.live_mode == LiveMode::Full)
        .map(|(slug, _)| slug.as_ref())
        .collect();

    if full_mode_collections.len() > 5 {
        findings.push(
            Finding::new(format!(
                "{} collections use live_mode = \"full\" — runs after_read hooks per event per subscriber",
                full_mode_collections.len()
            ))
            .with_hint("Use \"metadata\" mode for high-traffic collections; clients can re-fetch on demand."),
        );
    }
}

fn check_pagination(cfg: &CrapConfig, findings: &mut Vec<Finding>) {
    if cfg.pagination.max_limit > 500 {
        findings.push(
            Finding::new(format!(
                "pagination.max_limit = {} — large pages can cause slow queries and high memory use",
                cfg.pagination.max_limit
            ))
            .with_hint("Recommended max: 500. Use cursor pagination for large datasets."),
        );
    }
}

fn check_migrations(pool: &DbPool, config_dir: &Path, findings: &mut Vec<Finding>) {
    let migrations_dir = config_dir.join("migrations");
    let all_files = migrate::list_migration_files(&migrations_dir).unwrap_or_default();
    let applied = migrate::get_applied_migrations(pool).unwrap_or_default();
    let pending: Vec<_> = all_files.iter().filter(|f| !applied.contains(*f)).collect();

    if !pending.is_empty() {
        findings.push(
            Finding::new(format!(
                "{} pending migration(s): {}",
                pending.len(),
                pending
                    .iter()
                    .take(3)
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
            .with_hint("Run `crap-cms migrate up` to apply pending migrations."),
        );
    }
}

fn check_collections(reg: &Registry, conn: &dyn DbConnection, findings: &mut Vec<Finding>) {
    for (slug, def) in &reg.collections {
        // Auth collection without soft delete means accounts are permanently deleted
        if def.is_auth_collection() && !def.soft_delete {
            findings.push(
                Finding::new(format!(
                    "Auth collection '{slug}' has soft_delete disabled — deleted accounts are permanent"
                ))
                .with_hint("Consider enabling soft_delete for auth collections to support account recovery."),
            );
        }

        // Upload collection without versions means no rollback on file changes
        if def.is_upload_collection() && !def.has_versions() {
            findings.push(Finding::new(format!(
                "Upload collection '{slug}' has no versioning — overwritten files cannot be recovered"
            )));
        }

        // Soft delete without retention = trash grows unbounded
        if def.soft_delete && def.soft_delete_retention.is_none() {
            findings.push(
                Finding::new(format!(
                    "Collection '{slug}' has soft_delete enabled but no retention policy — trash grows unbounded"
                ))
                .with_hint("Set `soft_delete_retention = \"30d\"` to auto-purge old trash."),
            );
        }

        // Check for empty collections that have hooks (might indicate misconfiguration)
        let count = crate::db::query::count(conn, slug, def, &[], None).unwrap_or(0);
        let hook_count = count_hooks(&def.hooks);

        if count == 0 && hook_count > 0 && def.is_auth_collection() {
            findings.push(
                Finding::new(format!(
                    "Auth collection '{slug}' has 0 users — admin login will fail"
                ))
                .with_hint(format!(
                    "Run `crap-cms user create -c {slug}` to create an admin user."
                )),
            );
        }
    }
}

fn count_hooks(hooks: &Hooks) -> usize {
    hooks.before_validate.len()
        + hooks.before_change.len()
        + hooks.after_change.len()
        + hooks.before_read.len()
        + hooks.after_read.len()
        + hooks.before_delete.len()
        + hooks.after_delete.len()
        + hooks.before_broadcast.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::HookRef;
    use crate::core::{
        CollectionDefinition,
        collection::{Activation, Auth, AuthMethod, SurfaceSet},
    };

    fn registry_with(defs: Vec<CollectionDefinition>) -> Registry {
        let mut reg = Registry::new();
        for def in defs {
            reg.register_collection(def);
        }
        reg
    }

    fn auth_def_with(slug: &str, methods: Vec<AuthMethod>) -> CollectionDefinition {
        let mut def = CollectionDefinition::new(slug);
        def.auth = Some(Auth {
            enabled: true,
            methods,
            ..Default::default()
        });
        def
    }

    /// Test helper: an `Always`-active strategy on the admin surface.
    fn strategy_always(name: &str, authenticate: &str) -> AuthMethod {
        AuthMethod::Strategy {
            name: name.to_string(),
            authenticate: HookRef::new(authenticate),
            activates_on: Activation::always(),
            surfaces: SurfaceSet::admin_only(),
        }
    }

    /// Test helper: a header-discriminated strategy on the given surfaces.
    fn strategy_on_header(
        name: &str,
        authenticate: &str,
        header: &str,
        surfaces: SurfaceSet,
    ) -> AuthMethod {
        AuthMethod::Strategy {
            name: name.to_string(),
            authenticate: HookRef::new(authenticate),
            activates_on: Activation::Header {
                header: header.to_string(),
            },
            surfaces,
        }
    }

    #[test]
    fn check_auth_methods_clean_default_methods_emits_nothing() {
        let reg = registry_with(vec![auth_def_with("users", Auth::default_methods())]);
        let mut findings = Vec::new();
        check_auth_methods(&reg, &mut findings);
        assert!(
            findings.is_empty(),
            "default methods should be clean: {:?}",
            findings.iter().map(|f| &f.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn check_auth_methods_password_without_bearer_warns() {
        let reg = registry_with(vec![auth_def_with(
            "users",
            vec![AuthMethod::password_login()],
        )]);
        let mut findings = Vec::new();
        check_auth_methods(&reg, &mut findings);
        assert_eq!(findings.len(), 1, "expected exactly one finding");
        assert!(
            findings[0].message.contains("password_login but no bearer"),
            "wrong finding: {}",
            findings[0].message
        );
    }

    #[test]
    fn check_auth_methods_disabled_auth_skipped() {
        // enabled = false → not surveyed
        let mut def = CollectionDefinition::new("users");
        def.auth = Some(Auth {
            enabled: false,
            methods: vec![AuthMethod::password_login()],
            ..Default::default()
        });
        let reg = registry_with(vec![def]);
        let mut findings = Vec::new();
        check_auth_methods(&reg, &mut findings);
        assert!(findings.is_empty());
    }

    #[test]
    fn check_auth_methods_multiple_always_strategies_on_same_surface_warns() {
        let mtls_strategy = AuthMethod::Strategy {
            name: "mtls".to_string(),
            authenticate: HookRef::new("hooks.auth.mtls"),
            activates_on: Activation::always(),
            surfaces: SurfaceSet::admin_only(),
        };
        let proxy_strategy = AuthMethod::Strategy {
            name: "proxy".to_string(),
            authenticate: HookRef::new("hooks.auth.proxy"),
            activates_on: Activation::always(),
            surfaces: SurfaceSet::admin_only(),
        };
        let reg = registry_with(vec![
            auth_def_with("users", vec![AuthMethod::bearer(), mtls_strategy]),
            auth_def_with("admins", vec![AuthMethod::bearer(), proxy_strategy]),
        ]);
        let mut findings = Vec::new();
        check_auth_methods(&reg, &mut findings);
        assert!(
            findings
                .iter()
                .any(|f| f.message.contains("Multiple always-active")),
            "expected always-collision warning, got: {:?}",
            findings.iter().map(|f| &f.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn check_auth_methods_header_activated_strategy_does_not_warn() {
        let api_key = strategy_on_header(
            "api-key",
            "hooks.auth.api_key",
            "x-api-key",
            SurfaceSet::grpc_only(),
        );
        let reg = registry_with(vec![auth_def_with(
            "users",
            vec![AuthMethod::password_login(), AuthMethod::bearer(), api_key],
        )]);
        let mut findings = Vec::new();
        check_auth_methods(&reg, &mut findings);
        assert!(findings.is_empty(), "header-discriminated strategy is fine");
    }

    #[test]
    fn check_auth_methods_header_overlap_across_collections_warns() {
        // Two different collections both register a strategy bound
        // to the same header on the same surface. Whichever fires
        // first depends on HashMap iteration order — almost always
        // a config mistake.
        let mk_strategy = |name: &str| {
            strategy_on_header(
                name,
                "hooks.auth.api_key",
                "x-api-key",
                SurfaceSet::grpc_only(),
            )
        };
        let reg = registry_with(vec![
            auth_def_with(
                "users",
                vec![
                    AuthMethod::password_login(),
                    AuthMethod::bearer(),
                    mk_strategy("api-key"),
                ],
            ),
            auth_def_with(
                "service_accounts",
                vec![
                    AuthMethod::password_login(),
                    AuthMethod::bearer(),
                    mk_strategy("svc-key"),
                ],
            ),
        ]);
        let mut findings = Vec::new();
        check_auth_methods(&reg, &mut findings);
        assert!(
            findings
                .iter()
                .any(|f| f.message.contains("strategies bound to header 'x-api-key'")),
            "expected header-overlap warning, got: {:?}",
            findings.iter().map(|f| &f.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn check_auth_methods_header_case_difference_still_collides() {
        // `X-API-KEY` and `x-api-key` are the same HTTP header.
        // Activation matching lowercases for comparison; the status
        // check must do the same to catch operator-typo collisions.
        let reg = registry_with(vec![
            auth_def_with(
                "users",
                vec![
                    AuthMethod::password_login(),
                    AuthMethod::bearer(),
                    strategy_on_header(
                        "lower",
                        "hooks.auth.lower",
                        "x-api-key",
                        SurfaceSet::grpc_only(),
                    ),
                ],
            ),
            auth_def_with(
                "service_accounts",
                vec![
                    AuthMethod::password_login(),
                    AuthMethod::bearer(),
                    strategy_on_header(
                        "upper",
                        "hooks.auth.upper",
                        "X-API-KEY",
                        SurfaceSet::grpc_only(),
                    ),
                ],
            ),
        ]);
        let mut findings = Vec::new();
        check_auth_methods(&reg, &mut findings);
        assert!(
            findings
                .iter()
                .any(|f| f.message.contains("strategies bound to header 'x-api-key'")),
            "case-different header names should still collide: {:?}",
            findings.iter().map(|f| &f.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn check_auth_methods_distinct_headers_do_not_collide() {
        // Two strategies, each bound to its own header → no warning.
        let reg = registry_with(vec![auth_def_with(
            "users",
            vec![
                AuthMethod::password_login(),
                AuthMethod::bearer(),
                strategy_on_header(
                    "api-key",
                    "hooks.auth.api_key",
                    "x-api-key",
                    SurfaceSet::grpc_only(),
                ),
                strategy_on_header(
                    "sso",
                    "hooks.auth.sso",
                    "x-sso-assertion",
                    SurfaceSet::admin_only(),
                ),
            ],
        )]);
        let mut findings = Vec::new();
        check_auth_methods(&reg, &mut findings);
        assert!(
            findings.is_empty(),
            "distinct headers should not collide: {:?}",
            findings.iter().map(|f| &f.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn check_auth_methods_same_header_different_surfaces_does_not_collide() {
        // Same header name but the strategies are scoped to different
        // surfaces — they never race because they fire on different
        // request paths.
        let reg = registry_with(vec![
            auth_def_with(
                "users",
                vec![
                    AuthMethod::password_login(),
                    AuthMethod::bearer(),
                    strategy_on_header(
                        "admin-key",
                        "hooks.auth.admin",
                        "x-api-key",
                        SurfaceSet::admin_only(),
                    ),
                ],
            ),
            auth_def_with(
                "service_accounts",
                vec![
                    AuthMethod::password_login(),
                    AuthMethod::bearer(),
                    strategy_on_header(
                        "grpc-key",
                        "hooks.auth.grpc",
                        "x-api-key",
                        SurfaceSet::grpc_only(),
                    ),
                ],
            ),
        ]);
        let mut findings = Vec::new();
        check_auth_methods(&reg, &mut findings);
        assert!(
            findings.is_empty(),
            "same header on different surfaces should not collide: {:?}",
            findings.iter().map(|f| &f.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn check_auth_methods_single_always_strategy_does_not_warn() {
        // Only the multi-collection collision is a warning; one
        // always-active strategy on its own is the operator's call
        // (the startup validator already logs an info-level warning).
        let reg = registry_with(vec![auth_def_with(
            "users",
            vec![
                AuthMethod::bearer(),
                strategy_always("mtls", "hooks.auth.mtls"),
            ],
        )]);
        let mut findings = Vec::new();
        check_auth_methods(&reg, &mut findings);
        assert!(findings.is_empty());
    }
}
