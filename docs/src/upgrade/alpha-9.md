# Upgrading to alpha.9

This guide covers the upgrade path from `alpha.8` (or earlier) to
`alpha.9`. It focuses on operator action items first; opt-in features
follow.

## TL;DR

- **Replace your binary, restart.** DB schema migrations apply
  automatically (idempotent ALTER + CREATE INDEX IF NOT EXISTS).
- **Update `crap.toml` if you used `[jobs] image_concurrency`** —
  the field was removed; use `[jobs.queues.images] concurrency = N`
  instead. The framework auto-applies a default of `2` if you don't
  set it.
- **Update `crap.toml` if you set `[email] queue_*`** — the four
  `queue_retries` / `queue_name` / `queue_timeout` / `queue_concurrency`
  fields were removed; use `[jobs.queues.email] retries = N`,
  `timeout = "..."`, `concurrency = N` instead. Defaults are
  identical to alpha.8 (`retries = 3`, `timeout = "30s"`,
  `concurrency = 5`), so you only need to migrate if you customized
  any of them.
- **Review multi-step Lua job handlers.** Per-CRUD transactions are
  now opened independently (pool-mode). Wrap multi-write atomic
  sequences in `crap.transaction(fn)`.
- **Audit auth methods** if you use strategy authentication —
  `is_locked` / `verify_email` checks now apply uniformly across
  auth surfaces (closed a security gap).

## Required action items

### 1. Remove `[jobs] image_concurrency` from `crap.toml`

If present, config load fails with `unknown field "image_concurrency"`.

```diff
  [jobs]
  max_concurrent = 10
- image_concurrency = 4
+
+ [jobs.queues.images]
+ concurrency = 4
```

If you were happy with the previous default of `2`, you can omit the
`[jobs.queues.images]` block entirely — the framework's
`apply_queue_defaults` auto-applies `concurrency = 2` to the `images`
queue when no operator config is present. The default is documented
in `[jobs.queues]`'s "Framework-supplied defaults" section.

### 1b. Remove `[email] queue_*` from `crap.toml`

If present, config load fails with `unknown field "queue_retries"`
(or `queue_name` / `queue_timeout` / `queue_concurrency`).
`_system_email` now reads the same `[jobs.queues.<name>]` table as
`_system_image_convert` — one config mechanism for every system job.

```diff
  [email]
  smtp_host = "smtp.example.com"
- queue_retries = 5
- queue_timeout = 60
- queue_concurrency = 8
- queue_name = "email"
+
+ [jobs.queues.email]
+ retries = 5
+ timeout = "1m"
+ concurrency = 8
```

If you were on alpha.8 defaults (`queue_retries = 3`,
`queue_timeout = 30`, `queue_concurrency = 5`), you can omit the
`[jobs.queues.email]` block entirely — the framework auto-applies
those exact values via `apply_queue_defaults`. Per-call overrides
still work: `crap.email.queue{ retries = N }` wins over the queue
default.

**`queue_name` is dropped without replacement.** `_system_email`
now always uses the queue named `"email"`. If you previously set
`[email] queue_name = "<other>"` to route email jobs into a custom
queue, rename your `[jobs.queues.<other>]` block to
`[jobs.queues.email]`.

### 2. Review multi-step Lua job handlers

Earlier alpha.9 builds wrapped every Lua job handler in a single
outer `BEGIN DEFERRED` transaction. That model hit
`SQLITE_BUSY_SNAPSHOT` errors when a handler's read snapshot
collided with concurrent writers (admin edits, image queue, etc.).

The fix is **pool-mode**: each Lua CRUD call inside a job handler
opens its own short-lived `BEGIN IMMEDIATE` transaction. No more
snapshot conflicts. But this means multi-step writes in a single
handler are *no longer atomic with each other* by default.

If your handler depends on cross-CRUD atomicity:

```diff
  function M.run(ctx)
+     crap.transaction(function()
          crap.collections.posts.update(id_a, { count = 1 })
          crap.collections.posts.update(id_b, { count = 2 })
+     end)
  end
```

Most handlers don't need this — they do a single CRUD op or write
to a single document. Inspect handlers that do 2+ writes whose
mutual rollback you depended on.

