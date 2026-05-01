# Scenario 5: Add a custom admin page

**Goal**: add a "System info" page to the admin showing live
process metrics, with its own URL, sidebar entry, and
admin-only access.

**Difficulty**: low. ~15 minutes from scratch to a working page.

**You'll touch**: one HBS template, one `init.lua` block,
optionally one access function.

## Approach

Crap CMS has filesystem-routed custom pages. Drop a template at
`<config_dir>/templates/pages/<slug>.hbs` and the route
`/admin/p/<slug>` renders it automatically. Optionally call
`crap.pages.register("<slug>", { ... })` in `init.lua` to add a
sidebar entry, restrict access, and set the page label / icon.

For dynamic data on the page, use the same `crap.template_data.register`
+ `{{data "name"}}` pattern as [dashboard widgets](04-dashboard-widget.md)
— no separate "page data" mechanism.

## Step 1 — drop the template

`<config_dir>/templates/pages/system_info.hbs`:

```hbs
{{#> layout/base}}
  <h1>System info</h1>

  <div class="cards">
    <div class="card">
      <div class="card__header">
        <span class="material-symbols-outlined">info</span>
        <h3>Build</h3>
      </div>
      <div class="card__body">
        <p><strong>Version:</strong> {{crap.version}}</p>
        <p><strong>Build hash:</strong> <code>{{crap.build_hash}}</code></p>
        <p><strong>Dev mode:</strong> {{#if crap.dev_mode}}on{{else}}off{{/if}}</p>
      </div>
    </div>

    {{#with (data "system_info_counts")}}
      <div class="card">
        <div class="card__header">
          <span class="material-symbols-outlined">database</span>
          <h3>Counts</h3>
        </div>
        <div class="card__body">
          <p><strong>Collections:</strong> {{collections}}</p>
          <p><strong>Globals:</strong> {{globals}}</p>
          <p><strong>Custom pages:</strong> {{custom_pages}}</p>
        </div>
      </div>
    {{/with}}
  </div>
{{/layout/base}}
```

The template wraps `layout/base` (the standard admin chrome) and
uses three pieces of context:

- **`crap.*`** — process metadata (version, build hash, dev mode,
  site name, etc.). See [template context reference](../reference/template-context.md).
- **`{{data "system_info_counts"}}`** — pulls from a Lua function
  registered below. The data fn runs **on demand** — only when the
  template references it.
- **Standard HBS** — `{{#if}}`, `{{#with}}`, etc.

The page is now reachable at `/admin/p/system_info`. Without a Lua
registration, it just doesn't appear in the sidebar — direct URL
access works.

## Step 2 — register the sidebar entry

Add to `<config_dir>/init.lua`:

```lua
crap.pages.register("system_info", {
  section = "Tools",          -- sidebar section heading; nil = ungrouped
  label = "System info",      -- sidebar label; nil = no nav entry
  icon = "monitoring",        -- Material Symbols icon name
  access = "access.admin_only",  -- optional access function ref
})
```

The slug **must match** the template filename (without `.hbs`).
Slugs are restricted to `a-z`, `0-9`, `-`, `_` — anything else is
rejected at registration time.

| Field | Required? | Effect |
|---|---|---|
| `section` | no | Sidebar section heading. `nil` → renders ungrouped at the bottom. |
| `label` | no | Sidebar label. `nil` → page routes but isn't shown in nav. |
| `icon` | no | Material Symbols icon name (e.g. `"monitoring"`, `"heart-pulse"`). |
| `access` | no | Lua function-ref (registered via `crap.access.register`). Returning `false` produces a 403 and hides the page from sidebar nav. |

## Step 3 — provide the dynamic counts

In `<config_dir>/init.lua`:

```lua
crap.template_data.register("system_info_counts", function(ctx)
  local nav = ctx.nav or {}
  return {
    collections = nav.collections and #nav.collections or 0,
    globals = nav.globals and #nav.globals or 0,
    custom_pages = nav.custom_pages and #nav.custom_pages or 0,
  }
end)
```

The `ctx` argument is the **page render context** — `ctx.user`,
`ctx.nav`, `ctx.crap`, etc. are all available. The function returns
a table; that table is what `{{#with (data "system_info_counts")}}`
binds in the template.

The function runs **on demand** — only when a rendering template
evaluates the `{{data}}` call. Pages that don't reference it pay no
cost.

## Step 4 (optional) — restrict to admins

If you set `access = "access.admin_only"` above, define the access
function. Conventional layout: a single function per file under
`<config_dir>/access/`, then required in `init.lua`. But you can
also register inline.

Either way, register via `crap.access.register`:

```lua
-- <config_dir>/access/admin_only.lua
---@param context crap.AccessContext
---@return boolean
return function(context)
  return context.user ~= nil and context.user.role == "admin"
end
```

```lua
-- <config_dir>/init.lua
crap.access.register("access.admin_only", require("access.admin_only"))
```

If the function returns `false` for the current user, the route
returns 403 and the sidebar entry is filtered out (so non-admins
don't see a link they can't follow).

## Step 5 — restart

Lua loads at startup. Restart crap-cms (or kill and re-run `serve`).
Open `/admin` — your "System info" entry appears in the "Tools"
section of the sidebar. Click it; the page renders at
`/admin/p/system_info`.

## What this scenario covers

- ✅ A new admin URL (`/admin/p/<slug>`)
- ✅ A sidebar entry with section, label, icon
- ✅ Per-page access control via Lua function
- ✅ Dynamic data via `crap.template_data.register`
- ✅ Full admin chrome (header, sidebar, theme switcher, all of it)

## What this scenario *doesn't* cover

- **Custom POST endpoints / form actions** — pages render
  read-only. For mutations, drive them through gRPC or via a
  collection's standard CRUD routes; there's no `/admin/p/<slug>/submit`
  pattern. (See the gRPC API for write paths from custom JS.)
- **Streaming / SSE / live updates per page** — the page renders
  server-side. Use `<crap-live-events>` or HTMX swaps for dynamic
  content within the page.

## Verifying

```
$ crap-cms templates status
  · templates/pages/system_info.hbs  —  user-original (no upstream counterpart)
  · init.lua                          —  user-original (no upstream counterpart)
  · access/admin_only.lua             —  user-original (no upstream counterpart)
```

Custom pages and Lua files are user-original by definition (no
upstream counterpart). They never drift; `templates status` just
confirms they exist.

The shipped `example/init.lua` in the repo has this exact pattern
working — see [`example/init.lua`](https://github.com/dkluhs/crap-cms/blob/main/example/init.lua)
and [`example/templates/pages/system_info.hbs`](https://github.com/dkluhs/crap-cms/blob/main/example/templates/pages/system_info.hbs)
for the live reference.
