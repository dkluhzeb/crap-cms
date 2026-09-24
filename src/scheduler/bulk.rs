//! `_system_bulk` execution — queued bulk create/update/delete.
//!
//! The counterpart of `service::jobs::bulk_queue`: rebuilds the operation
//! from the stored [`BulkJobData`] and runs it through the SAME op core the
//! synchronous surfaces use (`op::run`) — identical access checks, hooks,
//! atomicity, and event semantics — under the actor snapshotted at queue
//! time. The result summary lands on the job run for `GetJobRun` polling.

use std::{sync::Arc, time::Instant};

use anyhow::{Context as _, Result};
use serde_json::json;
use tracing::{error, info};

use crate::{
    core::{Document, DocumentFields, job::JobRun},
    db::{
        DbConnection, DbPool, LocaleContext, query::filter::decode_where_json_str,
        query::jobs as job_query,
    },
    service::{
        AppInfra, CreateManyItem, OpDeadline, ServiceContext, ServiceError,
        auth::{get_session_version, is_locked, load_user},
        jobs::bulk_queue::{BulkJobData, BulkOpKind, QueuedBy, finished_payload},
        op::{
            self, CoreError, CreateMany, CreateManyArgs, DeleteMany, DeleteManyArgs, Principal,
            TargetRef, UpdateMany, UpdateManyArgs,
        },
    },
};

use super::runner::record_permanent_job_failure;

/// Inputs for [`execute_system_bulk`].
pub(super) struct ExecuteBulkParams<'a> {
    pub pool: &'a DbPool,
    pub app_infra: Option<&'a Arc<AppInfra>>,
    pub job_run: &'a JobRun,
    pub start: Instant,
    /// The run's wall-clock budget (`[jobs.queues.bulk] timeout`), used as
    /// the cooperative [`OpDeadline`]: the batch aborts and rolls back
    /// *itself* at this point, while the scheduler's uncancellable outer
    /// timer runs with extra grace and only fires for a stuck run.
    pub timeout_secs: u64,
}

/// Execute one `_system_bulk` run.
///
/// Every failure is recorded PERMANENTLY: the bulk queue defaults to zero
/// retries because a retry after the batch already committed (crash in the
/// window before the completion mark) would re-apply the whole batch.
/// Re-queue explicitly instead.
///
/// # Errors
///
/// Returns an error only when recording the outcome itself fails.
pub(super) fn execute_system_bulk(p: &ExecuteBulkParams<'_>) -> Result<()> {
    let label = format!("Bulk job {}", p.job_run.id);

    let Some(infra) = p.app_infra else {
        return fail_bulk_run(
            p,
            &label,
            "bulk execution requires the app infra bundle (not available in this worker context)",
        );
    };

    let data: BulkJobData = match serde_json::from_str(&p.job_run.data) {
        Ok(d) => d,
        Err(e) => {
            return fail_bulk_run(p, &label, &format!("invalid bulk job data: {e}"));
        }
    };

    let BulkJobData {
        op: kind,
        collection,
        queued_by,
        locale,
        ui_locale,
        draft,
        hooks,
        events,
        max_documents,
        documents,
        where_clause,
        data: set_data,
        force_hard_delete,
    } = data;

    // Re-load the queuing user instead of trusting a snapshot: a lock,
    // deletion, or session revocation between queueing and execution is
    // therefore OBSERVED, and no user fields were persisted in the jobs
    // table in the first place.
    let principal = match resolve_run_principal(infra, &queued_by, ui_locale) {
        Ok(principal) => principal,
        Err(reason) => {
            return fail_bulk_run(p, &label, &reason);
        }
    };

    let locale_ctx =
        match LocaleContext::from_locale_string(locale.as_deref(), &infra.locale_config) {
            Ok(l) => l,
            Err(e) => {
                return fail_bulk_run(p, &label, &format!("invalid locale: {e}"));
            }
        };

    // The configured budget IS the cooperative deadline: the scheduler's
    // outer timer runs with extra grace for this slug, so the in-batch
    // abort always wins. `0` means "no timer" upstream; keep that meaning.
    let deadline = if p.timeout_secs == 0 {
        OpDeadline::none()
    } else {
        OpDeadline::in_secs(p.timeout_secs)
    };

    let outcome = run_bulk_op(
        infra,
        principal,
        &TargetRef::collection(collection),
        RunBulkOp {
            kind,
            deadline,
            locale_ctx,
            draft,
            hooks,
            events,
            max_documents,
            documents,
            where_clause,
            data: set_data,
            force_hard_delete,
        },
    );

    match outcome {
        Ok(summary) => record_bulk_success(p, &label, &summary),
        Err(error_msg) => fail_bulk_run(p, &label, &error_msg),
    }
}

