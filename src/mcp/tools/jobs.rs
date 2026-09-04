//! MCP job tools — background-job introspection and triggering.
//!
//! Mirrors the gRPC job RPCs (`ListJobs`, `GetJobRun`, `ListJobRuns`,
//! `TriggerJob`) so an MCP client can follow up on work it started: notably
//! the `job_id` returned by a queued bulk operation (`queue = true`), plus
//! inspection and failure triage for user-defined jobs. The framework's own
//! `_system_email` / `_system_image_convert` runs are NOT visible here
//! (they have no job definition, and their payloads carry delivery tokens);
//! `_system_bulk` is the sole system slug these tools can read.
//!
//! **Tiering.** `[mcp] job_tools` is three-state: `false` (default, no job
//! tools at all), `"read"` (the introspection trio), `"all"` (adds
//! `trigger_job`). Reading and executing carry different risk, so they are
//! separable: `"read"` lets an assistant diagnose without gaining the power
//! to run jobs. Every tier is enforced at execution, not only in
//! `tools/list`, because a client can call a name it was never shown.
//!
//! Note `trigger_job` still runs the job's own `access` hook via
//! `service::jobs::queue_job` — but with `ctx.user = nil`, since MCP has no
//! end user. A hook that errors (including on a nil deref) is treated as a
//! DENY, so this is fail-closed; a job declared with no `access` hook at
//! all is open to any caller that reaches this tool.

use anyhow::{Context as _, Result, bail};
use serde::Serialize;
use serde_json::{Value, json, to_string_pretty};
use tracing::info;

use crate::{
    config::McpJobTools,
    core::job::JobRun,
    mcp::{protocol::ToolDefinition, tools::ToolExecCtx},
    service::{
        self, ServiceContext,
        jobs::{
            ListJobRunsInput,
            bulk_queue::{self, BulkJobData, BulkOpKind, QueuedBy},
        },
    },
};

pub(in crate::mcp) const TOOL_LIST_JOBS: &str = "list_jobs";
pub(in crate::mcp) const TOOL_GET_JOB_RUN: &str = "get_job_run";
pub(in crate::mcp) const TOOL_LIST_JOB_RUNS: &str = "list_job_runs";
pub(in crate::mcp) const TOOL_TRIGGER_JOB: &str = "trigger_job";
pub(in crate::mcp) const TOOL_CANCEL_JOB_RUN: &str = "cancel_job_run";

/// Default page size for `list_job_runs`, matching the gRPC RPC.
const DEFAULT_RUN_LIMIT: i64 = 50;

/// The job tools for the configured tier: none for `false`, the read trio
/// for `"read"`, plus `trigger_job` for `"all"`.
pub(in crate::mcp) fn job_tools(mode: McpJobTools) -> Vec<ToolDefinition> {
    if !mode.reads() {
        return Vec::new();
    }

    let mut tools = vec![
        ToolDefinition::new(
            TOOL_LIST_JOBS,
            "List defined background jobs (slug, queue, schedule, timeout, priority)",
            json!({ "type": "object", "properties": {} }),
        ),
        ToolDefinition::new(
            TOOL_GET_JOB_RUN,
            "Get the status and result of one job run by id — use this to poll a \
             job_id returned by a queued bulk operation",
            json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "The job run id" }
                },
                "required": ["id"]
            }),
        ),
        ToolDefinition::new(
            TOOL_LIST_JOB_RUNS,
            "List recent job runs, newest first. Filter by job slug and/or status \
             to triage failures (e.g. status = \"failed\")",
            json!({
                "type": "object",
                "properties": {
                    "slug": { "type": "string", "description": "Only runs of this job slug" },
                    "status": {
                        "type": "string",
                        "enum": ["pending", "running", "completed", "failed", "stale"],
                        "description": "Only runs in this status"
                    },
                    "limit": { "type": "integer", "description": "Max runs to return (default 50)" },
                    "offset": { "type": "integer", "description": "Runs to skip (default 0)" }
                }
            }),
        ),
    ];

    if mode.trigger() {
        tools.push(ToolDefinition::new(
            TOOL_CANCEL_JOB_RUN,
            "Cancel a job run that has not started yet — e.g. a queued bulk \
             operation you no longer want. A run already in flight cannot be \
             stopped",
            json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "The job run id to cancel" }
                },
                "required": ["id"]
            }),
        ));
        tools.push(ToolDefinition::new(
            TOOL_TRIGGER_JOB,
            "Queue a defined job for immediate execution. Returns the job run id",
            json!({
                "type": "object",
                "properties": {
                    "slug": { "type": "string", "description": "The job slug to trigger" },
                    "data": { "type": "object", "description": "JSON payload passed to the handler" },
                    "priority": { "type": "integer", "description": "Scheduling priority; higher runs sooner" }
                },
                "required": ["slug"]
            }),
        ));
    }

    tools
}

