# Display Conditions

Display conditions let you show or hide fields in the admin UI based on the values of other fields. This is useful for context-dependent forms — for example, showing a URL field only when the post type is "link".

## Configuration

Add `admin.condition` to a field definition, referencing a Lua function:

```lua
crap.fields.text({
    name = "external_url",
    admin = {
        condition = "hooks.posts.show_external_url",
    },
}),
```

The condition references a Lua function using the standard hook ref format (`hooks.<collection>.<name>`). The function is called as `function(form_data, ctx)` and returns **either** a condition table (client-side) or a boolean (server-side).

The second `ctx` argument carries `collection`, `operation` (`"create"` or `"update"`), `user` (the admin), `ui_locale`, `locale`, and `options` — so a field can be shown only to certain users, only when editing, etc. It's optional: a `function(form_data)` that ignores it keeps working. A condition that uses `ctx` must return a **boolean** (server-evaluated); the client-side condition-table form can't see `ctx`.

`ctx.options` carries the per-config table when the condition is declared as `condition = { ref = "hooks.show_field", options = {...} }` (it's `nil` for a bare-string ref) — letting one condition function be reused across fields with different parameters. See [Per-Config Options](../../hooks/hook-context.md#per-config-options-ctxoptions).

`ctx.operation` is correct on both the initial form render and live re-evaluation (the form sends it). `ctx.locale` is the editor's content locale on the initial render but is `nil` during live re-evaluation as you type (the live endpoint is locale-agnostic) — gate defensively if you branch on it.

The `data` parameter is typed per-collection (`crap.data.Posts`, `crap.global_data.SiteSettings`) for IDE autocomplete. The type generator emits these types automatically.

### What `data` holds

A condition sees the form's values in **one shape, wherever it is evaluated** — the edit form's first render, the create form, the re-render after a failed save, the live re-evaluation as you type, and the browser's evaluation of a condition table. The values are decoded the way a save stores them:

| Field | Value in `data` |
|-------|-----------------|
| Checkbox | `true` / `false` (an unchecked box is `false`) |
| Number | a number (`5`, not `"5"`); numbers compare by value, so `5` equals `5.0` |
| Text, textarea | the text, NFC-composed |
| Email | the address trimmed, lowercased and NFC-composed |
| Any input left empty | `nil` |
| A `has_many` field (select, text, number, relationship) | a list |
| A field inside a group | nested under the group: `data.seo.title` |
| Array / blocks | a list of rows |
| Date | its stored form, a UTC instant: a day is noon UTC (`2026-01-15` → `"2026-01-15T12:00:00.000Z"`), a date and time is UTC (`2026-01-15T09:30` → `"2026-01-15T09:30:00.000Z"`) — or, for a field with `timezone = true`, the time in the chosen zone converted to UTC. A time alone or a month keeps its text (`"14:30"`, `"2026-01"`) |

On the **create form** the condition sees each field's `default_value` — the values the inputs render with — so a field shown by a default select option starts visible. After a failed save it sees the values that were submitted.

A field you may not read is not in `data` (it is not in the form either).

Use `crap-cms make hook` with `--type condition` to scaffold condition hooks:

```bash
crap-cms -C ./config make hook show_external_url \
    -t condition -c posts -l table -F post_type
```

## Condition Functions

### Client-Side (Condition Table)

When the function returns a **table**, it is serialized to JSON and embedded in the HTML. JavaScript evaluates it instantly on field changes — no server round-trip.

```lua
-- hooks/posts/show_external_url.lua
return crap.collections.posts.condition(function(data)
    -- data is typed crap.data.Posts; data.post_type narrows to its select union
    return { field = "post_type", equals = "link" }
end)
```

The `crap.collections.posts.condition(fn)` factory narrows `data` to
`crap.data.Posts` for body-level autocomplete. See
[`crap.collections.<slug>.condition`](../../lua-api/collections.md)
and [`crap.any.display_condition`](../../lua-api/typing-factories.md)
for the generic equivalent.

### Server-Side (Boolean)

When the function returns a **boolean**, the field visibility is re-evaluated on the server via a debounced fetch (300ms delay after the last input change). Use this for complex logic that can't be expressed as a simple condition table.

```lua
-- hooks/posts/show_premium_options.lua
return crap.collections.posts.condition(function(data)
    -- Complex logic that needs server-side evaluation
    local tags = data.tags or {}
    for _, tag in ipairs(tags) do
        if tag == "premium" then return true end
    end
    return false
end)
```

> **Performance tip:** Prefer condition tables over booleans whenever possible. Tables evaluate instantly in the browser; booleans require a server round-trip on every field change.

## Condition Table Operators

`field` names a top-level field by its name, and a field inside a group by its dotted path (`"seo.title"`) or its form name (`"seo__title"`) — both resolve the same. A field with no value compares as `nil`. Values compare by type: a checkbox condition is `equals = true`, a number condition `equals = 5`.

| Operator | Example | Description |
|----------|---------|-------------|
| `equals` | `{ field = "type", equals = "link" }` | Exact match |
| `not_equals` | `{ field = "type", not_equals = "draft" }` | Not equal |
| `in` | `{ field = "type", ["in"] = {"link", "video"} }` | Value in list |
| `not_in` | `{ field = "type", not_in = {"a", "b"} }` | Value not in list |
| `is_truthy` | `{ field = "has_image", is_truthy = true }` | Non-empty, non-nil, non-false |
| `is_falsy` | `{ field = "has_image", is_falsy = true }` | Empty, nil, or false |

## Multiple Conditions (AND)

Return an array of condition tables to require all conditions to be true:

```lua
-- hooks/posts/show_advanced.lua
return crap.collections.posts.condition(function(data)
    return {
        { field = "post_type", not_equals = "link" },
        { field = "excerpt", is_truthy = true },
    }
end)
```

## How It Works

### Page Load

1. The server calls the Lua condition function with the current document data
2. Based on the return type:
   - **Table:** serialized as a `data-condition` JSON attribute on the field wrapper; initial visibility computed server-side
   - **Boolean:** result sets initial visibility; function reference stored as `data-condition-ref`
3. Fields with `false` conditions render with `display: none` (no flash of content)

### Client-Side Reactivity (Condition Tables)

When the user changes a form field:
1. JavaScript reads the `data-condition` JSON from each conditional field
2. Evaluates the condition against current form values
3. Shows or hides the field instantly

### Server-Side Reactivity (Boolean Functions)

When the user changes a form field:
1. JavaScript debounces for 300ms
2. POSTs the form's current values to `/admin/collections/{slug}/evaluate-conditions` (`/admin/globals/{slug}/…` for a global), keyed by each field's form name (`seo__title` for `title` inside group `seo`), with the edited document's `document_id` (none on a create form) and the editor `locale`
3. Server decodes the values into the condition data described above — against the fields the edit form rendered for its viewer, so a field the viewer may not read (which has no input) is `nil`, as on the first render and in the browser — and calls each boolean condition function with `ctx.locale` set to the editor locale
4. Response updates field visibility

Custom inputs (relationship, upload, tags) and array/blocks row changes (add, remove, reorder) re-evaluate conditions too.

## Fields Inside Array and Blocks Rows

`admin.condition` is **not supported** on a field inside an array or blocks row, and the server refuses to start with one: a condition judges the whole form, and a row has no scope of its own, so such a condition could never apply. Put the condition on the array/blocks field itself, or move the field out of the row. A condition on a field inside a group, row, collapsible or tab works as on a top-level field.

## Sidebar Fields

Display conditions work on fields in any position, including sidebar fields (`admin.position = "sidebar"`).

## Failure Modes

- If a condition function throws an error, returns a malformed condition table, or returns anything but a boolean, a table or `nil`, the field is **hidden** (fail closed) and the server logs a warning
- If the condition returns `nil`, the field is **visible** (no condition)
- On page load, fields are hidden server-side before rendering (no flash)
- A hidden field's inputs are still submitted with the form

## Complexity Limits

There are **no built-in depth, length, or array-size limits** on condition tables returned from Lua. The evaluator (`ConditionExpr::evaluate` in `src/core/condition.rs`) recurses through arrays of conditions until it bottoms out, and each leaf object is a single AND clause.

In practice, keep condition tables small and flat:

- A single object (one operator, one field), or
- An array of objects (AND-combined).

Deeply nested or runaway-large tables will be evaluated in full both server-side (initial render) and client-side (on every form change) — so keeping them simple is a performance and readability choice, not a hard requirement. Conditions that need richer logic (loops, lookups, multi-field reasoning) are better expressed as boolean condition functions.
