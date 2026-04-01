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
