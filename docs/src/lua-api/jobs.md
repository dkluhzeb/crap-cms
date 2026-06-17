# crap.jobs

Background job definition and queuing.

## crap.jobs.define(slug, config)

Define a background job. **Init-only:** call this from `jobs/*.lua`,
`init.lua`, or any file loaded by `require` from those. The scheduler
enrolls jobs once at startup; runtime registration would never reach
the cron or queue worker, so it's rejected outright:

> `crap.jobs.define must be called from a definition file or
> init.lua. To change a registered job, edit the file and restart
> the process.`

The common "define + handler in one file" layout (top-level
`crap.jobs.define(...)` plus `function M.run(ctx) ... end` returned
as a module) works fine: `init` loads each `jobs/foo.lua` once and
caches its return value in `package.loaded["jobs.foo"]`. When the
dispatcher later does `require("jobs.foo")` to call the handler, it
hits the cache and the top-level `define` does **not** re-run.

**Parameters:**
- `slug` (string) — Unique job identifier
- `config` (table) — Job configuration:
  - `handler` (string, required) — Lua function ref (e.g., `"jobs.cleanup.run"`)
  - `schedule` (string, optional) — Cron expression (e.g., `"0 3 * * *"`)
  - `queue` (string, default: `"default"`) — Queue name
  - `retries` (integer, optional) — Max retry attempts. When omitted, inherits `[jobs.queues.<queue>] retries` from `crap.toml`; if the queue has no entry either, defaults to `0` (one attempt). Set explicitly (including `retries = 0`) to override the queue default.
  - `timeout` (integer, default: 60) — Seconds before timeout
  - `concurrency` (integer, default: 1) — Max concurrent runs
  - `skip_if_running` (boolean, default: true) — Skip cron if still running
  - `labels` (table, optional) — `{ singular = "Display Name" }`
  - `access` (string, optional) — Lua function ref gating both trigger and run-reads. Receives `ctx.operation` (`"trigger"` or `"read"`); returns `true`/`false`.

**Example:**

```lua
crap.jobs.define("send_digest", {
    handler = "jobs.digest.run",
    schedule = "0 8 * * 1",  -- Mondays at 8am
    retries = 2,
    timeout = 120,
})
```

## crap.jobs.queue(slug, data?, opts?)

Queue a job for background execution. Only available inside hooks with transaction context.