See [Transactions in job handlers](../lua-api/jobs.md#transactions-in-job-handlers)
for the full pattern.

### 3. (Optional) Review hooks that used implicit cross-step atomicity

The pool-mode shift applies only to **job handlers**, not to hooks.
Hook code runs inside the parent operation's write transaction (e.g.
`after_change` shares the document-update tx). No changes needed
for hooks.

If you're not sure whether your code is a hook or a job handler —
hooks are registered via `crap.hooks.register(...)` or per-collection
`{ hooks = { before_change = ... } }`. Jobs are defined via
`crap.jobs.define(...)`.

### 4. Security review (auth-related)

These are bug fixes for security gaps that existed in earlier
alphas. Review:

- **Strategy-authenticated users now pass `is_locked` /
  `verify_email`** the same way as bearer / cookie users. If you
  relied on a strategy hook bypassing these checks, that hole is
  closed.
- **Logout invalidates all JWTs for the user**, not just the cookie.
  Active sessions across surfaces die on next request, not at JWT
  expiry.
- **`Resolution::Invalid(Unaccepted)`** for cookies/tokens that
  decode cleanly but whose `session_cookie` method has been removed
  from the collection's `methods`. Browser cookies are cleared on
  redirect to login instead of looping.

See the `### Security` section of the alpha.9 entry in
`CHANGELOG.md` at the project root for the full context.

## Automatic migrations

The following happen on first startup with the new binary. No manual
SQL required.

### `_crap_jobs` schema additions

| Column | Purpose | Migration |
|---|---|---|
| `priority INTEGER NOT NULL DEFAULT 0` | Scheduling priority | `ALTER TABLE ADD COLUMN` |
| `unique_key TEXT` | Dedup key for `unique` option | `ALTER TABLE ADD COLUMN` |

Plus new indexes: `idx_crap_jobs_priority` (priority claim ordering)
and `idx_crap_jobs_unique_active` (partial unique index for dedup).
Both created `IF NOT EXISTS`.

### `_crap_image_queue` table dropped

The legacy `_crap_image_queue` table is drained into `_crap_jobs` as
`_system_image_convert` jobs, then `DROP`ped. Idempotent — safe on
re-runs and on installs that never had the table.

**Operator action if you upgraded a pre-fix alpha.9 build** (i.e.
you saw `Job definition '_system_image_convert' not found, marking
as failed` in the logs before this build): the drained jobs were
incorrectly marked failed. Reset them with:

```sh
crap-cms images retry --all -y
```

New uploads on the current build process normally.

## Opt-in features (alpha.9)

### Job priority

```lua
-- At define time (default for all enqueues of this slug):
crap.jobs.define("analytics_rollup", {
    handler = "jobs.analytics.rollup",
    schedule = "0 3 * * *",
    priority = -5,         -- run only when queue is otherwise idle
})

-- Per-enqueue override:
crap.jobs.queue("send_password_reset", { user_id = id }, { priority = 10 })
```

CLI: `crap-cms jobs trigger <slug> --priority N`.
gRPC: `TriggerJobRequest.priority` (optional `int32`).

### Job delay

Defer a job's earliest claim time:

```lua
crap.jobs.queue("send_welcome_email", { user_id = id }, { delay = "5m" })
-- or integer seconds:
crap.jobs.queue("send_welcome_email", { user_id = id }, { delay = 300 })
```

Reuses the existing `retry_after` column — no new schema. Combines
with priority and unique.

### Unique jobs (dedup)

Don't queue a duplicate if one is already pending or running:

```lua
local job_id = crap.jobs.queue(
    "reindex_collection",
    { slug = "posts" },
    { unique = "reindex:posts" }
)
```

If another `reindex_collection` job with `unique = "reindex:posts"`
is already pending or running, `queue()` returns that job's id —
not a new one. Completed and failed jobs don't block re-enqueue.

### Per-queue concurrency, timeout, and retry defaults

Throttle aggregate work and supply timeout / retry defaults for jobs
in a queue independent of per-slug caps:

```toml
[jobs.queues]
emails  = { concurrency = 4, timeout = "1m" }
images  = { concurrency = 2 }                  # framework defaults: timeout = 5m, retries = 2
reports = { concurrency = 1, timeout = "30m", retries = 0 }
```

