//! `status --check` — best-practice audit for project configuration.
//!
//! Inspects the loaded config, registry, and database state for common
//! misconfigurations, performance risks, and missing best practices.
//! Returns the number of warnings emitted (0 = clean bill of health).

use std::path::Path;

use crate::{
    cli,
    config::{CompressionMode, CrapConfig},
    core::{
        Registry,
        collection::{Hooks, LiveMode},
    },
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
pub fn run_checks(
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

    if cfg.cache.backend == "none" && has_relationships && reg.collections.len() > 3 {
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
        .any(|d| d.auth.as_ref().is_some_and(|auth| auth.verify_email));

    if has_verify_email && cfg.email.provider == "log" {
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
