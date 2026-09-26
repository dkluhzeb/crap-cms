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
crap-cms serve --stop         # graceful shutdown: drains running jobs first
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

## Exposure and Connection Limits

Both listeners protect themselves without a proxy in front: each holds at most
`[server] max_connections` connections open (by default derived from the
open-file limit and logged at startup; `serve` raises the soft limit to the hard
limit first, so raise the hard limit — systemd `LimitNOFILE`, `ulimit -Hn` — to
raise it), a connection
that has not sent complete request headers (admin) or the HTTP/2 preface (gRPC)
within `header_read_timeout` (default 30s) is closed, an admin request must
arrive and be answered within `request_timeout` (default 60s) — a save still
running at that point is rolled back, so the `408` it gets means nothing
changed — the public login
/ reset / MFA / callback routes accept at most `auth_body_limit` (default
64KB), and the gRPC server caps streams per connection
(`grpc_max_concurrent_streams`) and drops peers that stop answering keep-alive
pings (`grpc_keepalive_interval`).

With `[server] h2c = true` the admin listener also speaks HTTP/2 without TLS.
Such a connection is pinged every 60 seconds and closed when the peer stops
answering — but the HTTP/2 server has no idle timeout, so an idle connection
whose client keeps answering the pings stays open, holding a `max_connections`
slot, until the client closes it. Enable `h2c` only for a reverse proxy you run
in front of the admin port (which is what the option is for), not for arbitrary
clients.

File uploads into upload collections have no deadline by default
(`upload_timeout = 0`), so a large file on a slow link is never cut off; set
`upload_timeout` when anonymous clients may upload. For any internet-facing
deployment a buffering reverse proxy such as nginx or Caddy in front is still
recommended. Behind a proxy, set `trust_proxy = true` and list the proxy's
addresses in `trusted_proxies` so per-IP rate limits see the real client; see
[`[server]`](../configuration/crap-toml.md#server).

## When to Scale

A single server handles thousands of concurrent readers and hundreds of writes per second. This covers the vast majority of CMS workloads — content sites, editorial teams, headless API backends.

Consider scaling when you need:
- **Multiple app servers** behind a load balancer (high availability)
- **50+ simultaneous content editors** (write throughput)
- **Dedicated job processing** (heavy background work separate from request handling)

See [Multi-Server](multi-server.md) for scaling options.