/// Record a run's permanent failure and drop its request payload: `failed` is
/// terminal too (frozen contract: a finished run's request body is dropped once
/// it reaches a terminal status), so the caller's documents/patch/filter must
/// not sit at rest for the retention window just because the run failed.
///
/// # Errors
///
/// Returns an error when the failure itself cannot be recorded.
fn fail_bulk_run(p: &ExecuteBulkParams<'_>, label: &str, reason: &str) -> Result<()> {
    let result = record_permanent_job_failure(p.pool, p.job_run, label, reason);

    match p.pool.write() {
        Ok(conn) => strip_finished_payload(&conn, p.job_run),
        Err(e) => error!("bulk job: no connection to strip the finished payload: {e}"),
    }

    result
}

/// Mark a finished batch completed and drop its request payload.
///
/// # Errors
///
/// Returns an error when the completion mark itself cannot be written.
fn record_bulk_success(p: &ExecuteBulkParams<'_>, label: &str, summary: &str) -> Result<()> {
    // The write pool, like every job-row write: a write on a read connection
    // starves the readers the pool split protects.
    let conn = p
        .pool
        .write()
        .context("Failed to get DB connection for bulk completion")?;

    // Repairing form: if the scheduler's outer timer already stamped this
    // run failed while the batch was finishing its post-commit work, the
    // CAS would silently no-op and leave a COMMITTED batch recorded as a
    // failure. Repair it loudly.
    let repaired =
        job_query::complete_job_repairing(&conn, &p.job_run.id, p.job_run.attempt, Some(summary))?;

    if repaired {
        error!(
            "{label} was marked failed by the job timer but its batch COMMITTED — \
             the run has been corrected to completed. Raise \
             `[jobs.queues.bulk] timeout` if this recurs."
        );
    }

    strip_finished_payload(&conn, p.job_run);

    info!("{label} completed in {:?}", p.start.elapsed());

    Ok(())
}

/// Once a run is finished its request body (documents / patch / filter) can
/// never be needed again, but it would otherwise sit in `_crap_jobs.data`
/// until the retention purge. Replace it with just what the visibility rules
/// need — the queuer and the collection — so the caller's submitted values
/// are not kept at rest for the retention window. Every path that moves a
/// `_system_bulk` run to a terminal status calls this.
pub(super) fn strip_finished_payload(conn: &dyn DbConnection, job_run: &JobRun) {
    let stripped = finished_payload(&job_run.data);

    if let Err(e) = job_query::set_job_data(conn, &job_run.id, &stripped) {
        error!("bulk job: could not strip the finished payload: {e:#}");
    }
}

/// Map an operation failure to the message stored on the job run (and
/// returned by `GetJobRun`). Internal/transient details are logged
/// server-side and replaced with a generic message — the same discipline
/// the synchronous codecs apply, which a raw `{:?}` of the error chain
/// (SQL text, column names) would bypass.
fn user_facing_error(e: CoreError) -> String {
    let service_error = e.into_service_error();

    match &service_error {
        ServiceError::Internal(detail) => {
            error!("bulk job internal error: {detail:#}");
            "internal error (see server logs)".to_string()
        }
        ServiceError::Transient(detail) => {
            error!("bulk job transient error: {detail:#}");
            "temporary backend failure — re-queue the operation".to_string()
        }
        other => other.to_string(),
    }
}

/// Rebuild the principal a run executes as. The stored identity is a
/// REFERENCE, so the user is re-loaded here: a lock, deletion, or session
/// revocation between queueing and execution is therefore OBSERVED, and no
/// user fields were persisted in the jobs table in the first place.
fn resolve_run_principal(
    infra: &AppInfra,
    queued_by: &QueuedBy,
    ui_locale: Option<String>,
) -> Result<Principal, String> {
    match queued_by {
        QueuedBy::System => Ok(Principal::Override),
        QueuedBy::User {
            id,
            collection,
            session_version,
        } => load_queuing_user(infra, collection, id, *session_version).map(|user| {
            Principal::Resolved {
                user: Some(user),
                ui_locale,
            }
        }),
    }
}

/// The run's stored error for a lookup that failed for an internal reason.
/// The detail (SQL text, column names) is logged, never stored on the run —
/// the same discipline `user_facing_error` applies.
fn unresolved_queuer(detail: &str) -> String {
    error!("bulk job: resolving the queuing user failed: {detail}");

    "the queuing user could not be resolved — the run was abandoned".to_string()
}

