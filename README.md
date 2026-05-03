# Crap CMS

[![CI](https://github.com/dkluhzeb/crap-cms/actions/workflows/ci.yml/badge.svg)](https://github.com/dkluhzeb/crap-cms/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Docker](https://img.shields.io/badge/docker-ghcr.io-blue)](https://ghcr.io/dkluhzeb/crap-cms)

A headless CMS written in Rust. Define your schema in Lua, extend everything with hooks, query via gRPC, manage content through an HTMX admin UI. Single binary, SQLite, zero infrastructure.

> **Alpha software.** While in `0.x`, breaking changes may appear without prior notice.

## Try it

```bash
docker run -p 3000:3000 -p 50051:50051 ghcr.io/dkluhzeb/crap-cms:latest serve -C /example
```

Open [http://localhost:3000/admin](http://localhost:3000/admin) — login: `admin@crap.studio` / `admin123`

Or install the latest release with the shell installer (Linux x86_64 / ARM64):

```bash
curl -fsSL https://raw.githubusercontent.com/dkluhzeb/crap-cms/main/scripts/install.sh | bash
```

The installer downloads the matching binary, verifies it against `SHA256SUMS`, and places it under `~/.local/share/crap-cms/versions/<version>/`. It wires up a shim at `~/.local/bin/crap-cms` — add that directory to your `PATH` if it isn't already.

<details>
<summary>Inspect the script before running (recommended)</summary>

```bash
curl -fsSL https://raw.githubusercontent.com/dkluhzeb/crap-cms/main/scripts/install.sh -o install.sh
less install.sh               # review
sha256sum install.sh          # compare against the repo's scripts/install.sh
bash install.sh               # run once you're satisfied
```

For a reproducible install, pin the URL to a tag: `…/crap-cms/v0.1.0-alpha.5/scripts/install.sh`.
</details>

Prefer a direct [release download](https://github.com/dkluhzeb/crap-cms/releases)? Grab `crap-cms-linux-x86_64` (or the arch you need), `chmod +x`, and run — no other dependencies.

### Managing versions

Once installed, the binary manages itself:

```bash
crap-cms update check           # is a newer release out?
crap-cms update list            # remote releases, marked with (installed) and *
crap-cms update install v0.1.0-alpha.5
crap-cms update use v0.1.0-alpha.5
crap-cms update                 # shortcut: install latest + switch to it
```

`crap-cms serve` prints a one-line nudge on startup when a newer release is cached (24h TTL, populated by `crap-cms update check`). Set `[update] check_on_startup = false` in `crap.toml` to silence it.

## Features

- **Collections** with 20 field types (text, number, textarea, richtext, select, radio, checkbox, date, email, json, code, relationship, upload, array, group, blocks, row, collapsible, tabs, join)
- **Globals** — single-document collections for site-wide settings
- **Lua hooks** at three levels (field, collection, global) with full CRUD access inside transactions
- **Access control** — collection-level and field-level, with filter constraints
- **Authentication** — JWT sessions, password login, custom auth strategies, email verification, password reset
- **Uploads** — file uploads with automatic image resizing and format conversion (WebP, AVIF)
- **Relationships** — has-one and has-many with configurable population depth and caching
- **Localization** — per-field opt-in with locale-suffixed columns and fallback
- **Versions & Drafts** — document version history with draft/publish workflow
- **Live updates** — real-time mutation events via SSE and gRPC streaming
- **Background jobs** — cron scheduling, retries, queues, heartbeat monitoring
- **Admin UI** — template overlay system, theme switching, Web Components, fully overridable
- **gRPC API** — full CRUD with filtering, pagination, server reflection. [REST proxy](https://github.com/dkluhzeb/crap-rest) available
- **MCP server** — Model Context Protocol integration for AI tooling
- **File logging** — optional rotating file logs with `crap-cms logs` viewer, auto-enabled for detached mode
- **CLI** — interactive scaffolding, blueprints, data export/import, backups

For full documentation, see the [user manual](https://dkluhzeb.github.io/crap-cms/).

## Motivation

I built several Rust/WebAssembly frontend projects and couldn't find a CMS that fit the stack. So I built one.

The idea: a simple CMS written in Rust, extensible via Lua, with no complicated build steps or infrastructure requirements.

Inspiration came from what I consider the best solutions out there:

- **Lua scripting** — modeled after Neovim and Awesome WM
- **Hook system** — inspired by [Payload CMS](https://payloadcms.com), an excellent CMS for anyone needing a production-ready solution
- **CLI tooling** — influenced by Laravel's Artisan
- **SQLite + WAL + FTS** — single binary, zero external dependencies, database layer abstracted behind a trait
- **HTMX + Web Components** — themeable like WordPress child themes, no frontend build step
- **gRPC API** — binary protocol with streaming, plus a [REST proxy](https://github.com/dkluhzeb/crap-rest) for plain JSON over HTTP

## Deployment

### Docker

Production — mount your own config directory:

```bash
docker run -v /path/to/config:/config -p 3000:3000 -p 50051:50051 \
  ghcr.io/dkluhzeb/crap-cms:latest
```

Images are Alpine-based (~30 MB) and published to `ghcr.io/dkluhzeb/crap-cms`.

| Tag | Description |
|-----|-------------|
| `latest` | Most recent tagged release |
| `X.Y.Z-alpha.N` | Tagged release |
| `X.Y` | Latest patch in a minor series |
| `nightly` | Latest main build (x86_64) |
| `sha-<commit>` | Pinned to a specific commit |

### Static Binaries

Pre-built static binaries are attached to each [GitHub Release](https://github.com/dkluhzeb/crap-cms/releases):

- `crap-cms-linux-x86_64` — Linux x86_64 (musl, fully static)
- `crap-cms-linux-aarch64` — Linux ARM64 (musl, fully static)
- `crap-cms-windows-x86_64.exe` — Windows x86_64

No runtime dependencies required. An `example.tar.gz` with a sample project is included in each release.

---

## Development

| Component    | Technology                            |
|--------------|---------------------------------------|
| Language     | Rust (edition 2024)                   |
| Web / Admin  | Axum + Handlebars + HTMX             |
| API          | gRPC via Tonic + Prost               |
| Database     | SQLite via rusqlite (WAL mode)        |
| Hooks        | Lua 5.4 via mlua                      |
| IDs          | nanoid                                |

### Project Structure

```
src/
├── main.rs           # binary entry point, subcommand dispatch
├── lib.rs            # crate exports
├── config/           # crap.toml loading + defaults
├── core/             # collection, field, document types
├── db/               # pool, migrations, query builder
├── service/          # service layer — chokepoint for CRUD lifecycle
├── hooks/            # Lua VM, crap.* API, hook lifecycle
├── admin/            # Axum admin UI (handlers, templates)
├── api/              # Tonic gRPC service
├── scheduler/        # background job scheduler
├── mcp/              # Model Context Protocol server
├── cli/              # CLI argument parsing
├── commands/         # CLI subcommand implementations
├── typegen/          # type generation (Rust, Lua, TS, Python, Go)
└── scaffold/         # init/make scaffolding
```

### Building

```bash
git config core.hooksPath .githooks  # enable shared git hooks (fmt + clippy pre-commit)
cargo build                          # compile
cargo test                           # run tests
cargo tarpaulin --out html           # coverage report
crap-cms serve -C ./example          # run with example config
```

Default templates and static files are compiled into the binary via `include_dir!`. The config directory overlay takes priority — any file placed in `{config_dir}/static/` or `{config_dir}/templates/` is served from disk without rebuilding. Only changes to the *embedded* defaults (under `static/` or `templates/` in the source tree) require `cargo build`.

Dev mode (`admin.dev_mode = true` in `crap.toml`) reloads templates from disk on every request instead of caching them.

#### CSP and inline `<script>` in custom templates

The admin UI ships a strict, nonce-based Content-Security-Policy: inline scripts are only allowed when they carry a per-request nonce. If your overlay template emits an inline `<script>`, it **must** use:

```html
<script nonce="{{crap.csp_nonce}}">
  /* your code */
</script>
```

The nonce is regenerated on every request and exposed to all admin templates as `crap.csp_nonce` (alongside `crap.version`, `crap.build_hash`, etc.). Inline event handler attributes (`onclick="..."`, `onchange="..."`, …) are **blocked** under the default policy — replace them with delegated listeners (see `static/components/password-toggle.js` for a minimal pattern) or with a small inline `<script nonce="...">` block. Inline `style="..."` attributes are still permitted (`style-src` keeps `'unsafe-inline'` for now). CSP can be tuned or disabled via `[admin.csp]` in `crap.toml`.

### API Testing

Requires [grpcurl](https://github.com/fullstorydev/grpcurl) and a running server:

```bash
source tests/api.sh
find_posts
create_post
```

### Load Testing

Requires [ghz](https://github.com/bojand/ghz), grpcurl, protoc, jq, and a running server:

```bash
./tests/grpc_loadtest.sh                              # all scenarios, default settings
./tests/grpc_loadtest.sh --duration 5                 # shorter runs
./tests/grpc_loadtest.sh --concurrency 1,10           # custom concurrency levels
./tests/grpc_loadtest.sh --scenarios find,count        # specific scenarios only
```

Scenarios: `describe`, `count`, `find`, `find_where`, `find_by_id`, `find_deep`, `create`, `update`.

### Documentation

```bash
cd docs && mdbook serve            # local preview at localhost:3000
```

### CI/CD

| Workflow | Trigger | What it does |
|----------|---------|--------------|
| **CI** | Every push & PR | fmt, clippy, tests |
| **Nightly** | Push to main | x86_64 musl binary, Docker `nightly` tag, docs deploy |
| **Release** | Tag `v*` | Multi-arch binaries, Docker semver tags, GitHub Release, docs deploy |

## Cargo Features

| Feature | Default | Description |
|---------|---------|-------------|
| `sqlite` | yes | SQLite backend (bundled, no runtime dependency). |
| `postgres` | no | PostgreSQL backend (via `tokio-postgres` + `deadpool-postgres`). |
| `s3-storage` | no | S3-compatible upload storage (AWS S3, MinIO, R2, B2, Spaces). |
| `redis` | no | Redis-backed cache and cross-node live-update transport. |
| `browser-tests` | no | Headless Chrome end-to-end tests (via `chromiumoxide`). |

Enable a feature at build time with `cargo build --features <name>` (combine with commas).

## License

MIT