/// The per-operation pieces of a queued bulk request, as the MCP bulk
/// tools have them at the point they decide to queue.
pub(in crate::mcp::tools) struct QueuedFields {
    pub locale: Option<String>,
    pub draft: bool,
    pub hooks: bool,
    pub events: bool,
    pub documents: Option<Vec<crate::core::DocumentFields>>,
    /// The MCP `where` argument, re-serialized to the stored JSON-string
    /// form so a queued run decodes it through the same chokepoint a
    /// synchronous call uses.
    pub where_clause: Option<String>,
    pub data: Option<crate::core::DocumentFields>,
    pub force_hard_delete: bool,
}

/// Queue a bulk operation from an MCP tool and return `{"job_id": …}`.
///
/// MCP executes with `override_access`, so the run is stamped
/// [`QueuedBy::System`] — visible to override callers (i.e. MCP's own
/// `get_job_run`), never to end users through the gRPC job RPCs.
///
/// # Errors
///
/// Returns an error when the payload cannot be serialized or the job
/// insert fails.
pub(in crate::mcp::tools) fn queue_bulk_tool(
    ctx: &ToolExecCtx<'_>,
    slug: &str,
    kind: BulkOpKind,
    f: QueuedFields,
) -> Result<String> {
    // Never hand back a job id the client has no tool to poll. The bulk
    // tool schemas omit `queue` in this state too; this is the execution
    // half of that rule.
    if !ctx.config.mcp.job_tools.reads() {
        bail!(
            "queue requires the job tools — set job_tools = \"read\" (or \"all\") in \
             [mcp] config so the resulting job_id can be polled with get_job_run"
        );
    }

    let data = BulkJobData {
        op: kind,
        collection: slug.to_string(),
        queued_by: QueuedBy::System,
        locale: f.locale,
        ui_locale: None,
        draft: f.draft,
        hooks: f.hooks,
        events: f.events,
        max_documents: ctx.config.server.bulk_max_documents,
        documents: f.documents,
        where_clause: f.where_clause,
        data: f.data,
        force_hard_delete: f.force_hard_delete,
    };

    let run = bulk_queue::queue_bulk(&ctx.infra.pool, &data)
        .map_err(crate::service::ServiceError::into_anyhow)?;

    info!(
        "MCP queued bulk {:?} {}: job {} [client={}]",
        kind, slug, run.id, ctx.client_label
    );

    Ok(to_string_pretty(&json!({ "job_id": run.id }))?)
}

/// Whether `name` is one of the job tools.
pub(in crate::mcp) fn is_job_tool(name: &str) -> bool {
    matches!(
        name,
        TOOL_LIST_JOBS
            | TOOL_GET_JOB_RUN
            | TOOL_LIST_JOB_RUNS
            | TOOL_TRIGGER_JOB
            | TOOL_CANCEL_JOB_RUN
    )
}

/// The wire shape of a job run returned to MCP clients.
#[derive(Serialize)]
struct JobRunView {
    id: String,
    slug: String,
    status: String,
    queue: String,
    attempt: u32,
    max_attempts: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    created_at: Option<String>,
}

impl From<&JobRun> for JobRunView {
    fn from(run: &JobRun) -> Self {
        Self {
            id: run.id.clone(),
            slug: run.slug.clone(),
            status: run.status.as_str().to_string(),
            queue: run.queue.clone(),
            attempt: run.attempt,
            max_attempts: run.max_attempts,
            // Surface the handler's summary as real JSON when it is JSON
            // (queued-bulk runs return `{"created":N}` etc.).
            result: run
                .result
                .as_deref()
                .map(|r| serde_json::from_str(r).unwrap_or_else(|_| json!(r))),
            error: run.error.clone(),
            created_at: run.created_at.clone(),
        }
    }
}