/// Re-load the user a run was queued by, refusing to execute when the
/// account no longer exists — deleted or moved to the trash — or has been
/// locked or had its sessions revoked since.
fn load_queuing_user(
    infra: &AppInfra,
    collection: &str,
    id: &str,
    session_version: u64,
) -> Result<Document, String> {
    let def = infra
        .registry
        .get_collection(collection)
        .ok_or_else(|| format!("auth collection '{collection}' is no longer defined"))?;

    let conn = infra
        .pool
        .get()
        .map_err(|e| unresolved_queuer(&format!("DB connection: {e}")))?;

    // The locale config lets `load_user` read a localized auth collection's
    // default-locale columns.
    let ctx = ServiceContext::collection(collection, def)
        .conn(&conn)
        .locale_config(Some(&infra.locale_config))
        .build();

    // `None` for a trashed account as for a deleted one: neither may act.
    let Some(user) = load_user(&ctx, id).map_err(|e| unresolved_queuer(&e.to_string()))? else {
        return Err(format!(
            "the queuing user ({collection}/{id}) no longer exists — the run was abandoned"
        ));
    };

    ensure_queuer_still_active(&ctx, id, session_version)?;

    Ok(user)
}

/// Refuse a queuer that was locked, or whose sessions were revoked, after
/// the run was queued.
fn ensure_queuer_still_active(
    ctx: &ServiceContext,
    id: &str,
    session_version: u64,
) -> Result<(), String> {
    let collection = ctx.slug;

    if is_locked(ctx, id).map_err(|e| unresolved_queuer(&e.to_string()))? {
        return Err(format!(
            "the queuing user ({collection}/{id}) is locked — the run was abandoned"
        ));
    }

    // A session-version bump (force-logout, password reset, unverify)
    // revokes every live token; it must revoke pending work too.
    let current_version = get_session_version(ctx, id)
        .map_err(|e| unresolved_queuer(&format!("session version: {e}")))?;

    if current_version != session_version {
        return Err(format!(
            "the queuing user's session was revoked ({collection}/{id}) — the run was abandoned"
        ));
    }

    Ok(())
}

/// The decoded pieces of a bulk job payload, ready to rebuild op args.
struct RunBulkOp {
    kind: BulkOpKind,
    deadline: OpDeadline,
    locale_ctx: Option<LocaleContext>,
    draft: bool,
    hooks: bool,
    events: bool,
    max_documents: i64,
    documents: Option<Vec<DocumentFields>>,
    where_clause: Option<String>,
    data: Option<DocumentFields>,
    force_hard_delete: bool,
}

/// Rebuild and run the operation through the shared op core, returning the
/// JSON result summary or a human-readable failure message.
fn run_bulk_op(
    infra: &AppInfra,
    principal: Principal,
    target: &TargetRef,
    p: RunBulkOp,
) -> Result<String, String> {
    let RunBulkOp {
        kind,
        deadline,
        locale_ctx,
        draft,
        hooks,
        events,
        max_documents,
        documents,
        where_clause,
        data: set_data,
        force_hard_delete,
    } = p;

    // The same wire→filter chokepoint the synchronous gRPC codecs use
    // (`parse_where_json` delegates to it), so a queued run and a direct
    // call decode the stored `where` identically.
    let decode_filters = || match where_clause.as_deref() {
        None => Ok(Vec::new()),
        Some(json) => decode_where_json_str(json).map_err(|e| format!("invalid where clause: {e}")),
    };

    match kind {
        BulkOpKind::CreateMany => {
            let items = documents
                .unwrap_or_default()
                .into_iter()
                .map(|data| CreateManyItem {
                    data,
                    password: None,
                })
                .collect();

            let args = CreateManyArgs::builder(items)
                .run_hooks(hooks)
                .draft(draft)
                .locale_ctx(locale_ctx)
                .max_documents(max_documents)
                .deadline(deadline)
                .events(events)
                .build();

            op::run::<CreateMany>(infra, principal, target, args)
                .map(|r| json!({ "created": r.created }).to_string())
                .map_err(user_facing_error)
        }
        BulkOpKind::UpdateMany => match decode_filters() {
            Ok(filters) => {
                let args = UpdateManyArgs::builder(filters, set_data.unwrap_or_default())
                    .locale_ctx(locale_ctx)
                    .run_hooks(hooks)
                    .draft(draft)
                    .max_documents(max_documents)
                    .deadline(deadline)
                    .events(events)
                    .build();

                op::run::<UpdateMany>(infra, principal, target, args)
                    .map(|r| json!({ "modified": r.modified }).to_string())
                    .map_err(user_facing_error)
            }
            Err(e) => Err(e),
        },
        BulkOpKind::DeleteMany => match decode_filters() {
            Ok(filters) => {
                let args = DeleteManyArgs::builder(filters)
                    .run_hooks(hooks)
                    .force_hard_delete(force_hard_delete)
                    .max_documents(max_documents)
                    .deadline(deadline)
                    .events(events)
                    .build();

                op::run::<DeleteMany>(infra, principal, target, args)
                    .map(|r| {
                        json!({
                            "deleted": r.hard_deleted,
                            "soft_deleted": r.soft_deleted,
                            "skipped": r.skipped,
                        })
                        .to_string()
                    })
                    .map_err(user_facing_error)
            }
            Err(e) => Err(e),
        },
    }
}
