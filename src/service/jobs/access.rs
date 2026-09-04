//! Job-run read access — the gate shared by `list_job_runs` and `get_job_run`.
//!
//! A job has a single access hook (`JobDefinition.access`). `queue_job`
//! invokes it with operation `"trigger"`; the run-read paths invoke the
//! same hook with operation `"read"`, so an operator can branch on
//! `ctx.operation` to allow read-only viewers, hide run payloads from
//! triggerers, or share one rule for both. A job without an access hook is
//! readable by any authenticated caller — parity with `queue_job`, which
//! only enforces when `access.is_some()`.

use crate::{
    core::{Registry, job::JobDefinition},
    db::{AccessResult, DbConnection},
    hooks::AccessCheckInput,
    service::{ServiceContext, ServiceError},
};

/// Whether `ctx.user` may read runs of `job_def` (slug `slug`).
///
/// Mirrors `queue_job`'s trigger gate but with operation `"read"`. Job
/// access is allow/deny only — a returned filter table is a
/// misconfiguration (run reads are not row-filterable).
pub(super) fn can_read_job_runs(
    ctx: &ServiceContext,
    conn: &dyn DbConnection,
    job_def: &JobDefinition,
    slug: &str,
) -> Result<bool, ServiceError> {
    let Some(access) = job_def.access.as_ref() else {
        return Ok(true);
    };

    let input = AccessCheckInput::builder("read", slug)
        .access(Some(access))
        .user(ctx.user)
        .build();

    // One rule, two evaluators — the same split every CRUD path already
    // uses. A context carrying `write_hooks` evaluates through them
    // (`LuaWriteHooks` runs in the CALLER's VM, so a hook or job handler
    // never re-enters the VM pool); otherwise the runner is used directly,
    // which is what the gRPC and MCP surfaces do.
    let result = match ctx.write_hooks {
        Some(hooks) => hooks.check_access(&input),
        None => ctx.runner()?.check_access(&input, conn),
    }
    .map_err(ServiceError::Internal)?;

    match result {
        AccessResult::Allowed => Ok(true),
        AccessResult::Denied => Ok(false),
        AccessResult::Constrained(_) => Err(ServiceError::HookError(format!(
            "Access hook for job '{slug}' returned a filter table; job run reads are allow/deny only — return true/false based on ctx.user fields instead."
        ))),
    }
}

/// The set of job slugs whose runs `ctx.user` may read, across all
/// registered jobs. Used by `ListJobs` to hide jobs the caller can't read,
/// keeping definition-listing consistent with run-reading.
///
/// # Errors
///
/// Propagates a job access hook error or a backend error.
pub fn readable_job_slugs(
    ctx: &ServiceContext,
    conn: &dyn DbConnection,
    registry: &Registry,
) -> Result<Vec<String>, ServiceError> {
    readable_slugs(ctx, conn, registry, None)
}

/// Resolve the set of job slugs whose runs `ctx.user` may read.
///
/// When `requested` is `Some(slug)`, the result is either `[slug]` (the job
/// exists and grants read) or empty (unknown slug, or denied) — hiding
/// existence on denial. When `requested` is `None`, every job the caller may
/// read is returned. Runs whose job definition no longer exists in the
/// registry (job removed from config) are never reported — fail closed.
pub(super) fn readable_slugs(
    ctx: &ServiceContext,
    conn: &dyn DbConnection,
    registry: &Registry,
    requested: Option<&str>,
) -> Result<Vec<String>, ServiceError> {
    if let Some(slug) = requested {
        let Some(job_def) = registry.get_job(slug) else {
            return Ok(Vec::new());
        };

        if can_read_job_runs(ctx, conn, job_def, slug)? {
            return Ok(vec![slug.to_string()]);
        }

        return Ok(Vec::new());
    }

    let mut allowed = Vec::new();
    for (slug, job_def) in &registry.jobs {
        if can_read_job_runs(ctx, conn, job_def, slug)? {
            allowed.push(slug.to_string());
        }
    }

    Ok(allowed)
}