| Field | Meaning |
|---|---|
| `concurrency` | Max concurrent runs in this queue. `0` = unlimited. |
| `timeout` | Per-job wall-clock timeout for system jobs (`_system_image_convert`, `_system_email`) that lack their own `JobDefinition`. Accepts seconds or human-readable. |
| `retries` | Default `max_attempts` for jobs in this queue, expressed as **retries** (total attempts = `retries + 1`). Used by system jobs AND by user Lua jobs that omit `retries` in `crap.jobs.define`. Explicit `JobDefinition.retries` (including `retries = 0`) overrides the queue default. `crap.email.queue{ retries = N }` overrides for that one call. |

Partial overrides merge field-by-field — supplying only `concurrency`
leaves the framework's `timeout` / `retries` defaults intact. The
framework ships sensible `images` defaults (`concurrency = 2`,
`timeout = "5m"`, `retries = 2`); operator config wins on a per-field
basis.

Concurrency caps are **cluster-wide** (counted from the shared DB),
composing with global `max_concurrent` and per-slug
`JobDefinition::concurrency` (strictest wins). See [Concurrency model]
in `jobs.md`.

> **Earlier alpha.9 builds hardcoded the image-convert timeout at
> 120s.** Large originals on slow storage hit that ceiling. Upgrade
> and adjust `[jobs.queues.images] timeout` if your workload needs
> more (the new framework default is `5m`).

**Reach for `retries`.** Resolution at queue time, strictest-explicit
wins:

1. `JobDefinition.retries` set explicitly in `crap.jobs.define` (any
   value, including `0`).
2. `[jobs.queues.<queue>] retries` from `crap.toml` when (1) is
   omitted.
3. `0` (one attempt) when neither is set.

So an operator who writes `[jobs.queues.reports] retries = 5` and a
user who registers `crap.jobs.define("rollup", { queue = "reports" })`
without retries gets 6 total attempts per rollup — symmetric with how
system jobs inherit from `[jobs.queues.email]` /
`[jobs.queues.images]`.

`crap.email.queue{ retries = N }` is the only per-call retry override
on the Lua surface; `crap.jobs.queue` for user jobs accepts
`priority` / `delay` / `unique` (no `retries`).

**Reach for `timeout`.** Applies to system jobs only —
`_system_image_convert` reads `[jobs.queues.images] timeout`,
`_system_email` reads `[jobs.queues.email] timeout`. User Lua jobs
use the `timeout` declared on their `JobDefinition` (`60` if
omitted).

### Priority decay (aging-based promotion)

Prevent backlog starvation of low-priority jobs:

```toml
[jobs]
priority_decay = "1m"  # +1 effective priority per minute waiting
```

After enough wait time, an old low-priority job overtakes fresh
higher-priority jobs. `0` (default) disables decay — pure static
priority + FIFO.

### `crap.transaction(fn)` for explicit atomicity

Already covered in **Required action items**. Use this when a job
handler needs multi-step atomicity in pool-mode.

## Notable changes (non-breaking)

- **gRPC**: `TriggerJobRequest.priority` (optional `int32`),
  `GetJobRunResponse.priority` (`int32`),
  `GetJobRunResponse.unique_key` (optional `string`).
- **CLI**:
  - `crap-cms jobs trigger <slug> --priority N`
  - `crap-cms jobs status` — new `Prio` column
  - `crap-cms images list` — new `Prio` column
  - `crap-cms images retry --priority N`
- **Image conversion** runs in the unified `_crap_jobs` queue as
  `_system_image_convert` system jobs (operator-visible via
  `crap-cms jobs status --slug _system_image_convert` if needed,
  though `crap-cms images list / stats / retry / purge` still work
  unchanged).
- **Postgres + SQLite job-claim paths unified.** Same observable
  behavior across backends; strict global priority ordering;
  identical per-slug and per-queue cap enforcement.

## Reference

- `CHANGELOG.md` at the project root — the full alpha.9 entry with
  every change, including non-job-related items not covered here.
- [Jobs documentation](../lua-api/jobs.md)
- [crap.toml reference](../configuration/crap-toml.md)

[Concurrency model]: ../lua-api/jobs.md#concurrency-model
