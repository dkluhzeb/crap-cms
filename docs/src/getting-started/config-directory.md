# Config Directory

All customization lives in a single config directory. When you run CLI commands from inside this directory (or a subdirectory), the config is auto-detected by walking up and looking for `crap.toml`. You can also set it explicitly with `--config`/`-C` or the `CRAP_CONFIG_DIR` environment variable.

## Directory Structure

```
my-project/
├── crap.toml              # Server/database/auth configuration
├── init.lua               # Runs at startup (register global hooks, etc.)
├── .luarc.json            # LuaLS config for IDE support
├── .gitignore             # Ignores data/, uploads/, types/ by default
├── stylua.toml            # StyLua formatter config for Lua files
├── .mcp.json              # MCP client config (points an MCP client at `crap-cms mcp`)
├── collections/           # One .lua file per collection
│   ├── posts.lua
│   ├── users.lua
│   └── media.lua
├── globals/               # One .lua file per global
│   └── site_settings.lua
├── hooks/                 # Lua modules referenced by hook strings
│   ├── posts.lua
│   └── access.lua
├── access/                # Reusable access-control Lua modules
├── jobs/                  # Job definitions + handlers (see `jobs` feature)
├── routes/                # Custom HTTP route handlers (`crap.routes.register`)
├── plugins/               # Plugin Lua modules (see `plugins` feature)
├── migrations/            # Custom SQL migrations (see `migrate` command)
├── templates/             # Handlebars overrides for admin UI
│   ├── fields/            # Field render templates (`make field`)
│   │   └── custom.hbs
│   ├── pages/             # Custom admin pages (`make page`)
│   └── slots/             # Slot widgets (`make slot`)
├── translations/          # (optional, user-created) Admin UI translation overrides (JSON per locale)
│   └── de.json
├── static/                # Static file overrides (CSS, JS, fonts)
│   ├── components/        # Custom Web Components (`make component`, `make field`)
│   └── styles/themes/     # Theme CSS files (`make theme`)
├── data/                  # Runtime data (auto-created)
│   ├── crap.db            # SQLite database
│   ├── crap.pid           # Process ID file (when running with --detach)
│   └── logs/              # Rotating log files (when [logging] file = true)
├── uploads/               # Uploaded files (auto-created per collection)
│   └── media/
└── types/                 # Auto-generated type definitions (see `typegen`)
    ├── crap.lua           # API surface (crap.* functions, hook context, …)
    ├── hooks.lua          # Per-collection hook types (data, doc, filters)
    ├── client.<ext>       # Per-collection types for API clients (ts, go, py, rs)
    └── proto.rs           # (optional) `From<proto::Document>` impls for Rust gRPC
```

`crap-cms init` creates `collections/`, `globals/`, `hooks/`, `access/`, `jobs/`, `plugins/`, `templates/`, `static/`, `migrations/` and `types/` (most of them empty) — populate them as needed. `routes/`, `translations/` and the `templates/pages|slots` and `static/components|styles/themes` subdirectories are not scaffolded; the matching `make` subcommand creates them on first use, or create them yourself (see [Localization](../locale/overview.md) for `translations/`).

## File Loading Order

1. `crap.toml` is loaded first (or defaults are used if absent)
2. `collections/*.lua` files are loaded alphabetically
3. `globals/*.lua` files are loaded alphabetically
4. `jobs/*.lua` files are loaded alphabetically
5. `init.lua` is executed last — this is where `crap.routes.register`, `crap.pages.register`, custom providers and other register-only APIs are called

`hooks/`, `access/` and `routes/` are **not** loaded eagerly: their modules are `require`d on demand through hook refs (`"hooks.posts.slugify"`, `"routes.stripe_webhook"`), and every statically-known ref is resolved once at startup so a typo fails to boot rather than at first request.

## Lua Package Path

The config directory is prepended to Lua's `package.path`:

```
<config_dir>/?.lua;<config_dir>/?/init.lua;...
```

This means `require("hooks.posts")` resolves to `<config_dir>/hooks/posts.lua`.

## LuaLS Support

Create a `.luarc.json` in your config directory for IDE autocompletion:

```json
{
    "runtime": { "version": "Lua 5.4" },
    "workspace": { "library": ["./types"] }
}
```

Generate the server-side Lua type definitions with:

```bash
crap-cms typegen lua
```

This writes two files: `types/crap.lua` (the `crap.*` API surface) and `types/hooks.lua` (per-collection hook/data/doc shapes derived from your field definitions). Under `admin.dev_mode = true`, `crap-cms serve` regenerates them on every startup so they stay in sync while you iterate.

External API consumers (TypeScript, Go, Python, Rust) get their own per-collection types via:

```bash
crap-cms typegen client -l ts          # types/client.ts
crap-cms typegen client -l ts,go,py    # multiple languages in one pass
```

See [`typegen`](../cli/flags.md#typegen--generate-typed-definitions) for the full subcommand reference.
