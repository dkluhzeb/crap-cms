# Lua API

The `crap` global table is the entry point for all CMS operations in Lua. It's available in `init.lua`, collection definitions, and hook functions.

## Namespace

| Namespace | Description |
|-----------|-------------|
| `crap.collections` | Collection definition, CRUD operations, and per-collection typing factories (`crap.collections.<slug>.{hook,field_hook,condition,access,auth_strategy,row_label}`) |
| `crap.globals` | Global definition, get/update, and per-global typing factories |
| `crap.any` | Cross-collection typing factories — `crap.any.{collection_hook,field_hook,access,auth_strategy,job_handler,row_label,display_condition}` |
| `crap.fields` | Field factory functions (`crap.fields.text()`, etc.) |
| `crap.hooks` | Global hook registration |
| `crap.jobs` | Job definition |
| `crap.log` | Structured logging |
| `crap.util` | Utility functions |
| `crap.auth` | Password hashing and verification (Argon2id) |
| `crap.env` | Read-only environment variable access |
| `crap.http` | Outbound HTTP requests (blocking) |
| `crap.config` | Read-only access to crap.toml values |
| `crap.locale` | Locale configuration queries |
| `crap.email` | Send/queue email via the configured provider; register a custom provider (`crap.email.register`) |
| `crap.storage` | Register a custom upload-storage backend (`crap.storage.register`, for `[upload] storage = "custom"`) |
| `crap.cache` | Register a custom cross-request cache backend (`crap.cache.register`, for `[cache] backend = "custom"`) — see [crap.cache](cache.md) |
| `crap.uploads` | Signed serve-URL minting for private media (`crap.uploads.sign_url`) — see [Uploads](../uploads/client-uploads.md#signed-urls) |
| `crap.tx` | Transaction-outcome effects (`crap.tx.on_commit`, `crap.tx.on_rollback`) — see [Transaction Access](../hooks/transaction-access.md#transaction-outcome-effects) |
| `crap.crypto` | Cryptographic utilities (HMAC, random bytes, hashing) |
| `crap.schema` | Runtime schema introspection |
| `crap.richtext` | Custom rich text node registration |
| `crap.access` | Re-evaluate a collection/global access gate from Lua (`crap.access.check`) — see [Access Control](../access-control/overview.md#programmatic-access-checks) |
| `crap.json` | JSON encode/decode — see [crap.json](json.md) |
| `crap.routes` | Custom HTTP endpoints (`crap.routes.register`, `crap.routes.list`) — see [crap.routes](routes.md) |
| `crap.pages` | Custom admin pages (`crap.pages.register`) — see [Custom Pages](../admin-ui/scenarios/05-custom-page.md) |
| `crap.template_data` | Extra data injected into admin templates (`crap.template_data.register`) — see [Template Data](../admin-ui/scenarios/04-dashboard-widget.md) |

## Typed hook factories

Every callable a Lua user writes — collection hooks, field hooks,
access functions, auth strategies, job handlers, row labels, display
conditions — should be wrapped in a **typing factory** so the editor
can infer the parameter types of the function body.

```lua
-- Per-collection: ctx narrows to crap.hook.Posts
return crap.collections.posts.hook(function(context)
    -- context.data.title, context.operation, etc. all typed
    return context
end)

-- Per-collection per-field: value narrows to the field's type
return crap.collections.posts.field_hook("title", function(value, context)
    -- value: string (from posts.title's text field)
    return value:lower()
end)

-- Cross-collection generic (hook that runs on multiple collections)
return crap.any.field_hook(function(value, context)
    -- value: any, context: crap.FieldHookContext
    return value
end)

-- Access function (uniform context across all collections)
return crap.any.access(function(context)
    return context.user ~= nil
end)
```

The factories are pure pass-throughs at runtime (`f(fn) = fn`); they
exist solely so LuaLS can propagate the typed parameter slot into
your function body's locals. Requires `"type.inferParamType": true`
in your `.luarc.json` — included in the `init` scaffold by default.

See [`crap.collections`](collections.md) and [`crap.globals`](globals.md)
for the full per-slug factory surface; [`crap.any`](typing-factories.md)
for the cross-collection helpers.

## Two ways to address a collection

Every defined collection is reachable in two forms:

- **Per-collection accessor (recommended for static slugs):**
  `crap.collections.<slug>.find(query)`, `crap.collections.<slug>.create(data)`,
  …  Slug is bound, return values fully typed for editor autocomplete.
- **Slug-keyed dispatch (required for dynamic slugs):**
  `crap.collections.find(slug, query)`, `crap.collections.create(slug, data)`,
  …  Same semantics, slug passed at call time. Use when the collection
  varies at runtime (auth strategies, generic plugins, migration loops).

Globals follow the same pattern: `crap.globals.<slug>.{get,update}(...)`
versus `crap.globals.{get,update}(slug, ...)`.

See [`crap.collections`](collections.md) and [`crap.globals`](globals.md)
for the full surface of each accessor.

## CRUD Availability

CRUD functions (whether called as `crap.collections.<slug>.find(...)` or
`crap.collections.find(slug, ...)`, and the `crap.globals` equivalents) are
available in every context that carries a database connection:

**Conn-mode** (shares the parent operation's transaction):

- `before_validate` / `before_change` / `before_delete` hooks — Yes
- `after_change` / `after_delete` hooks — Yes (same transaction via `run_hooks_with_conn`)
- `on_init` hooks — Yes (one shared startup transaction)
- Access-control functions — Yes (on the calling operation's connection)
- Custom auth strategies — Yes (on the request's connection; see
  [Transaction & CRUD Access](../hooks/transaction-access.md) for the
  rollback caveat)

**Pool-mode** (opens its own transaction per CRUD call batch):

- Job handlers — Yes
- Custom route handlers — Yes
- `crap.transaction(fn)` — Yes (explicit transaction block)

**No CRUD:**

- `before_read` / `after_read` hooks — No (no connection; `after_read` is a
  per-document transform)
- `before_render` / `before_broadcast` hooks — No
- Collection definition files — No (definitions load before the DB is ready)

Calling CRUD functions anywhere else results in an error:

```
crap.collections CRUD functions need a database context — call
them inside a lifecycle hook (before_change, before_delete,
etc.), a job handler, a custom route handler, or wrap the call
in crap.transaction(fn)
```

## Lua VM Architecture

Crap CMS uses two stages of Lua execution:

1. **Startup VM** — a single VM that loads collection/global definitions and runs `init.lua`. Used only during initialization, then discarded.
2. **HookRunner pool** — an elastic pool of Lua VMs for runtime hook execution. It pre-warms `hooks.vm_pool_size` VMs and grows on demand up to `hooks.max_vm_pool_size` as concurrency rises. Each VM gets its own copy of the `crap.*` API with CRUD functions registered.

All VMs have the config directory on their package path, so `require("hooks.posts")` works in both stages.
