# Field-Level Access Control

Field-level access controls which fields are visible or writable per-user.

## Configuration

```lua
crap.fields.select({
    name = "status",
    access = {
        read = "hooks.access.everyone",
        create = "hooks.access.admin_only",
        update = "hooks.access.admin_only",
    },
    -- ...
})
```

| Property | Controls |
|----------|----------|
| `read` | Whether the field appears in API responses |
| `create` | Whether the field can be set on create |
| `update` | Whether the field can be changed on update |

Omitted properties default to allowed (no restriction).

## How It Works

### Write Access (create/update)

Before a write operation, denied fields are **stripped from the input data**. The operation proceeds with the remaining fields. This means:

- On create: denied fields get their default value (or NULL)
- On update: denied fields keep their current value

> **This is silent.** Stripping happens before validation and before any hook sees the data — the client gets no error or warning that fields were dropped, and the returned document reflects the stored state. If a client reports "I set field X but it didn't save", check whether field-level access is denying their role for that field.

### Read Access

After a query, denied fields are **stripped from the response**. The field still exists in the database, but the user doesn't see it.

Fields with `admin.hidden = true` are also stripped from all API responses, regardless of access rules.

Field-level read access is independent of the [content view](overview.md#content-views) a document came from: the same field rules are applied per returned document whether it was read as published, draft, or trash content. Field access narrows *which fields* of an already-visible document the user sees; the collection-level view keys decide *which documents* are visible in the first place.

This also applies to **populated relationship and upload targets**: when a reference is expanded into the full related document, the target collection's own field-level read rules (and `admin.hidden` flags) are evaluated for the requesting user and denied fields are stripped from the embedded document — at any populate depth, including references nested inside groups, arrays, and blocks.

## Data-Aware Field Access

Field-access functions receive the **document data**, not just the user — the same `ctx.data` / `ctx.document` shape as a [field lifecycle hook](../hooks/field-hooks.md):

| Field | What it is |
|-------|------------|
| `ctx.data` | The field's **immediate level** — the row object for a field inside an array/blocks row, the group object for a field in a group, the whole document at the top level. Lets a rule gate on sibling values. |
| `ctx.document` | The **full document** the field belongs to (the stored document on read/update, the incoming document on create). Stable as the check descends into rows, so a nested field can depend on a top-level value. |
| `ctx.user` | The requesting user (or `nil` when anonymous). |
| `ctx.collection` | The collection (or global) slug the field belongs to — lets a field-access function shared across collections branch on which one it is running for. |
| `ctx.operation` | `"read"`, `"create"`, or `"update"`. |

This makes rules like these possible:

```lua
-- Hide `salary` unless the document is published.
function M.only_when_published(ctx)
    return ctx.document ~= nil and ctx.document.status == "published"
end

-- In an array of line items, hide `cost_price` on rows whose `kind` is "public".
function M.hide_cost_on_public_rows(ctx)
    return not (ctx.data ~= nil and ctx.data.kind == "public")
end
```

Because the rule is evaluated against each level, an array/blocks field rule runs **per row** — the field can be stripped from some rows and kept in others within the same document. Reads, writes (`create`/`update`), populated targets, version snapshots, and the live event stream all evaluate field access the same way, so a rule reading `ctx.data` / `ctx.document` behaves identically everywhere.

> **Performance.** When *any* field in a collection configures `access.read` (or `create`/`update`), that collection's field-access functions are evaluated **per returned document** on list reads (and per row for array/blocks rules). The work is gated to **zero** when no field configures the relevant access function — the common case pays nothing. On the live event stream, field-read rules are evaluated **per event per subscriber**; a rule that performs a CRUD query there is treated as denied (the live path has no transaction), so keep live-streamed collections' field-read rules pure (`ctx.user` / `ctx.data` / `ctx.document` only).

## Introspection

`crap.access.field_read_denied(collection)` and `crap.access.field_write_denied(collection, operation)` return the names of fields the current user cannot read/write. These are evaluated **without document context** (`ctx.data` / `ctx.document` are `nil`), so they report a field's *categorical* (data-independent) denials — a data-dependent rule that allows when the document is absent is reported as allowed. Use them for UI gating, not as the enforcement path (enforcement is the per-document strip described above).

## Example

```lua
-- hooks/access.lua
local M = {}

-- Only admins can see the internal_notes field
function M.admin_read(ctx)
    return ctx.user ~= nil and ctx.user.role == "admin"
end

-- Only admins can change the status field
function M.admin_write(ctx)
    return ctx.user ~= nil and ctx.user.role == "admin"
end

return M
```

```lua
-- In collection definition
crap.fields.textarea({
    name = "internal_notes",
    access = {
        read = "hooks.access.admin_read",
    },
}),
crap.fields.select({
    name = "status",
    access = {
        update = "hooks.access.admin_write",
    },
    -- ...
}),
```

## Error Behavior

If a field access function throws an error, the field is treated as **denied** (fail-closed) and a warning is logged.
