# Lua API

The `crap` global table is the entry point for all CMS operations in Lua. It's available in `init.lua`, collection definitions, and hook functions.

## Namespace

| Namespace | Description |
|-----------|-------------|
| `crap.collections` | Collection definition and CRUD operations |
| `crap.globals` | Global definition and get/update operations |
| `crap.hooks` | Global hook registration |
| `crap.log` | Structured logging |
| `crap.util` | Utility functions |

## CRUD Availability

CRUD functions (`crap.collections.find`, `.create`, `.update`, `.delete`, `crap.globals.get`, `.update`) are **only available inside hooks with transaction context**:

- `before_validate` hooks — Yes
- `before_change` hooks — Yes
- `before_delete` hooks — Yes
- `on_init` hooks — Yes
- `after_change` hooks — No
- `after_read` hooks — No
- `after_delete` hooks — No
- Collection definition files — No

Calling CRUD functions outside of transaction context results in an error:

```
crap.collections CRUD functions are only available inside hooks
with transaction context (before_change, before_delete, etc.)
```

## Two Lua VMs

Crap CMS uses two Lua VMs:

1. **Startup VM** — loads collection/global definitions and runs `init.lua`. Used only during initialization.
2. **HookRunner VM** — handles runtime hook execution. Gets its own copy of the `crap.*` API with CRUD functions registered.

Both VMs have the config directory on their package path, so `require("hooks.posts")` works in both.