/// Build the MCP service context: override access (the transport's API key
/// is the boundary), no user.
fn job_ctx<'a>(
    conn: &'a crate::db::BoxedConnection,
    ctx: &'a ToolExecCtx<'_>,
    slug: &'a str,
) -> ServiceContext<'a> {
    ServiceContext::slug_only(slug)
        .conn(conn)
        .runner(&ctx.infra.hook_runner)
        .override_access(true)
        .build()
}

/// Execute one job tool.
///
/// # Errors
///
/// Returns an error for a missing/invalid argument, an unknown run or job
/// slug, a disabled `trigger_job`, or a backend failure.
pub(in crate::mcp) fn exec_job_tool(
    name: &str,
    args: &Value,
    ctx: &ToolExecCtx<'_>,
) -> Result<String> {
    // Enforced at execution, not only in `tools/list`: an MCP client can
    // call a tool name it was never shown.
    let mode = ctx.config.mcp.job_tools;
    if !mode.reads() {
        bail!("Job tools are not enabled. Set job_tools = \"read\" (or \"all\") in [mcp] config.");
    }

    match name {
        TOOL_LIST_JOBS => exec_list_jobs(ctx),
        TOOL_GET_JOB_RUN => exec_get_job_run(args, ctx),
        TOOL_LIST_JOB_RUNS => exec_list_job_runs(args, ctx),
        TOOL_CANCEL_JOB_RUN => {
            if !mode.trigger() {
                bail!(
                    "Cancelling job runs is not enabled. Set job_tools = \"all\" in [mcp] \
                     config (current: \"{}\")",
                    mode.as_str()
                );
            }
            exec_cancel_job_run(args, ctx)
        }
        TOOL_TRIGGER_JOB => {
            if !mode.trigger() {
                bail!(
                    "Job triggering is not enabled. Set job_tools = \"all\" in [mcp] config \
                     (current: \"{}\")",
                    mode.as_str()
                );
            }
            exec_trigger_job(args, ctx)
        }
        _ => bail!("Unknown job tool: {name}"),
    }
}

fn exec_list_jobs(ctx: &ToolExecCtx<'_>) -> Result<String> {
    let jobs: Vec<Value> = ctx
        .infra
        .registry
        .jobs
        .values()
        .map(|def| {
            json!({
                "slug": def.slug.as_ref(),
                "queue": def.queue,
                "schedule": def.schedule,
                "timeout": def.timeout,
                "priority": def.priority,
            })
        })
        .collect();

    Ok(to_string_pretty(&json!({ "jobs": jobs }))?)
}

fn exec_get_job_run(args: &Value, ctx: &ToolExecCtx<'_>) -> Result<String> {
    let id = args
        .get("id")
        .and_then(Value::as_str)
        .context("Missing 'id' argument")?;

    let conn = ctx.infra.pool.get().context("DB connection")?;
    let svc = job_ctx(&conn, ctx, "");

    let run = service::jobs::get_job_run(&svc, ctx.infra.registry.as_ref(), id)
        .map_err(crate::service::ServiceError::into_anyhow)?
        .with_context(|| format!("Job run '{id}' not found"))?;

    Ok(to_string_pretty(&JobRunView::from(&run))?)
}

fn exec_list_job_runs(args: &Value, ctx: &ToolExecCtx<'_>) -> Result<String> {
    let conn = ctx.infra.pool.get().context("DB connection")?;
    let svc = job_ctx(&conn, ctx, "");

    let slug = args.get("slug").and_then(Value::as_str);
    let status = args.get("status").and_then(Value::as_str);
    let limit = args
        .get("limit")
        .and_then(Value::as_i64)
        .unwrap_or(DEFAULT_RUN_LIMIT)
        .max(0);
    let offset = args
        .get("offset")
        .and_then(Value::as_i64)
        .unwrap_or(0)
        .max(0);

    let page = service::jobs::list_job_runs(
        &svc,
        &ListJobRunsInput {
            registry: ctx.infra.registry.as_ref(),
            slug,
            status,
            limit,
            offset,
        },
    )
    .map_err(crate::service::ServiceError::into_anyhow)?;

    let runs: Vec<JobRunView> = page.docs.iter().map(JobRunView::from).collect();

    Ok(to_string_pretty(&json!({
        "runs": runs,
        "total": page.total,
    }))?)
}

