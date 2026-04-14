# Single Server Deployment

The default and recommended deployment for most use cases. Everything runs on one machine with a single command.

## Quick Start

```bash
crap-cms serve
```

That's it. The server starts the admin UI, gRPC API, and background job scheduler. SQLite stores everything in a single file (`data/crap.db`).

## Background Mode

```bash
crap-cms serve --detach       # start in background
crap-cms serve --status       # check if running
crap-cms serve --stop         # graceful shutdown
crap-cms serve --restart      # stop + start
```

## Logs

When you run with `--detach`, file logging is auto-enabled — the child process no longer owns a terminal, so stdout / stderr are redirected to `/dev/null` and the log output instead goes to a rotating file in the log directory.

- **Location**: `<config_dir>/data/logs/` by default (configurable via `[logging] path` in `crap.toml`; absolute paths are respected).
- **Rotation**: daily by default (`[logging] rotation`: `"daily"` / `"hourly"` / `"never"`). Old files are pruned on startup based on `[logging] max_files` (default `30`).
- **Reading logs**: `crap-cms logs` tails recent output, `crap-cms logs -f` follows in real time, `crap-cms logs clear` deletes rotated files. Or tail the files under `data/logs/` directly with any tool of your choice.
- **Structured output**: pass `--json` to `crap-cms serve` (or set `CRAP_LOG_FORMAT=json`) for JSON lines suitable for log-aggregation pipelines (Loki, ELK, etc.).

Foreground `crap-cms serve` (no `--detach`) keeps logs on stdout by default — set `[logging] file = true` in `crap.toml` to also write to a file.

If the disk fills up, log writes silently fail; size the log directory to tolerate `max_files × typical_rotation_size` worst-case.

## What You Get

- **Admin UI** on port 3000
- **gRPC API** on port 50051
- **Job scheduler** processing cron jobs and queued tasks
- **Image processing** for upload collections
- **Live updates** via SSE and gRPC streaming

## When to Scale

A single server handles thousands of concurrent readers and hundreds of writes per second. This covers the vast majority of CMS workloads — content sites, editorial teams, headless API backends.

Consider scaling when you need:
- **Multiple app servers** behind a load balancer (high availability)
- **50+ simultaneous content editors** (write throughput)
- **Dedicated job processing** (heavy background work separate from request handling)

See [Multi-Server](multi-server.md) for scaling options.
