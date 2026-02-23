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

## crap.globals.get(slug)

Get a global's current value. Returns a document table.

**Only available inside hooks with transaction context.**

```lua
local settings = crap.globals.get("site_settings")
print(settings.site_name)  -- "My Site"
print(settings.id)         -- always "default"
```

## crap.globals.update(slug, data)

Update a global's value. Returns the updated document.

**Only available inside hooks with transaction context.**

```lua
local settings = crap.globals.update("site_settings", {
    site_name = "New Site Name",
    tagline = "A new beginning",
})
```
