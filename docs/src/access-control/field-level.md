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

Field access belongs on a field that holds a value — a scalar, relationship,
group, array or blocks field. A layout wrapper (`row`, `collapsible`, `tabs`)
holds no value and its children are read and written as if the wrapper were
not there, so `access` (and `hidden`) on a wrapper is a **load error**: put the
rule on each child field, or wrap the children in a [group](../fields/group.md)
and put the rule on the group.

## How It Works

### Write Access (create/update)

Before a write operation, denied fields are **stripped from the input data**. The operation proceeds with the remaining fields. This means:

- On create: denied fields get their default value (or NULL)
- On update: denied fields keep their current value
- On **version restore**: denied fields are stripped from the snapshot too, so a restore keeps the live value of any field the caller may not write (it can't be used as a side channel to overwrite a write-locked field)

### A write never changes a field its writer cannot read

`read` and `update` are separate rules, so a user may be allowed to update a field they may not read. Such a user would be writing blind — an edit form cannot show them the value, so whatever it submits for the field (an empty input, an unchecked box, an empty list) would replace a value they never saw. So on **update** (a save, a draft save, a publish, a bulk update, a global update, a version restore, and the validate dry-run alike), every field the writer may **not read** is also kept as it is, exactly like a write-denied field:

- The `read` rule is judged against what the write replaces — `ctx.data` is that level (the row, for a field inside an array/blocks row), `ctx.document` the whole document — in the write's locale. For a save that is the stored document; for a **draft save** it is the pending draft (the content the draft edit form shows), or the stored document when no draft is pending. A field with no stored value is judged too, so it cannot be filled in blind either.
- It applies at every depth: a group sub-field, a field inside an array/blocks row (matched to its stored row by the row's `id`), and inside a group within a row. A row the write adds — or a block row whose type changed — has nothing stored and is judged against an **empty row**, so the writer cannot fill into a new row what it could not read on an empty one. A list nested inside a row has no row identity, so one holding a value the writer cannot read is kept as stored as a whole.
- A checkbox the writer cannot read keeps its stored state (an omitted checkbox would otherwise be stored as unchecked).
- A **publish** makes the pending draft live in every locale, and a **version restore** writes every locale of its snapshot: each locale's values are judged against the document as stored in that locale (`ctx.locale` set to it), a shared field once at the write's own locale, and a value the rule hides keeps its stored value there — neither cleared nor overwritten. A restore is partial for a restorer who may not read every field.
- To let a role change a field, let it read the field too.

The admin edit form renders no input for a field its viewer may not read, at any depth. A field inside array/blocks rows is judged **row by row**, as the read strip judges it (`ctx.data` = the row): a data-aware rule that hides it in some rows renders it — and saves it — in the rows the viewer may read, renders no input in the others (whose values the save leaves untouched), and the form's new-row template offers it only where an empty row allows it. A form re-rendered after a failed save judges each row by its `id` the same way.

Create is unaffected: a new document has no stored value to protect.

> **This is silent.** Stripping happens before validation and before any hook sees the data — the client gets no error or warning that fields were dropped, and the returned document reflects the stored state. If a client reports "I set field X but it didn't save", check whether field-level access is denying their role for that field.

### Read Access

After a query, denied fields are **stripped from the response**. The field still exists in the database, but the user doesn't see it.

Fields with the **top-level `hidden = true`** flag are also stripped from all
API responses, regardless of access rules. (`admin.hidden` is different — it
only hides the field from admin *forms*; the value is still returned by the
APIs.)

Field-level read access is independent of the [content view](overview.md#content-views) a document came from: the same field rules are applied per returned document whether it was read as published, draft, or trash content. Field access narrows *which fields* of an already-visible document the user sees; the collection-level view keys decide *which documents* are visible in the first place.

This also applies to **populated relationship and upload targets**: when a reference is expanded into the full related document, the target collection's own field-level read rules (and top-level `hidden` flags) are evaluated for the requesting user and denied fields are stripped from the embedded document — at any populate depth, including references nested inside groups, arrays, and blocks.

## Data-Aware Field Access

Field-access functions receive the **document data**, not just the user — the same `ctx.data` / `ctx.document` shape as a [field lifecycle hook](../hooks/field-hooks.md):

| Field | What it is |
|-------|------------|
| `ctx.data` | The field's **immediate level** — the row object for a field inside an array/blocks row, the group object for a field in a group, the whole document at the top level. Lets a rule gate on sibling values. |
| `ctx.document` | The **full document** the field belongs to: the stored document on read and update (on a draft save, the *published* row — a pending draft does not change who may write a field until it is published; on a version restore, the live row, not the snapshot), the incoming document on create. Stable as the check descends into rows, so a nested field can depend on a top-level value. |
| `ctx.user` | The requesting user (or `nil` when anonymous). |
| `ctx.collection` | The collection (or global) slug the field belongs to — lets a field-access function shared across collections branch on which one it is running for. |
| `ctx.operation` | `"read"`, `"create"`, or `"update"`. |
| `ctx.locale` | The content locale being accessed when localization is enabled, else `nil`. It is threaded on the standard collection/global read and write paths; unpublish, undelete and the user document returned by Login and Me use the default locale; live events leave it `nil`. When a pending draft is published or a version restored, the snapshot's shared fields are judged once at that write's locale and each **localized** field once per configured locale, with `ctx.locale` set to the locale under judgment — a rule that denies one locale keeps only that locale's column at its stored value. A read with `locale = "all"` judges a localized field at the document level — top-level or inside a row / collapsible / tabs wrapper — once per locale and returns only the locales its rule allows; a localized field inside a group, array or blocks field is judged once, at the default locale. Treat it as an optional hint — don't make a security decision depend on it being present. |

A field the rule denies is left exactly as stored — for a checkbox too, which the row write would otherwise read as "absent = unchecked".

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

A field-level rule is **boolean**: `true` keeps the field, `false`/`nil` (or a hook error) strips it. A returned filter table is treated as **allowed** — there is no row to constrain at field granularity — so express data-dependent field rules with `ctx.data` / `ctx.document` instead.

Because the rule is evaluated against each level, an array/blocks field rule runs **per row** — the field can be stripped from some rows and kept in others within the same document. Reads, writes (`create`/`update`), populated targets, version snapshots, and the live event stream all evaluate field access the same way, so a rule reading `ctx.data` / `ctx.document` behaves identically everywhere.

> **Performance.** When *any* field in a collection configures `access.read` (or `create`/`update`), that collection's field-access functions are evaluated **per returned document** on list reads (and per row for array/blocks rules). The work is gated to **zero** when no field configures the relevant access function — the common case pays nothing. On the live event stream, field-read rules are evaluated **per event per subscriber**; a rule that performs a CRUD query there is treated as denied (the live path has no transaction), so keep live-streamed collections' field-read rules pure (`ctx.user` / `ctx.data` / `ctx.document` only).

## Introspection

`crap.access.field_read_denied(collection [, document])` and `crap.access.field_write_denied(collection, operation [, document])` return the names of fields the current user cannot read/write — a denied timezone date is listed with its `<name>_tz` companion (`starts`, `starts_tz`; `items.starts`, `items.starts_tz` inside rows). They are for **UI gating**, not enforcement (enforcement is the per-document strip described above).

The optional `document` controls how data-dependent rules are evaluated:

- **Omit `document`** → a **categorical** check: `ctx.data` / `ctx.document` are `nil`, so the result reflects only role/`ctx.user`-based rules. A data-dependent rule that allows when the document is absent is reported as allowed. This is the right choice for a **create** form (no row exists yet) or a static field-visibility list.
- **Pass `document`** → a **data-aware** check: each rule is evaluated with `ctx.data` *and* `ctx.document` both set to the supplied table, so role- and document-level rules give the same answer the enforcement strip would. This is the right choice for an **edit** form, where the row being edited is known. Note one limit: introspection evaluates every rule at the **document level** (`ctx.data` = the whole document you pass), whereas the enforcement strip evaluates an array/blocks rule **per row** with `ctx.data` = that row. For a rule that keys on a row's own fields the introspection answer can therefore differ from the actual per-row strip — which is fine for UI gating but is why this helper is not an enforcement substitute.

```lua
-- Edit form: which fields can THIS user not edit on THIS record?
local locked = crap.access.field_write_denied("posts", "update", current_post)

-- Create form: categorical (no row yet)
local hidden = crap.access.field_write_denied("posts", "create")
```

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

## Filtering, Sorting, and Search

A field the caller cannot read is never a query oracle. On every read
surface (`find`, `count`, search, the admin list), a `where` filter or an
`order_by` on such a field is rejected with an access error. The bulk writes
`update_many` and `delete_many` apply the same rule to their `where` filter
on every surface (gRPC, Lua, the admin, and a queued bulk job — refused
when it is queued, before a `job_id` is issued) — their
`modified` / `deleted` / `skipped` counts and the `bulk_max_documents`
"matched N documents" error would otherwise count rows by the hidden value.
Contexts with `override_access` (MCP, Lua `override_access = true`) skip the check
as they skip every access rule. A field is unreadable here when it is:

- a field with `hidden = true` — always, for every caller;
- a field with an `access.read` rule — when the rule denies for this caller
  **without row data**. The rule is evaluated through the same read strip
  that guards responses, against a probe carrying `null` values; a rule that
  needs the row to decide therefore denies.

The check covers every field on the path, not just the first segment: a
group sub-field (`seo.secret` / `seo__secret`), an array row sub-field
(`items.secret`), a block sub-field (`content.body`), and anything nested
inside a row (`items.sizes.label`), and an array, blocks or has-many field
inside a group (`seo.links.url` / `seo__links.url`, `seo.tags.id`) are
rejected when their own rule — or the rule of any group, array or blocks field
on the way — denies, or when any of them is `hidden`, whichever way the group
part is spelled. A block path
does not name its block type, so it is rejected when the field's rule denies
in **any** block type that holds a field of that name. A relationship's
`.id` path is judged by the relationship field's own rule.

Full-text search follows the same idea at index time: hidden fields and
fields with an `access.read` rule are excluded from the default searchable
set (the index is shared by every reader). An operator may still list a
read-gated field in `list_searchable_fields` explicitly — that is a choice
to make it searchable by everyone who can search the collection. A hidden
field listed there is ignored with a startup warning.