fn exec_cancel_job_run(args: &Value, ctx: &ToolExecCtx<'_>) -> Result<String> {
    let id = args
        .get("id")
        .and_then(Value::as_str)
        .context("Missing 'id' argument")?;

    let conn = ctx.infra.pool.get().context("DB connection")?;
    let svc = job_ctx(&conn, ctx, "");

    let cancelled = service::jobs::cancel_job_run(&svc, ctx.infra.registry.as_ref(), id)
        .map_err(crate::service::ServiceError::into_anyhow)?;

    info!(
        "MCP cancel_job_run: {} -> {} [client={}]",
        id, cancelled, ctx.client_label
    );

    Ok(to_string_pretty(&json!({ "cancelled": cancelled }))?)
}

fn exec_trigger_job(args: &Value, ctx: &ToolExecCtx<'_>) -> Result<String> {
    let slug = args
        .get("slug")
        .and_then(Value::as_str)
        .context("Missing 'slug' argument")?;

    let job_def = ctx
        .infra
        .registry
        .get_job(slug)
        .cloned()
        .with_context(|| format!("Job '{slug}' not found"))?;

    let data_json = match args.get("data") {
        Some(v) if !v.is_null() => serde_json::to_string(v)?,
        _ => "{}".to_string(),
    };

    let conn = ctx.infra.pool.get().context("DB connection")?;
    let svc = job_ctx(&conn, ctx, slug);

    let priority = args
        .get("priority")
        .and_then(Value::as_i64)
        .and_then(|p| i32::try_from(p).ok())
        .unwrap_or(job_def.priority);

    let queue_retries = ctx
        .config
        .jobs
        .queues
        .get(&job_def.queue)
        .and_then(|q| q.retries);

    let run = service::jobs::queue_job(
        &svc,
        &service::jobs::QueueJobInput {
            job_def: &job_def,
            data: Some(&data_json),
            scheduled_by: "mcp",
            priority,
            queue_retries,
        },
    )
    .map_err(crate::service::ServiceError::into_anyhow)?;

    info!(
        "MCP trigger_job: {} -> {} [client={}]",
        slug, run.id, ctx.client_label
    );

    Ok(to_string_pretty(&json!({ "job_id": run.id }))?)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::*;
    use crate::{
        config::{CrapConfig, McpJobTools},
        core::{CollectionDefinition, Registry, job::SYSTEM_BULK_JOB},
        db::{migrate, pool, query::jobs as job_query},
        hooks::lifecycle::HookRunner,
        mcp::tools::test_helpers::{make_exec_ctx, make_registry},
    };

    /// The three tiers list exactly what they promise: nothing, the read
    /// trio, or the trio plus `trigger_job`.
    #[test]
    fn tiers_list_exactly_their_tools() {
        let names = |mode: McpJobTools| -> Vec<String> {
            job_tools(mode).into_iter().map(|t| t.name).collect()
        };

        assert!(
            names(McpJobTools::Off).is_empty(),
            "`false` must expose no job tools — the flag would otherwise lie"
        );

        let read = names(McpJobTools::Read);
        assert!(read.contains(&TOOL_LIST_JOBS.to_string()));
        assert!(read.contains(&TOOL_GET_JOB_RUN.to_string()));
        assert!(read.contains(&TOOL_LIST_JOB_RUNS.to_string()));
        assert!(!read.contains(&TOOL_TRIGGER_JOB.to_string()));
        assert!(!read.contains(&TOOL_CANCEL_JOB_RUN.to_string()));

        let all = names(McpJobTools::All);
        assert_eq!(all.len(), read.len() + 2);
        assert!(all.contains(&TOOL_TRIGGER_JOB.to_string()));
        assert!(all.contains(&TOOL_CANCEL_JOB_RUN.to_string()));
    }

    #[test]
    fn is_job_tool_matches_exactly_the_four() {
        for n in [
            TOOL_LIST_JOBS,
            TOOL_GET_JOB_RUN,
            TOOL_LIST_JOB_RUNS,
            TOOL_TRIGGER_JOB,
            TOOL_CANCEL_JOB_RUN,
        ] {
            assert!(is_job_tool(n), "{n}");
        }
        assert!(!is_job_tool("find_posts"));
        assert!(!is_job_tool("list_collections"));
    }

    struct TestCtx {
        tmp: tempfile::TempDir,
        pool: crate::db::DbPool,
        registry: Arc<Registry>,
        runner: HookRunner,
        config: CrapConfig,
    }

    fn setup() -> TestCtx {
        setup_with(McpJobTools::Read)
    }

    fn setup_with(mode: McpJobTools) -> TestCtx {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = CrapConfig::test_default();
        config.database.path = "test.db".to_string();
        config.mcp.job_tools = mode;

        let db_pool = pool::create_pool(tmp.path(), &config).unwrap();

        let mut reg = make_registry();
        reg.register_collection(CollectionDefinition::new("notes"));
        let registry = Arc::new(reg);
        migrate::sync_all(&db_pool, &registry, &config.locale).unwrap();

        let runner = HookRunner::builder()
            .config_dir(tmp.path())
            .registry(Arc::clone(&registry))
            .config(&config)
            .build()
            .unwrap();

        TestCtx {
            tmp,
            pool: db_pool,
            registry,
            runner,
            config,
        }
    }

    /// A queued-bulk run (system-queued) is readable through the MCP
    /// `get_job_run` tool — MCP runs with override access, which is what
    /// makes `queue = true` usable from an MCP client at all.
    #[test]
    fn get_job_run_reads_a_queued_bulk_run() {
        let t = setup();
        let ctx = make_exec_ctx(&t.pool, &t.registry, &t.runner, &t.config, t.tmp.path());

        let run = {
            let conn = t.pool.get().unwrap();
            job_query::insert_job(
                &conn,
                SYSTEM_BULK_JOB,
                r#"{"op":"create_many","collection":"notes","queued_by":{"kind":"system"},"max_documents":0}"#,
                "mcp",
                1,
                "bulk",
                0,
            )
            .unwrap()
        };

        let out = exec_job_tool(TOOL_GET_JOB_RUN, &json!({ "id": run.id }), &ctx).unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();

        assert_eq!(parsed["id"], json!(run.id));
        assert_eq!(parsed["slug"], json!(SYSTEM_BULK_JOB));
        assert_eq!(parsed["status"], json!("pending"));

        // An unknown id is an error, not an empty success.
        assert!(exec_job_tool(TOOL_GET_JOB_RUN, &json!({ "id": "nope" }), &ctx).is_err());
    }

    /// Every job tool refuses at the `false` tier — the gate is enforced at
    /// execution, not only in `tools/list` (an MCP client can call a name it
    /// was never shown).
    #[test]
    fn all_job_tools_refuse_when_off() {
        let t = setup_with(McpJobTools::Off);
        let ctx = make_exec_ctx(&t.pool, &t.registry, &t.runner, &t.config, t.tmp.path());

        for name in [
            TOOL_LIST_JOBS,
            TOOL_GET_JOB_RUN,
            TOOL_LIST_JOB_RUNS,
            TOOL_TRIGGER_JOB,
            TOOL_CANCEL_JOB_RUN,
        ] {
            let err = exec_job_tool(name, &json!({ "id": "x", "slug": "y" }), &ctx)
                .unwrap_err()
                .to_string();
            assert!(err.contains("job_tools"), "{name}: {err}");
        }
    }

    /// `trigger_job` refuses at the `"read"` tier while the read tools work.
    #[test]
    fn trigger_job_is_refused_at_read_tier() {
        let t = setup();
        let ctx = make_exec_ctx(&t.pool, &t.registry, &t.runner, &t.config, t.tmp.path());

        let err = exec_job_tool(TOOL_TRIGGER_JOB, &json!({ "slug": "anything" }), &ctx)
            .unwrap_err()
            .to_string();

        assert!(err.contains("job_tools = \"all\""), "{err}");

        // …while a read tool at the same tier succeeds.
        assert!(exec_job_tool(TOOL_LIST_JOBS, &json!({}), &ctx).is_ok());
    }

    /// `list_jobs` reports the registry's defined jobs.
    #[test]
    fn list_jobs_reports_defined_jobs() {
        let t = setup();
        let ctx = make_exec_ctx(&t.pool, &t.registry, &t.runner, &t.config, t.tmp.path());

        let out = exec_job_tool(TOOL_LIST_JOBS, &json!({}), &ctx).unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();

        assert!(parsed["jobs"].is_array(), "{parsed}");
    }
}
