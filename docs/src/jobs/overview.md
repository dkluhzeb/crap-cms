# Jobs

Background job system for scheduled and queued tasks.

## Overview

Crap CMS includes a built-in job scheduler for running background tasks. Jobs are defined
in Lua, can be triggered manually or on a cron schedule, and execute with full CRUD access
to all collections.

Use cases:
- Scheduled cleanup (e.g., delete expired posts nightly)
- Async processing triggered from hooks (e.g., send welcome email after user creation)
- Periodic data sync or aggregation

## Defining Jobs

Jobs are defined via `crap.jobs.define()` in `init.lua` or files under `jobs/`:

```lua
-- jobs/cleanup_expired.lua
crap.jobs.define("cleanup_expired", {
    handler = "jobs.cleanup_expired.run",
    schedule = "0 3 * * *",        -- daily at 3am
    queue = "maintenance",
    retries = 3,
    timeout = 300,
    concurrency = 1,
    skip_if_running = true,
    labels = { singular = "Cleanup Expired Posts" },
    access = "hooks.check_admin",  -- optional access control
})

local M = {}
function M.run(ctx)
    -- ctx.data = input data from queue() or {} for cron
    -- ctx.job  = { id, slug, queue, attempt, max_attempts, priority,
    --             unique_key?, scheduled_by?, queued_at? }
    -- Full CRUD access available
    local expired = crap.collections.posts.find({
        where = { expires_at = { less_than = os.date("!%Y-%m-%dT%H:%M:%SZ") } }
    })
    for _, doc in ipairs(expired.documents) do
        crap.collections.posts.delete(doc.id)
    end
    return { deleted = #expired.documents }
end
return M
```