**Parameters:**
- `slug` (string) — Job slug (must be previously defined)
- `data` (table, optional) — Input data passed to the handler (default: `{}`)
- `opts` (table, optional) — Per-enqueue options:
  - `priority` (integer) — Static scheduling priority. Higher values are
    claimed sooner; negative values run only when the queue is otherwise
    idle. Same-priority jobs claim FIFO. When omitted, falls back to the
    job definition's `priority` (or `0`).
  - `delay` (integer seconds **or** duration string `"5m"` / `"30s"` /
    `"1h"` / `"7d"`) — How long to wait before the job becomes
    claimable. Default `0` = immediate. Internally sets `retry_after =
    now() + delay`; works the same way as exponential-backoff retries.
  - `unique` (string) — Dedup key. If another job with the same slug
    is already pending or running with this `unique` value, returns
    that job's id instead of inserting a duplicate. Completed/failed
    jobs don't block re-enqueue (only pending/running are "active").

  The options table is strict: `priority`, `delay`, and `unique` are the
  only accepted keys. An unknown key (e.g. a typo'd `priorty`) is a hard
  error, not silently ignored.

**Returns:** `string` — The queued job run ID (either freshly inserted
or, when `unique` matched, the existing one).

**Example:**

```lua
-- In an after_change hook
local job_id = crap.jobs.queue("send_welcome_email", {
    user_id = ctx.data.id,
    email = ctx.data.email,
})
crap.log.info("Queued welcome email job: " .. job_id)

-- High-priority job (e.g. password reset email) jumps the queue.
crap.jobs.queue("send_password_reset", { user_id = id }, { priority = 10 })

-- Low-priority background work (e.g. analytics rollup) runs only
-- when the queue is otherwise idle. Note: prefer setting this on
-- the definition with `priority = -5` so every enqueue site inherits it.
crap.jobs.queue("analytics_rollup", {}, { priority = -5 })

-- Delayed: send the welcome email 5 minutes after signup so a
-- newly-created user has time to land on the dashboard first.
crap.jobs.queue("send_welcome_email", { user_id = id }, { delay = "5m" })

-- Idempotent: only one "rebuild search index for collection X" job
-- pending at a time. If two writes fire in quick succession, the
-- second `queue()` returns the first job's id — no duplicate work.
crap.jobs.queue("reindex_collection", { slug = "posts" }, {
    unique = "reindex:posts",
})
```

**Defining a job with a default priority:**

```lua
crap.jobs.define("analytics_rollup", {
    handler = "jobs.analytics.rollup",
    schedule = "0 3 * * *",     -- nightly
    priority = -5,              -- yield to user-triggered work
    timeout = 600,
})

-- Now every cron-fired run AND every manual `crap.jobs.queue("analytics_rollup")`
-- inherits priority = -5 unless the queue site explicitly overrides.
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
    local result = crap.collections.posts.find({
        where = { status = "expired" }
    })

    -- Return value is stored as the job result (optional)
    return { processed = result.pagination.totalDocs }
end
return M
```

## Transactions in job handlers

Each CRUD call inside a job handler runs in its own short-lived
`BEGIN IMMEDIATE` transaction (**pool-mode**). Different from hooks,
which share the parent operation's write transaction.

| Context | Transaction model |
|---|---|
| Hook (`before_change`, `after_change`, …) | All CRUD ops share the parent's write tx. Atomic with the document being created/updated. |
| Job handler (default) | Each CRUD op is its own short-lived `BEGIN IMMEDIATE` tx (pool-mode). `find` then `update` are two distinct atomic writes. |
| Inside `crap.transaction(fn)` (job-only) | All CRUD ops in `fn` share one `BEGIN IMMEDIATE` tx. Used for multi-step atomicity in jobs. |

### Why pool-mode for jobs

Earlier releases wrapped each job handler in a single outer
`BEGIN DEFERRED` transaction shared across all CRUD calls. Long-running
handlers that did `find` followed by `update` (canonical pattern) could
hit `SQLITE_BUSY_SNAPSHOT` if any other writer (admin edits, image
queue, other jobs) committed between the read and the write — SQLite's
deadlock-prevention error, **not retried** by `busy_timeout`. The
pool-mode model avoids this entirely: each op opens its own
`IMMEDIATE` tx, so there's no snapshot window for a writer to invalidate.

### `crap.transaction(fn)` — explicit multi-step atomicity

When a job needs multiple CRUD ops to be atomic (e.g., read a counter,
write it back incremented), wrap them in `crap.transaction(fn)`:

```lua
local M = {}
function M.run(ctx)
    -- Atomic: both ops share one IMMEDIATE tx.
    crap.transaction(function()
        local doc = crap.collections.posts.find_by_id(ctx.data.id)
        crap.collections.posts.update(ctx.data.id, {
            view_count = doc.view_count + 1,
        })
    end)
end
return M
```

- Errors raised inside `fn` roll back the entire block.
- The return value of `fn` is the return value of `crap.transaction`.
- Inside a hook (which already has a shared tx), `crap.transaction(fn)`
  is a pass-through — useful so the same code works in both contexts.
- Calling `crap.transaction` from outside a job/hook (e.g. `init.lua`)
  errors with a clear message.
- Nesting `crap.transaction(crap.transaction(...))` is currently a
  pass-through on the inner call (no `SAVEPOINT` support yet).

### Migration from earlier releases

Existing job handlers that depended on implicit cross-CRUD atomicity
(e.g., a job that does two updates and expects both to roll back on
error) need to wrap those updates in `crap.transaction(fn)`:

```lua
-- Before (was implicitly atomic, but SQLITE_BUSY_SNAPSHOT-prone):
function M.run(ctx)
    crap.collections.posts.update(id_a, { count = 1 })
    crap.collections.posts.update(id_b, { count = 2 })
    -- If a writer commits between the two updates, the second would
    -- fail. Now both succeed independently (each is its own tx),
    -- but they're no longer atomic with each other.
end

-- After (explicit atomicity):
function M.run(ctx)
    crap.transaction(function()
        crap.collections.posts.update(id_a, { count = 1 })
        crap.collections.posts.update(id_b, { count = 2 })
    end)
end
```

### Handling rollbacks with `pcall`

`crap.transaction` follows Lua's normal exception model: any error
inside the block triggers a rollback and the error propagates to the
caller. There is no `on_rollback` callback — use Lua's built-in
`pcall` to react to rollbacks:

```lua
function M.run(ctx)
    local ok, err = pcall(crap.transaction, function()
        crap.collections.posts.update(ctx.data.id, {
            balance = doc.balance - amount,
        })
        crap.collections.audit_log.create({
            kind = "transfer",
            amount = amount,
        })
        -- Validate post-conditions; raising here rolls back BOTH writes.
        if doc.balance - amount < 0 then
            error("balance would go negative")
        end
    end)

    if not ok then
        crap.log.error("transfer failed, rolled back: " .. tostring(err))
        -- Re-enqueue, emit a metric, schedule a compensation job, …
        crap.jobs.queue("send_failure_email", {
            user_id = ctx.data.user_id,
            reason = tostring(err),
        }, { priority = 5 })
    end
end
```

Common patterns:

| Goal | Idiom |
|---|---|
| Re-enqueue the work on failure | `if not ok then crap.jobs.queue(ctx.slug, ctx.data) end` |
| Emit a metric on every rollback | `if not ok then crap.metrics.inc("tx_rollback") end` |
| Compensate external side-effects | Run the side-effect inside the `if not ok` branch (after rollback) |
| Stop the job from being marked failed | Catch with `pcall` and return success from `M.run`; the framework will treat it as completed |

## Concurrency model

Three caps stack when the scheduler decides whether to claim a job.
Strictest applicable cap wins.

| Cap | Scope | Configured in | Counts |
|---|---|---|---|
| `[jobs] max_concurrent` | **Per-server** | `crap.toml` | Total jobs in flight on this scheduler process |
| `[jobs.queues.<name>] concurrency` | **Cluster-global** | `crap.toml` | Total jobs in queue `<name>` across the whole DB |
| `JobDefinition::concurrency` | **Cluster-global** | `crap.jobs.define({ concurrency = N })` | Total jobs of this specific slug across the whole DB |

Example: queue `emails` cap=4, slug `send_welcome` concurrency=2 →
- At most 4 jobs running in queue `emails` total across the cluster.
- Of those, at most 2 can be `send_welcome` specifically.
- The remaining 2 slots can be filled by any other slug routed to `emails`.

Queues without an entry in `[jobs.queues]` are unconstrained beyond
the global `max_concurrent`. The scheduler logs a warning at startup
if `[jobs.queues]` references a queue name that no defined job uses
(typo catcher).

`[jobs.queues.<name>]` also carries two non-concurrency knobs that
apply to **system jobs** (`_system_image_convert`, `_system_email`)
which lack their own `JobDefinition`:

| Field | What it sets |
|---|---|
| `timeout` | Per-job wall-clock timeout for system jobs in this queue. User Lua jobs use the timeout on their `JobDefinition` instead. |
| `retries` | Default `max_attempts` for jobs in this queue (`max_attempts = retries + 1`). Used by system jobs AND by user Lua jobs that omit `retries` in `crap.jobs.define`. Explicit `JobDefinition.retries` (including `retries = 0`) overrides the queue default. `crap.email.queue{ retries = N }` overrides for that one call. |

See [`[jobs.queues]`](../configuration/crap-toml.md#jobsqueues) in
the config reference for the full field table, defaults, and merging
semantics.

## Multi-server semantics

The CMS supports running multiple scheduler processes against the
same Postgres database (each binary runs its own scheduler tick
loop). SQLite is single-process by file-locking, so this section
applies to Postgres deployments.

**Every concurrency cap is cluster-wide.** Each server's poll tick
reads `COUNT(*) FROM _crap_jobs WHERE status = 'running'` (filtered
by slug or queue as needed) from the shared DB, so all servers see
the same running totals and apply the same caps.

| Cap / knob | Mechanism |
|---|---|
| `[jobs] max_concurrent` | `SELECT COUNT(*) ... WHERE status='running'` (no slug filter). |
| `[jobs.queues.<name>] concurrency` | Per-queue `COUNT(*)` via `count_running_per_queue`. |
| `JobDefinition::concurrency` | Per-slug `COUNT(*)` via `count_running_per_slug`. |
| `[jobs] priority_decay` | Pure SQL ORDER BY — every server orders the same way. |

**What this means in practice:**

- Setting `max_concurrent = 10` on every server caps total in-flight
  jobs at 10 across the entire cluster, not 10 × N. Adding more
  servers does not multiply throughput; it adds load distribution
  and fault tolerance, but the cluster-wide cap holds.
- If servers have *different* `max_concurrent` values (e.g.
  staggered rollout), the effective cluster cap converges to the
  *highest* value across servers. Once cluster running reaches that
  number, even the strictest server sees the cap as reached.
- Per-queue and per-slug caps compose cleanly across servers: an
  `emails` queue with `concurrency = 4` will have at most 4 email
  jobs running cluster-wide, regardless of server count.

**Operator guidance:**

- Set `[jobs] max_concurrent` based on the throughput your cluster
  should sustain (not per-server). Tune up if backlogs grow.
- Use `[jobs.queues.<name>] concurrency` for resource-shared work
  (SMTP pool, image encoder pool, …) — these caps are global.
- Use `JobDefinition::concurrency` (`crap.jobs.define({ concurrency
  = N })`) for "at most N of this specific job type at a time" —
  also global.
- `priority_decay`'s ordering is stable across servers as long as
  their clocks are within a few seconds. Small clock drift may
  cause a different server to win a particular tick's race, but the
  *set* of claimed jobs is the same.

**Cap precision under concurrent ticks.** Per-slug caps are enforced
inside the claim transaction (the running count is part of the
locked SQL subquery), so they're exact. Global `max_concurrent` and
per-queue caps are checked *before* the claim transaction starts, so
two servers ticking within the same millisecond can each claim a
small batch and briefly push the cluster total over the cap. The
overshoot is bounded by the number of concurrent ticks × the batch
size and converges back as soon as one tick completes — Sidekiq, Oban,
and similar systems behave the same way. Treat these caps as soft.
If you need precise enforcement (e.g. licensing limits), use a
per-slug cap.

The shared coordination point is the database — `FOR UPDATE SKIP
LOCKED` (Postgres) or `BEGIN IMMEDIATE` (SQLite) ensures no two
servers claim the same job row.
