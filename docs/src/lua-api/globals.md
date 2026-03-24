# crap.globals

Global (singleton document) definition and runtime operations.

## crap.globals.define(slug, config)

Define a new global. Call this in global definition files (`globals/*.lua`).

```lua
crap.globals.define("site_settings", {
    labels = { singular = "Site Settings" },
    fields = {
        { name = "site_name", type = "text", required = true, default_value = "My Site" },
        { name = "tagline", type = "text" },
    },
})
```

See [Globals](../globals/overview.md) for the full config reference.

## crap.globals.config.get(slug)

Get a global's current definition as a Lua table. The returned table is round-trip
compatible with `define()` — you can modify it and pass it back.

Returns `nil` if the global doesn't exist.

```lua
local def = crap.globals.config.get("site_settings")
if def then
    def.fields[#def.fields + 1] = { name = "footer_text", type = "textarea" }
    crap.globals.define("site_settings", def)
end
```

## crap.globals.config.list()

Get all registered globals as a slug-keyed table. Iterate with `pairs()`.

```lua
for slug, def in pairs(crap.globals.config.list()) do
    -- Add a "last_updated_by" field to every global
    def.fields[#def.fields + 1] = { name = "last_updated_by", type = "text" }
    crap.globals.define(slug, def)
end
```

See [Plugins](../plugins/overview.md) for patterns using these functions.

## crap.globals.get(slug, opts?)

Get a global's current value. Returns a document table.

**Only available inside hooks with transaction context.**

### Parameters

| Parameter | Type | Description |
|-----------|------|-------------|
| `slug` | string | Global slug |
| `opts` | table (optional) | Options table |

### Options

| Key | Type | Description |
|-----|------|-------------|
| `locale` | string | Locale code (e.g., `"en"`, `"de"`). Fetches locale-specific field values. If omitted, returns the default locale data. |

```lua
local settings = crap.globals.get("site_settings")
print(settings.site_name)  -- "My Site"
print(settings.id)         -- always "default"

-- Fetch German locale data
local settings_de = crap.globals.get("site_settings", { locale = "de" })
```

## crap.globals.update(slug, data, opts?)

Update a global's value. Returns the updated document.

**Only available inside hooks with transaction context.**

### Parameters

| Parameter | Type | Description |
|-----------|------|-------------|
| `slug` | string | Global slug |
| `data` | table | Fields to update |
| `opts` | table (optional) | Options table |

### Options

| Key | Type | Description |
|-----|------|-------------|
| `locale` | string | Locale code (e.g., `"en"`, `"de"`). Updates locale-specific field values. If omitted, updates the default locale data. |

```lua
local settings = crap.globals.update("site_settings", {
    site_name = "New Site Name",
    tagline = "A new beginning",
})

-- Update German locale data
crap.globals.update("site_settings", {
    site_name = "Neuer Seitenname",
}, { locale = "de" })
```
