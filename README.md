# Crap CMS

[![CI](https://github.com/dkluhzeb/crap-cms/actions/workflows/ci.yml/badge.svg)](https://github.com/dkluhzeb/crap-cms/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Docker](https://img.shields.io/badge/docker-ghcr.io-blue)](https://ghcr.io/dkluhzeb/crap-cms)

Headless CMS in Rust. Lua config (neovim-style) + gRPC API + HTMX admin UI.

For usage documentation, see the [user manual](https://crapcms.com/docs) (source in `docs/`).

## Motivation

I built several Rust/WebAssembly frontend projects and couldn't find a CMS that fit the stack. So I built one.

The idea: a simple CMS written in Rust, extensible via a lightweight scripting language, with no complicated build steps or infrastructure requirements. It's also a playground for me to explore ideas and learn — which means things may change, break, or get rewritten.

Inspiration came from what I consider the best solutions out there:

- **Lua scripting API** — modeled after Neovim and Awesome WM, where Lua gives users deep control without touching core code
- **Configuration & hook system** — inspired by [Payload CMS](https://payloadcms.com), an excellent and highly recommended CMS for anyone needing a production-ready solution
- **CLI tooling** — influenced by Laravel's comprehensive Artisan CLI
- **SQLite + WAL + FTS** — sufficient for most of my use cases, and it bundles cleanly into a single binary with zero external dependencies
- **Pure JavaScript with JSDoc types** — no TypeScript, no bundler, no build step. Type safety through JSDoc annotations, checkable with `tsc --checkJs` without compiling anything
- **HTMX + Web Components** — easy to theme (similar to WordPress child themes), no frontend build step. Web Components are a native browser standard — no framework updates, no outdated dependencies, no breaking changes
- **gRPC API** — because I wanted it. A separate [REST proxy](https://github.com/dkluhzeb/crap-rest) is available for those who prefer plain JSON over HTTP

The project is functional but not yet production-ready — it still needs to prove itself.

## Tech Stack

| Component    | Technology                            |
|--------------|---------------------------------------|
| Language     | Rust (edition 2024)                   |
| Web / Admin  | Axum + Handlebars + HTMX             |
| API          | gRPC via Tonic + Prost               |
| Database     | SQLite via rusqlite (WAL mode)        |
| Hooks        | Lua 5.4 via mlua                      |
| IDs          | nanoid                                |

## Project Structure

```
src/
├── main.rs           # binary entry point, subcommand dispatch
├── lib.rs            # crate exports
├── config/           # crap.toml loading + defaults
├── core/             # collection, field, document types
├── db/               # SQLite pool, migrations, query builder
├── hooks/            # Lua VM, crap.* API, hook lifecycle
├── admin/            # Axum admin UI (handlers, templates)
├── api/              # Tonic gRPC service
├── scheduler/        # background job scheduler
├── mcp/              # Model Context Protocol server
├── commands/         # CLI subcommands
└── scaffold/         # init/make scaffolding
```

## Development

```bash
git config core.hooksPath .githooks  # enable shared git hooks (fmt + clippy pre-commit)
cargo build                          # compile
cargo test                           # run tests (3600+)
cargo tarpaulin --out html           # coverage report
crap-cms serve ./example             # run with example config
```

Static files and templates are compiled into the binary via `include_dir!`. Rebuild after changing files in `static/` or `templates/`.

Dev mode (`admin.dev_mode = true` in `crap.toml`) reloads templates from disk per-request — but static files still require a rebuild.

### API Testing

Requires [grpcurl](https://github.com/fullstorydev/grpcurl) and a running server:

```bash
source tests/api.sh
find_posts
create_post
```

### Load Testing

#### gRPC benchmarks (recommended)

Requires [ghz](https://github.com/bojand/ghz), grpcurl, protoc, jq, and a running server:

```bash
./tests/grpc_loadtest.sh                              # all scenarios, default settings
./tests/grpc_loadtest.sh --duration 5                 # shorter runs
./tests/grpc_loadtest.sh --concurrency 1,10           # custom concurrency levels
./tests/grpc_loadtest.sh --scenarios find,count        # specific scenarios only
```

Scenarios: `describe`, `count`, `find`, `find_where`, `find_by_id`, `find_deep`, `create`, `update`.

#### HTTP + gRPC mixed

Requires [oha](https://github.com/hatoo/oha), grpcurl, jq, and a running server:

```bash
./tests/loadtest.sh                                    # all scenarios
./tests/loadtest.sh --scenarios read_list,grpc_find    # specific scenarios
```

### Documentation Book

```bash
cd docs && mdbook build            # build the user manual
cd docs && mdbook serve            # local preview at localhost:3000
```

## Deployment

### Docker

```bash
# Start the server — example project with demo data is pre-loaded
docker run -p 3000:3000 -p 50051:50051 \
  ghcr.io/dkluhzeb/crap-cms:nightly serve /example

# Open http://localhost:3000/admin
# Login: admin@crap.studio / admin123
```

Production — mount your own config:

```bash
docker run -v /path/to/config:/config -p 3000:3000 -p 50051:50051 \
  ghcr.io/dkluhzeb/crap-cms:nightly
```

Images are Alpine-based (~15 MB) and published to `ghcr.io/dkluhzeb/crap-cms`. Tags:

| Tag | Description |
|-----|-------------|
| `nightly` | Latest main build (x86_64) |
| `sha-<commit>` | Pinned to a specific commit |
| `X.Y.Z-alpha.N` | Tagged release |
| `X.Y` | Latest patch in a minor series |
| `latest` | Most recent tagged release |

### Static Binaries

Pre-built static binaries are attached to each [GitHub Release](https://github.com/dkluhzeb/crap-cms/releases):

- `crap-cms-linux-x86_64` — Linux x86_64 (musl, fully static)
- `crap-cms-linux-aarch64` — Linux ARM64 (musl, fully static)
- `crap-cms-windows-x86_64.exe` — Windows x86_64

Download and run directly — no runtime dependencies required. An `example.tar.gz` with a sample project is also included in each release.

```bash
curl -L -o crap-cms \
  https://github.com/dkluhzeb/crap-cms/releases/latest/download/crap-cms-linux-x86_64
chmod +x crap-cms

# Download and extract the example project
curl -L https://github.com/dkluhzeb/crap-cms/releases/latest/download/example.tar.gz \
  | tar xz

./crap-cms serve ./example
```

### CI/CD

| Workflow | Trigger | What it does |
|----------|---------|--------------|
| **CI** | Every push & PR | fmt, clippy, tests |
| **Nightly** | Push to main | x86_64 musl binary, Docker `nightly` tag, docs deploy |
| **Release** | Tag `v*` | Multi-arch binaries, Docker semver tags, GitHub Release (pre-release), docs deploy |

## License

MIT
