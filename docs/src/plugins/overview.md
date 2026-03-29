# Plugins

Crap CMS doesn't have a formal plugin system. It doesn't need one — Lua's open module
system provides everything required. A plugin is just a Lua module that modifies
collections, globals, registers hooks, or any combination.

## How It Works

1. Collection and global definition files (`collections/*.lua`, `globals/*.lua`) are auto-loaded first.
2. `init.lua` runs after all definitions are registered.
3. Plugins are `require()`-d from `init.lua` and can read, modify, or extend any
   registered collection or global.

This works because `crap.collections.define()` and `crap.globals.define()` overwrite —
calling either twice for the same slug replaces the first definition with the second.

## Writing a Plugin

A plugin is a Lua module that returns a table with an `install()` function:

```lua
-- plugins/audit_log.lua
local M = {}

function M.install()
    -- Register a global hook that runs for all collections
    crap.hooks.register("before_change", function(ctx)
        if ctx.operation == "create" then
            ctx.data.created_by = ctx.user and ctx.user.email or "system"
        end
    end)
end

return M
```

## Modifying Collections

Use `crap.collections.config.get()` to retrieve a single collection, or
`crap.collections.config.list()` to iterate all collections:

```lua
-- plugins/alt_text.lua
-- Adds an alt_text field to every upload collection.
local M = {}

function M.install()
    for slug, def in pairs(crap.collections.config.list()) do
        if def.upload then
            def.fields[#def.fields + 1] = crap.fields.text({
                name = "alt_text",
                admin = { description = "Describe this image for accessibility" },
            })
            crap.collections.define(slug, def)
        end
    end
end

return M
```

### Patching a Single Collection

```lua
-- plugins/post_reading_time.lua
local M = {}

function M.install()
    local def = crap.collections.config.get("posts")
    if not def then return end

    def.fields[#def.fields + 1] = crap.fields.number({
        name = "reading_time",
        admin = { readonly = true, description = "Estimated reading time (minutes)" },
    })

    -- Add a hook to calculate it
    def.hooks.before_change = def.hooks.before_change or {}
    def.hooks.before_change[#def.hooks.before_change + 1] = "plugins.post_reading_time.calculate"

    crap.collections.define("posts", def)
end

function M.calculate(ctx)
    local body = ctx.data.body or ""
    local words = select(2, body:gsub("%S+", ""))
    ctx.data.reading_time = math.ceil(words / 200)
    return ctx
end

return M
```

## Modifying Globals

The same pattern works for globals:

```lua
-- plugins/global_meta.lua
-- Adds a "last_updated_by" field to every global.
local M = {}

function M.install()
    for slug, def in pairs(crap.globals.config.list()) do
        def.fields[#def.fields + 1] = crap.fields.text({
            name = "last_updated_by",
            admin = { readonly = true },
        })
        crap.globals.define(slug, def)
    end
end

return M
```

### Patching a Single Global

```lua
local def = crap.globals.config.get("site_settings")
if def then
    def.fields[#def.fields + 1] = crap.fields.richtext({ name = "footer_html" })
    crap.globals.define("site_settings", def)
end
```

## Installing a Plugin

```lua
-- init.lua
require("plugins.alt_text").install()
require("plugins.post_reading_time").install()
```

A plugin is a file in your config directory (typically `plugins/`). Install it by copying
or cloning into that directory and adding a `require` line.

## Collection-Level Override

When a plugin adds fields to all collections but you want a custom version for one
collection, just define the field directly in that collection's Lua file. The plugin
should check for existing fields before adding:

```lua
-- Plugin checks before adding
for _, field in ipairs(def.fields) do
    if field.name == "seo" then
        has_seo = true
        break
    end
end

if not has_seo then
    def.fields[#def.fields + 1] = seo_fields
    crap.collections.define(slug, def)
end
```

This way, `posts.lua` can define its own custom SEO group (e.g., with an extra `og_image`
field) and the plugin will skip it.

## API Reference

| Function | Description |
|----------|-------------|
| `crap.collections.config.get(slug)` | Get a collection's full config as a Lua table. Returns `nil` if not found. |
| `crap.collections.config.list()` | Get all collections as a `{ slug = config }` table. Iterate with `pairs()`. |
| `crap.collections.define(slug, config)` | Define or redefine a collection. |
| `crap.globals.config.get(slug)` | Get a global's full config as a Lua table. Returns `nil` if not found. |
| `crap.globals.config.list()` | Get all globals as a `{ slug = config }` table. Iterate with `pairs()`. |
| `crap.globals.define(slug, config)` | Define or redefine a global. |
| `crap.hooks.register(event, fn)` | Register a global hook for all collections. |

## Plugin Execution Order

Since `init.lua` runs sequentially, plugins install in the order you `require` them.
If plugin B depends on fields added by plugin A, require A first:

```lua
require("plugins.seo").install()         -- adds seo fields
require("plugins.seo_defaults").install() -- sets default values on seo fields
```
