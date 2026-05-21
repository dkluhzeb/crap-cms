# Lua API

The `crap` global table is the entry point for all CMS operations in Lua. It's available in `init.lua`, collection definitions, and hook functions.

## Namespace

| Namespace | Description |
|-----------|-------------|
| `crap.collections` | Collection definition and CRUD operations |
| `crap.globals` | Global definition and get/update operations |
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
| `crap.email` | Send email via configured SMTP |
| `crap.crypto` | Cryptographic utilities (HMAC, random bytes, hashing) |
| `crap.schema` | Runtime schema introspection |
| `crap.richtext` | Custom rich text node registration |

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
`crap.collections.find(slug, ...)`, and `crap.globals.<slug>.{get,update}(...)`
or the slug-keyed equivalents) are **only available inside hooks with
transaction context**:

- `before_validate` hooks — Yes
- `before_change` hooks — Yes
- `before_delete` hooks — Yes
- `after_change` hooks — Yes (runs inside the same transaction via `run_hooks_with_conn`)
- `after_delete` hooks — Yes (runs inside the same transaction via `run_hooks_with_conn`)
- `after_read` hooks — No (no transaction)
- `before_read` hooks — No (no transaction)
- Collection definition files — No

Calling CRUD functions outside of transaction context results in an error:

```
crap.collections CRUD functions are only available inside hooks
with transaction context (before_change, before_delete, etc.)
```

## Lua VM Architecture

Crap CMS uses two stages of Lua execution:

1. **Startup VM** — a single VM that loads collection/global definitions and runs `init.lua`. Used only during initialization, then discarded.
2. **HookRunner pool** — a pool of Lua VMs for runtime hook execution (size configured via `hooks.vm_pool_size`). Each VM gets its own copy of the `crap.*` API with CRUD functions registered.

All VMs have the config directory on their package path, so `require("hooks.posts")` works in both stages.