## Configuration Options

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `handler` | string | (required) | Lua function ref (e.g., `"jobs.cleanup.run"`) |
| `schedule` | string | nil | Cron expression for automatic scheduling |
| `queue` | string | `"default"` | Queue name for grouping |
| `retries` | integer | inherits queue, else 0 | Max retry attempts on failure (see [Retry Backoff](#retry-backoff) below). Omit to inherit `[jobs.queues.<queue>] retries` from `crap.toml`; set explicitly (including `0`) to override the queue default. |
| `timeout` | integer | 60 | Seconds before job is marked failed |
| `concurrency` | integer | 1 | Max concurrent runs of this job (cluster-wide) |
| `priority` | integer | 0 | Default scheduling priority; higher = sooner. Per-enqueue value overrides this. |
| `skip_if_running` | boolean | true | Skip cron trigger if previous run still active |
| `labels` | table | nil | Display labels (`{ singular = "..." }`) |
| `access` | string | nil | Lua function ref gating both trigger and run-reads. Receives `ctx.operation` (`"trigger"` or `"read"`) so one function can serve both, or branch to allow read-only viewers. Returns `true`/`false` only — a filter table is rejected. |

## Queuing from Hooks

Jobs can be queued programmatically from hooks:

```lua
-- Basic enqueue:
crap.jobs.queue("send_welcome_email", { user_id = ctx.data.id, email = ctx.data.email })

-- With per-enqueue options:
crap.jobs.queue("send_password_reset", { user_id = id }, {
    priority = 10,         -- jump the queue
    delay = "5m",          -- defer claim by 5 minutes
    unique = "reset:" .. id, -- dedup against active jobs with this key
})
```

`queue()` inserts a pending job and returns its id immediately. The scheduler
picks it up on its next poll cycle. When `unique` matches an existing
pending/running job, `queue()` returns that job's id instead of inserting a
duplicate. See [`crap.jobs.queue` reference](../lua-api/jobs.md#crapjobsqueueslug-data-opts)
for full opts.

The same three options exist on every trigger surface — `priority`, `delay`,
and `unique` on the gRPC [`TriggerJob`](../grpc-api/rpcs.md#triggerjob) RPC
and the MCP `trigger_job` tool — and all of them go through one queue
chokepoint, so the semantics cannot differ between surfaces. The
[operation-options reference](../reference/operation-options.md#job-operations)
lists the full cross-surface field matrix.

## Handler Context

The handler function receives a context table:

```lua
function M.run(ctx)
    ctx.data          -- table: input data from queue() or {} for cron
    ctx.job.id        -- string: this run's nanoid id
    ctx.job.slug      -- string: job definition slug
    ctx.job.queue     -- string: queue this run executes on
    ctx.job.attempt   -- integer: current attempt (1-based)
    ctx.job.max_attempts -- integer: total attempts allowed
    ctx.job.priority  -- integer: scheduling priority (higher = sooner)
    ctx.job.unique_key   -- string?: dedup key, if queued with { unique = ... }
    ctx.job.scheduled_by -- string?: "cron" | "hook" | "grpc" | "cli"
    ctx.job.queued_at -- string?: ISO-8601 time the run was queued
    ctx.options       -- table?: per-config options when the handler was
                      --   registered as { ref = "...", options = {...} }
end
```

The handler has full CRUD access (`crap.collections.find()`, `.create()`, etc.).
**Each CRUD op opens its own short-lived `BEGIN IMMEDIATE` transaction**
(pool-mode), so a `find` followed by an `update` are two separate atomic
writes. If you need multi-step atomicity (read-modify-write, multi-write
all-or-nothing), wrap the block in
[`crap.transaction(fn)`](../lua-api/jobs.md#craptransactionfn--explicit-multi-step-atomicity).

Handler writes behave like writes on every other surface: they **publish
[live-update events](../live-updates/overview.md)** (the `events` option
defaults to `true` — pass `{ events = false }` for a quiet write, e.g. bulk
seeding) and **invalidate the populate cache**. Events queued during the
handler are dispatched after the handler returns — i.e. after every per-op
transaction has committed — including for ops that completed before a later
error.

If the handler returns a table, it's stored as the job result (JSON).
If it errors, the job is marked failed (and retried if attempts remain).

## Back Pressure

Concurrency caps stack, strictest wins (all cluster-wide via the shared DB):

- **Global**: `[jobs] max_concurrent` in `crap.toml` (default: 10)
- **Per-queue**: `[jobs.queues.<name>]` for queue-level defaults —
  `concurrency = N` (aggregate cap), `timeout = "5m"` (per-job
  wall-clock timeout for system jobs in this queue), `retries = N`
  (default `max_attempts - 1` for system jobs in this queue). Partial
  overrides keep framework defaults intact. Framework ships
  `images = { concurrency = 2, timeout = "5m", retries = 2 }`.
- **Per-job**: `concurrency` field on the definition (default: 1)
- **Timeout**: Jobs running longer than `timeout` are marked failed
- **Skip-if-running**: Cron-triggered jobs skip if a previous run is still active

For aging-based promotion of low-priority jobs in busy queues, set
`[jobs] priority_decay = "1m"` — see
[Concurrency model](../lua-api/jobs.md#concurrency-model) for the
full picture.

## Error Handling

Job execution is fully isolated. If a job handler panics, the panic is caught and logged — it does not crash the server or affect other jobs. The job is marked as failed and retried if attempts remain.

### Retry Backoff

Failed jobs with remaining `retries` are re-queued with an **exponential backoff** before the next attempt:

```
delay = min(2^(attempt - 1) * 5, 300)  // seconds, capped at 5 minutes
```

| Attempt (1-based) | Delay before retry |
|-------------------|---------------------|
| 1 (first failure) | 5 s   |
| 2                 | 10 s  |
| 3                 | 20 s  |
| 4                 | 40 s  |
| 5                 | 80 s  |
| 6                 | 160 s |
| 7 or later        | 300 s (cap) |

This is fixed and not currently configurable per-job. Plan your `retries` budget knowing that attempt 6+ is at least ~5 minutes out, not a handful of seconds.

## Crash Recovery (at-least-once)

A running job updates a **heartbeat** timestamp every `heartbeat_interval`
seconds. If a worker dies mid-job, its heartbeat stops; once it is older than
`heartbeat_interval × 3` the job is considered dead and **recovered**:

- a job with retry attempts remaining is **re-queued** (any surviving node
  re-runs it), and
- a job that has exhausted its retries is marked terminal `stale`.

Recovery runs both at startup and periodically at runtime, so in a multi-node
cluster a **crashed node's in-flight jobs are reclaimed by the surviving
nodes** — you don't have to wait for the dead node to restart. A job whose
heartbeat is still fresh (a live node is working it) is never reclaimed.

This makes job delivery **at-least-once**: a job that times out or whose worker
crashes will run again, so **job handlers must be idempotent**. A job that must
never run twice needs its own guard (e.g. a unique key or an idempotency check
at the top of the handler).

## System jobs

The framework queues three job kinds of its own. They live outside
`crap.jobs.define` (Rust handlers, no Lua VM), but flow through the same
claim/execute/retry machinery and are configured through their queues:

| Slug | Queue | Purpose |
|---|---|---|
| `_system_email` | `email` | Outbound email delivery |
| `_system_image_convert` | `images` | AVIF / WebP image conversion |
| `_system_bulk` | `bulk` | Queued bulk ops ([`queue = true`](../grpc-api/rpcs.md#queued-mode-queue--true) on CreateMany / UpdateMany / DeleteMany) |

`_system_bulk` runs are visible only to the identity that queued them and
never appear in `ListJobRuns`; the `bulk` queue defaults to
`concurrency = 1`, `timeout = 3600`, `retries = 0`.

## Configuration (`crap.toml`)

```toml
[jobs]
max_concurrent = 10       # global concurrency limit
poll_interval = 1         # seconds between pending job polls
cron_interval = 60        # seconds between cron schedule checks
heartbeat_interval = 10   # seconds between heartbeat updates
auto_purge = "30d"        # auto-delete old job runs (default 30d); `false` disables
```

## CLI Commands

```bash
crap-cms -C <config_dir> jobs list                   # list defined jobs
crap-cms -C <config_dir> jobs trigger <slug>         # manually queue a job
crap-cms -C <config_dir> jobs status [--id <id>]     # show recent job runs
crap-cms -C <config_dir> jobs purge [--older-than 7d] # clean up old runs
crap-cms -C <config_dir> jobs cancel [-s <slug>]      # cancel pending runs
crap-cms -C <config_dir> jobs healthcheck             # health summary; exit 0/2/1
```

## gRPC API

Four RPCs for job management:

- `ListJobs` — list all defined jobs
- `TriggerJob(slug, data?)` — queue a job, returns the run ID
- `GetJobRun(id)` — get details of a specific run
- `ListJobRuns(slug?, status?, limit?, offset?)` — list job runs with filters

All require authentication. Every RPC also enforces the job's `access`
function if defined (see below): `TriggerJob` calls it with
`operation == "trigger"`; `GetJobRun`, `ListJobRuns`, and `ListJobs` call it
with `operation == "read"`. Run reads are a permissive union — `ListJobRuns`
and `ListJobs` silently omit jobs the caller may not read (never error), and
`GetJobRun` returns `not_found` for a denied run (hiding its existence). A job
with no `access` function is readable and triggerable by any authenticated
caller.
