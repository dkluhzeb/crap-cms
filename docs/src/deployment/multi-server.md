# Multi-Server Deployment

For high availability and dedicated job processing. Requires a shared database and shared file storage.

> **Most projects don't need this.** A single server handles thousands of concurrent users. See [Single Server](single-server.md) for the recommended default.

## Architecture

```
┌─────────────┐     ┌─────────────┐
│  App Server  │     │  App Server  │
│  serve       │     │  serve       │
│  --no-sched  │     │  --no-sched  │
└──────┬───────┘     └──────┬───────┘
       │                    │
       ▼                    ▼
┌──────────────────────────────────┐
│          Load Balancer           │
└──────────────────────────────────┘
       │                    │
       ▼                    ▼
┌─────────────┐     ┌─────────────┐
│  PostgreSQL  │     │  S3 / MinIO │
│  (shared DB) │     │ (shared FS) │
└─────────────┘     └─────────────┘
       ▲
       │
┌──────┴───────┐
│   Worker     │
│   work       │
└──────────────┘
```

## Requirements

1. **Shared database** — PostgreSQL (build with `--features postgres`)
2. **Shared file storage** — **S3-compatible object storage only** (build with `--features s3-storage`). Works with AWS S3, MinIO, Cloudflare R2, Backblaze B2, or any other S3-compatible backend. Shared filesystems (NFS, EFS, SMB) are **NOT supported** for multi-node — `storage = "local"` assumes a single writer and is not tested against the fsync / advisory-lock semantics of networked filesystems. Use object storage.
3. **Shared rate limits** (**required** for security) — Redis (build with `--features redis`). Without it, per-IP login rate limits are per-node counters: an attacker that round-robins across nodes multiplies their allowance by the node count (e.g. a 5-attempt limit across 3 nodes becomes 15 attempts before any node throttles). This is a throttling bypass, not a performance tweak.
4. **Shared cache** (recommended) — Redis for cross-server cache invalidation
5. **Shared live-update transport** (required if you rely on SSE or gRPC Subscribe across more than one node) — Redis pub/sub so mutation events and user-invalidation signals fan out to every server node. See [Live Updates](#5-live-updates-sse--grpc-subscribe) below.

## Setup

### 1. Database

```toml
# crap.toml (all servers share the same config)
[database]
backend = "postgres"
url = "host=db.example.com user=crap dbname=crap_cms"
```

### 2. File Storage

S3-compatible object storage is the only supported multi-node option. `storage = "local"` is fine for single-node; across multiple nodes it breaks because each node writes to its own local disk. Do **not** substitute a shared filesystem (NFS, EFS, SMB) — the code isn't tested against their fsync / locking semantics and silent data loss is the likely failure mode.

```toml
[upload]
storage = "s3"

[upload.s3]
bucket = "my-uploads"
endpoint = "https://s3.amazonaws.com"
access_key = "${AWS_ACCESS_KEY}"
secret_key = "${AWS_SECRET_KEY}"
```

Any S3-compatible backend works — AWS S3, MinIO, Cloudflare R2, Backblaze B2, etc. Point `endpoint` at the provider's URL.

### 3. Cache

Without a shared cache, each server maintains its own in-memory populate cache. Writes on one server won't invalidate another's cache. Use Redis for shared cache invalidation:

```toml
[cache]
backend = "redis"
redis_url = "redis://redis.example.com:6379"
prefix = "crap:"
```

Alternatively, use `max_age_secs` with the memory backend to limit staleness:

```toml
[cache]
backend = "memory"
max_age_secs = 10
```

### 4. Rate Limits (required)

Without shared rate limits, each server tracks login attempts independently — an attacker who round-robins requests across nodes effectively multiplies their rate-limit budget by the node count. A 5-attempt-per-IP limit spread across 3 nodes means 15 attempts before any single node throttles. This is a **security requirement**, not a performance optimization: use Redis to enforce a global per-IP counter.

```toml
[auth]
rate_limit_backend = "redis"
# rate_limit_redis_url defaults to cache.redis_url if empty
```

### 5. Live Updates (SSE / gRPC Subscribe)

Two transports are available:

- **`transport = "memory"`** (default) — events are broadcast on an in-process `tokio::sync::broadcast` channel. A write on node A reaches only subscribers connected to node A. Acceptable for sticky-load-balanced setups where every subscriber is pinned to the node that serves every write they care about; insufficient for round-robin balancing or for a pure reader node watching writes from elsewhere.
- **`transport = "redis"`** — mutation events and user-invalidation signals (`PermissionDenied` on lock / hard-delete) are published to Redis pub/sub and fan out to every node. Any subscriber on any node sees every event, regardless of which node published it. This is the correct choice as soon as you have more than one node.

```toml
[live]
transport = "redis"
# Reuses [cache] redis_url — no duplicate configuration
```

Both transports use the same Redis URL configured under `[cache] redis_url` (single source of truth — no separate `[live] redis_url` key). `transport = "redis"` requires `--features redis` at build time; if the feature is missing, startup aborts with an explicit error.

Events are JSON-encoded and published to the `crap:events` / `crap:invalidations` channels. The same send-timeout / lagged-subscriber drop semantics apply as for the in-process transport — a Redis reader that can't keep up is force-dropped with `RecvError::Lagged`.

Sticky load balancing is still recommended for SSE and gRPC Subscribe streams even with `transport = "redis"`: reconnects to a different node lose the in-flight subscription context (sequence position, filter state) and the client has to re-subscribe. See the [Load Balancer Stickiness](#load-balancer-stickiness) section below.

See also [Live Updates Overview](../live-updates/overview.md) for the transport-selection rationale and caveats.

### 6. App Servers

Run without the scheduler — job processing is handled by dedicated workers.

```bash
crap-cms serve --no-scheduler
```

### 7. Workers

One or more dedicated job workers process queues.

```bash
# General worker (all queues)
crap-cms work --detach

# Specialized workers
crap-cms work --detach --queues email
crap-cms work --detach --queues heavy --concurrency 2
```

Workers support the same lifecycle management as the server:

```bash
crap-cms work --status
crap-cms work --stop
crap-cms work --restart
```

## Job Queue Safety

The job queue is multi-server safe:

- **Cron dedup** — Cron jobs are deduplicated via the `_crap_cron_fired` table. Only one server fires each cron job per schedule window, regardless of how many instances run the scheduler.
- **Atomic claiming (Postgres)** — Jobs are claimed using `FOR UPDATE SKIP LOCKED`. Workers never claim the same job, and per-slug concurrency limits are enforced in the database.
- **Atomic claiming (SQLite)** — Claims run inside IMMEDIATE transactions, serializing concurrent workers.

Multiple workers can safely run `crap-cms work` against the same database.

## Configuration Notes

- All servers and workers share the same `crap.toml` and config directory
- Schema sync (`migrate up`) only needs to run once — any server that starts first handles it
- `on_init` hooks run on every server/worker startup
- Email uses the job queue automatically — password resets and verification emails are processed by workers with retries

## Email Configuration

With dedicated workers, the `webhook` email provider is recommended over SMTP for better reliability:

```toml
[email]
provider = "webhook"
webhook_url = "https://api.sendgrid.com/v3/mail/send"
webhook_headers = { Authorization = "Bearer ${SENDGRID_API_KEY}" }
```

Emails are queued and processed by workers with automatic retries. Configure retry behavior:

```toml
[email]
queue_retries = 5
queue_concurrency = 10
```

## Load Balancer Stickiness

Not every request benefits from sticky sessions, and not every route can tolerate reconnects:

| Traffic | Stickiness | Why |
|---------|------------|-----|
| gRPC unary / regular HTTP (find, create, update, login, admin pages) | Not required | Each request is stateless. Round-robin freely. |
| gRPC Subscribe / Admin SSE (long-lived streams) | **Recommended** | Reconnects to a different node lose the in-flight subscription state (sequence cursor, filter context). The client has to re-subscribe, which may miss events in the gap. |

With `transport = "redis"` for live updates, a reconnecting subscriber on a different node will still see all future events — but the state that was held on the original node (current sequence position, any pending-but-undelivered events buffered in the broadcast channel) is gone. Sticky sessions keep that state warm across the connection's lifetime.

A reasonable default is sticky sessions for `/api/*` streaming routes and the admin SSE endpoint, round-robin for everything else.
