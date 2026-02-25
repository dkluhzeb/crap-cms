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
    -- ctx.job = { slug, attempt, max_attempts }
    -- Full CRUD access available
    local expired = crap.collections.find("posts", {
        filters = { expires_at = { less_than = os.date("!%Y-%m-%dT%H:%M:%SZ") } }
    })
    for _, doc in ipairs(expired.documents) do
        crap.collections.delete("posts", doc.id)
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
| `retries` | integer | 0 | Max retry attempts on failure |
| `timeout` | integer | 60 | Seconds before job is marked failed |
| `concurrency` | integer | 1 | Max concurrent runs of this job |
| `skip_if_running` | boolean | true | Skip cron trigger if previous run still active |
| `labels` | table | nil | Display labels (`{ singular = "..." }`) |
| `access` | string | nil | Lua function ref for trigger access control |

## Queuing from Hooks

Jobs can be queued programmatically from hooks:

```lua
-- In a hook
crap.jobs.queue("send_welcome_email", { user_id = ctx.data.id, email = ctx.data.email })
```

`queue()` inserts a pending job and returns immediately. The scheduler picks it up
on its next poll cycle.

## Handler Context

The handler function receives a context table:

```lua
function M.run(ctx)
    ctx.data          -- table: input data from queue() or {} for cron
    ctx.job.slug      -- string: job definition slug
    ctx.job.attempt   -- integer: current attempt (1-based)
    ctx.job.max_attempts -- integer: total attempts allowed
end
```

The handler has full CRUD access (`crap.collections.find()`, `.create()`, etc.) running
inside its own database transaction. If the handler returns a table, it's stored as the
job result (JSON). If it errors, the job is marked failed (and retried if attempts remain).

## Back Pressure

- **Global concurrency**: `[jobs] max_concurrent` in `crap.toml` (default: 10)
- **Per-job concurrency**: `concurrency` field on the definition
- **Timeout**: Jobs running longer than `timeout` are marked failed
- **Skip-if-running**: Cron-triggered jobs skip if a previous run is still active

## Crash Recovery

On startup, the scheduler marks any previously-running jobs as stale (the server was
restarted while they were executing). Jobs with remaining retry attempts are re-queued.

Running jobs update a heartbeat timestamp periodically so stale detection works even
during normal operation.

## Configuration (`crap.toml`)

```toml
[jobs]
max_concurrent = 10       # global concurrency limit
poll_interval = 1         # seconds between pending job polls
cron_interval = 60        # seconds between cron schedule checks
heartbeat_interval = 10   # seconds between heartbeat updates
auto_purge = "7d"         # auto-delete completed jobs older than this
```

## CLI Commands

```bash
crap-cms jobs list <config_dir>                   # list defined jobs
crap-cms jobs trigger <config_dir> <slug>         # manually queue a job
crap-cms jobs status <config_dir> [--id <id>]     # show recent job runs
crap-cms jobs purge <config_dir> [--older-than 7d] # clean up old runs
```

## gRPC API

Four RPCs for job management:

- `ListJobs` — list all defined jobs
- `TriggerJob(slug, data_json?)` — queue a job, returns the run ID
- `GetJobRun(id)` — get details of a specific run
- `ListJobRuns(slug?, status?, limit?, offset?)` — list job runs with filters

All require authentication. `TriggerJob` also checks the job's `access` function if defined.
