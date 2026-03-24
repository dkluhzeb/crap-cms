# Config Directory

All customization lives in a single config directory. When you run CLI commands from inside this directory (or a subdirectory), the config is auto-detected by walking up and looking for `crap.toml`. You can also set it explicitly with `--config`/`-C` or the `CRAP_CONFIG_DIR` environment variable.

## Directory Structure

```
my-project/
├── crap.toml              # Server/database/auth configuration
├── init.lua               # Runs at startup (register global hooks, etc.)
├── .luarc.json            # LuaLS config for IDE support
├── .gitignore             # Ignores data/, uploads/, types/ by default
├── collections/           # One .lua file per collection
│   ├── posts.lua
│   ├── users.lua
│   └── media.lua
├── globals/               # One .lua file per global
│   └── site_settings.lua
├── hooks/                 # Lua modules referenced by hook strings
│   ├── posts.lua
│   └── access.lua
├── migrations/            # Custom SQL migrations (see `migrate` command)
├── templates/             # Handlebars overrides for admin UI
│   └── fields/
│       └── custom.hbs
├── translations/          # Admin UI translation overrides (JSON per locale)
│   └── de.json
├── static/                # Static file overrides (CSS, JS, fonts)
├── data/                  # SQLite database (auto-created)
│   └── crap.db
├── uploads/               # Uploaded files (auto-created per collection)
│   └── media/
└── types/                 # Auto-generated type definitions (see `typegen`)
    ├── crap.lua           # API surface types (crap.* functions)
    └── generated.lua      # Per-collection types (data, doc, hook, filters)
```

## File Loading Order

1. `crap.toml` is loaded first (or defaults are used if absent)
2. `collections/*.lua` files are loaded alphabetically
3. `globals/*.lua` files are loaded alphabetically
4. `init.lua` is executed last

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

Generate type definitions with:

```bash
crap-cms typegen
```

This writes two files: `types/crap.lua` (API surface types for the `crap.*` functions) and `types/generated.lua` (per-collection types derived from your field definitions). Use `-l all` to generate types for all supported languages.
