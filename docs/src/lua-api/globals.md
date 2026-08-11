# crap.globals

Global (singleton document) definition and runtime operations.

## crap.globals.define(slug, config)

Define a new global. **Init-only:** call this from `globals/*.lua`,
`init.lua`, or any file loaded by `require` from those — i.e. while
the `InitPhase` marker is set on the VM. Runtime calls error with:

> `crap.globals.define must be called from a definition file or
> init.lua. To change a registered global, edit the file and restart
> the process.`

```lua
crap.globals.define("site_settings", {
    labels = { singular = "Site Settings" },
    fields = {
        crap.fields.text({ name = "site_name", required = true, default_value = "My Site" }),
        crap.fields.text({ name = "tagline" }),
    },
})
```

See [Globals](../globals/overview.md) for the full config reference.

## crap.globals.config.get(slug)

Get a global's current definition as a Lua table. The returned table is round-trip
compatible with `define()` — you can modify it and pass it back, **inside init**.

Returns `nil` if the global doesn't exist.

```lua
-- inside a definition file or init.lua
local def = crap.globals.config.get("site_settings")
if def then
    def.fields[#def.fields + 1] = crap.fields.textarea({ name = "footer_text" })
    crap.globals.define("site_settings", def)
end
```

## crap.globals.config.list()

Get all registered globals as a slug-keyed table. Iterate with
`pairs()`. The realistic bulk-modify pattern runs from `init.lua` (or
a file it `require`s) where the strict guard on `define` doesn't fire.

```lua
-- inside init.lua / a plugin loaded by init.lua
for slug, def in pairs(crap.globals.config.list()) do
    -- Add a "last_updated_by" field to every global
    def.fields[#def.fields + 1] = crap.fields.text({ name = "last_updated_by" })
    crap.globals.define(slug, def)
end
```

See [Plugins](../plugins/overview.md) for patterns using these functions.

## Runtime operations — `crap.globals.<slug>`

Every global registered via `define()` gets a typed accessor at
`crap.globals.<slug>` with `get` / `update` methods. The slug is
bound; return values are typed as the per-global document class.

Both operations are **only available inside hooks with transaction
context**.

> For the rare dynamic-slug case (you don't know the slug until
> runtime — e.g. iterating `crap.globals.config.list()`), call the
> slug-keyed dispatch: `crap.globals.get(slug, opts?)` /
> `crap.globals.update(slug, data, opts?)`. Same semantics, slug as
> the first arg.

### `crap.globals.<slug>.get(opts?)`

Get a global's current value. Returns the typed document.

**Options:**

| Key | Type | Description |
| --- | --- | --- |
| `locale` | string | Locale code (e.g. `"en"`, `"de"`). Fetches locale-specific field values; omit for default locale. |
| `override_access` | boolean | Bypass the global's `access.read` check (default `false`). |
| `draft` | boolean | Read unpublished (draft) content (default `false`). Gated by `access.draft` (falling back to `access.update`); a reader without draft access silently gets the published snapshot, never an error. When the global has drafts enabled and has been unpublished, a normal read serves the last published snapshot; set `true` to read the draft. |

```lua
local settings = crap.globals.site_settings.get()
print(settings.site_name)  -- "My Site"
print(settings.id)         -- always "default"

-- Fetch German locale data
local settings_de = crap.globals.site_settings.get({ locale = "de" })

-- Read the unpublished draft of an unpublished global
local draft = crap.globals.site_settings.get({ draft = true })
```

### `crap.globals.<slug>.update(data, opts?)`

Update a global's value. `data` is a partial payload — only the
fields being changed need to be present. Returns the updated typed
document.

**Options:**

| Key | Type | Description |
|-----|------|-------------|
| `locale` | string | Locale code. Updates locale-specific field values; omit for default locale. |
| `override_access` | boolean | Bypass the global's `access.update` check (default `false`). |
| `hooks` | boolean | Run lifecycle hooks (default `true`). Set `false` for seeding/migrations. |
| `draft` | boolean | When `true` and the global has `versions.drafts`, performs a version-only save (main row unchanged, only a draft snapshot). Default `false`. Mirrors `crap.collections.update`. |

```lua
local settings = crap.globals.site_settings.update({
    site_name = "New Site Name",
    tagline = "A new beginning",
})

-- Update German locale data
crap.globals.site_settings.update({
    site_name = "Neuer Seitenname",
}, { locale = "de" })

-- Save a draft edit (main row stays on the published value)
crap.globals.site_settings.update({ tagline = "WIP" }, { draft = true })
```

### `crap.globals.<slug>.unpublish(opts?)`

Revert a versioned global's `_status` to `"draft"` without modifying its
stored field data. Only available on globals with `versions` enabled
(errors otherwise). Mirrors `crap.collections.unpublish`.

**Options:**

| Key | Type | Description |
|-----|------|-------------|
| `override_access` | boolean | Bypass the global's `access.update` check (default `false`). |
| `hooks` | boolean | Run lifecycle hooks (default `true`). |

```lua
crap.globals.banner.unpublish()
```

### `crap.globals.validate(slug, data, opts?)`

Validate global field data **without persisting**. Runs the full
before-write pipeline (field coercion, validators, `before_validate`
hooks) and returns `{ valid = true }` or
`{ valid = false, errors = { field = "message", ... } }`. Globals are a
singleton document, so validation always runs in update mode against the
fixed `default` row — there is no create mode and no `id` option. Mirrors
`crap.collections.validate`.

**Options:**

| Key | Type | Description |
|-----|------|-------------|
| `locale` | string | Locale code for localized-field validation; omit for default locale. |
| `override_access` | boolean | Bypass the global's `access.update` check (default `false`). |
| `draft` | boolean | Validate as a draft (relaxes required checks for globals with drafts enabled). |

```lua
local result = crap.globals.validate("site_settings", {
    tagline = "A tagline that is far too long for the field",
})

if not result.valid then
    for field, message in pairs(result.errors) do
        print(field .. ": " .. message)
    end
end
```
