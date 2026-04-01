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
2. **Shared file storage** — S3-compatible (build with `--features s3-storage`) or shared filesystem (NFS/EFS)
3. **Shared cache** (recommended) — Redis (build with `--features redis`) for cross-server cache invalidation
4. **Shared rate limits** (recommended) — Redis for cross-server rate limit enforcement

## Setup

### 1. Database

```toml
# crap.toml (all servers share the same config)
[database]
backend = "postgres"
url = "host=db.example.com user=crap dbname=crap_cms"
```

### 2. File Storage

```toml
[upload]
storage = "s3"

[upload.s3]
bucket = "my-uploads"
endpoint = "https://s3.amazonaws.com"
access_key = "${AWS_ACCESS_KEY}"
secret_key = "${AWS_SECRET_KEY}"
```

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

### 4. Rate Limits

Without shared rate limits, each server tracks login attempts independently. Use Redis to enforce global rate limits:

```toml
[auth]
rate_limit_backend = "redis"
# rate_limit_redis_url defaults to cache.redis_url if empty
```

### 5. App Servers

Run without the scheduler — job processing is handled by dedicated workers.

```bash
crap-cms serve --no-scheduler
```

### 6. Workers

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
