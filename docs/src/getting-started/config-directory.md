# Config Directory

All customization lives in a single directory passed via `--config`. This is the only required argument.

## Directory Structure

```
my-project/
├── crap.toml              # Server/database/auth configuration
├── init.lua               # Runs at startup (register global hooks, etc.)
├── .luarc.json            # LuaLS config for IDE support
├── collections/           # One .lua file per collection
│   ├── posts.lua
│   ├── users.lua
│   └── media.lua
├── globals/               # One .lua file per global
│   └── site_settings.lua
├── hooks/                 # Lua modules referenced by hook strings
│   ├── posts.lua
│   └── access.lua
├── templates/             # Handlebars overrides for admin UI
│   └── fields/
│       └── custom.hbs
├── static/                # Static file overrides (CSS, JS, fonts)
├── data/                  # SQLite database (auto-created)
│   └── crap.db
├── uploads/               # Uploaded files (auto-created per collection)
│   └── media/
└── types/                 # Auto-generated Lua type definitions
    └── crap.lua
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
cargo run -- --config ./my-project --generate-types
```

This writes `types/crap.lua` with LuaLS annotations for the entire `crap.*` API.
