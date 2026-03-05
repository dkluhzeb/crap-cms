# Crap CMS

Headless CMS in Rust. Lua config (neovim-style) + gRPC API + HTMX admin UI.

For usage documentation, see the [user manual](https://crapcms.com/docs) (source in `docs/`).

## Tech Stack

| Component    | Technology                            |
|--------------|---------------------------------------|
| Language     | Rust (edition 2021)                   |
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
├── config.rs         # crap.toml loading + defaults
├── core/             # collection, field, document types
├── db/               # SQLite pool, migrations, query builder
├── hooks/            # Lua VM, crap.* API, hook lifecycle
├── admin/            # Axum admin UI (handlers, templates)
└── api/              # Tonic gRPC service
```

## Development

```bash
cargo build                        # compile
cargo test                         # run tests (2400+)
cargo tarpaulin --out html         # coverage report
crap-cms serve ./example           # run with example config
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

Requires [oha](https://github.com/hatoo/oha), grpcurl, jq, and a running server:

```bash
./tests/loadtest.sh                              # all scenarios, default settings
./tests/loadtest.sh --duration 5                 # shorter runs
./tests/loadtest.sh --concurrency 1,10           # custom concurrency levels
./tests/loadtest.sh --scenarios grpc_find,search  # specific scenarios only
```

Scenarios: `read_list`, `read_single`, `grpc_find`, `grpc_find_deep`, `grpc_write`, `search`.

### Documentation Book

```bash
cd docs && mdbook build            # build the user manual
cd docs && mdbook serve            # local preview at localhost:3000
```

## License

TBD
