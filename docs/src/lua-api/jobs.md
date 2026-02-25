# crap.jobs

Background job definition and queuing.

## crap.jobs.define(slug, config)

Define a background job. Call in `init.lua` or `jobs/*.lua` files.

**Parameters:**
- `slug` (string) — Unique job identifier
- `config` (table) — Job configuration:
  - `handler` (string, required) — Lua function ref (e.g., `"jobs.cleanup.run"`)
  - `schedule` (string, optional) — Cron expression (e.g., `"0 3 * * *"`)
  - `queue` (string, default: `"default"`) — Queue name
  - `retries` (integer, default: 0) — Max retry attempts
  - `timeout` (integer, default: 60) — Seconds before timeout
  - `concurrency` (integer, default: 1) — Max concurrent runs
  - `skip_if_running` (boolean, default: true) — Skip cron if still running
  - `labels` (table, optional) — `{ singular = "Display Name" }`
  - `access` (string, optional) — Lua function ref for trigger access control

**Example:**

```lua
crap.jobs.define("send_digest", {
    handler = "jobs.digest.run",
    schedule = "0 8 * * 1",  -- Mondays at 8am
    retries = 2,
    timeout = 120,
})
```

## crap.jobs.queue(slug, data?)

Queue a job for background execution. Only available inside hooks with transaction context.

**Parameters:**
- `slug` (string) — Job slug (must be previously defined)
- `data` (table, optional) — Input data passed to the handler (default: `{}`)

**Returns:** `string` — The queued job run ID.

**Example:**

```lua
-- In an after_change hook
local job_id = crap.jobs.queue("send_welcome_email", {
    user_id = ctx.data.id,
    email = ctx.data.email,
})
crap.log.info("Queued welcome email job: " .. job_id)
```

## Handler Function

The handler function receives a context table and has full CRUD access:

```lua
local M = {}
function M.run(ctx)
    -- ctx.data: input data from queue() or {} for cron
    -- ctx.job.slug: job definition slug
    -- ctx.job.attempt: current attempt (1-based)
    -- ctx.job.max_attempts: total attempts allowed

    -- Full CRUD access:
    local result = crap.collections.find("posts", {
        filters = { status = "expired" }
    })

    -- Return value is stored as the job result (optional)
    return { processed = result.total }
end
return M
```
