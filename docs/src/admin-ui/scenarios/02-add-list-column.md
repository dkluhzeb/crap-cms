# Scenario 2: Add a column to the collection list page

**Goal**: add a "Word count" column to the `posts` collection's
list view, computed from each post's `body` field.

**Difficulty**: easy to medium, depending on where the value lives.

**You'll touch**: either the collection definition plus a
`before_change` hook (Option A), or `templates/collections/items_table.hbs`,
`templates/collections/items_row.hbs` and a `before_render` hook
(Option B).

## Choose where the value lives

- **Option A — store it.** A `word_count` field kept fresh on every save.
  It is a real column: it shows up in the column picker, and it sorts and
  filters like any other field. No template override needed.
- **Option B — compute it at render time.** No schema change; a
  `before_render` hook adds the value to each list row and a template
  override renders it. Display only: it cannot be sorted or filtered.

Prefer Option A unless you have a reason to keep the value out of the
schema.

## Option A — a stored field

### Step 1 — add the field

In the `posts` definition, add a number field and make it a default list
column:

```lua
crap.collections.define("posts", {
    fields = {
        crap.fields.text({ name = "title", required = true }),
        crap.fields.textarea({ name = "body" }),
        crap.fields.number({
            name = "word_count",
            integer = true,
            admin = { readonly = true, description = "Updated on save" },
        }),
    },
    admin = {
        use_as_title = "title",
        list_columns = { "word_count", "created_at" },
    },
})
```

`list_columns` is the default column set; each user can still change their
own selection with the column picker. See the
[definition schema](../../collections/definition-schema.md) for details.

### Step 2 — keep it fresh

Register a `before_change` hook in `init.lua`. Write hooks read and write
the document through `ctx.data`:

```lua
-- <config_dir>/init.lua
crap.hooks.register("before_change", function(ctx)
    if ctx.collection ~= "posts" then return ctx end

    -- A partial update that doesn't touch `body` keeps the stored count.
    local body = ctx.data.body
    if type(body) ~= "string" then return ctx end

    local _, words = string.gsub(body, "%S+", "")
    ctx.data.word_count = words
    return ctx
end)
```

For an HTML rich text `body`, strip the markup first
(`body:gsub("<[^>]*>", " ")`) so tags aren't counted as words.

### Step 3 — restart

Schema and `init.lua` changes are read at startup: restart crap-cms. Posts
saved before the hook existed have no count until they are saved again —
backfill them once with a script that re-saves each post, or with
`crap.collections.update_many` from a one-off job.

## Option B — computed at render time

### Step 1 — extract the table and row templates

```
$ crap-cms templates extract collections/items_table.hbs collections/items_row.hbs
```

This drops both files into your config dir with source-version headers so
`templates status` tracks drift later.

### Step 2 — add the column markup

In `<config_dir>/templates/collections/items_table.hbs`, add a header cell
to the `<thead>` row, just before the last (actions) cell:

```hbs
<th>Word count</th>
```

In `<config_dir>/templates/collections/items_row.hbs`, add the matching
cell at the same position — before the last `<td>`:

```hbs
<td>{{this.word_count}}</td>
```

Each row the template sees is an entry of `ctx.docs`. A row carries only
what the list needs — `id`, `title_value`, `created_at`, `updated_at`, the
`cells` of the selected columns, and `thumbnail_url` for uploads — not the
document's fields. So `word_count` has to be put on the row by a hook.

### Step 3 — add the value to each row

```lua
-- <config_dir>/init.lua
crap.hooks.register("before_render", function(ctx, info)
    if info.page ~= "collection_items" or info.collection ~= "posts" then
        return
    end

    local ids = {}
    for _, row in ipairs(ctx.docs) do
        ids[#ids + 1] = row.id
    end
    if #ids == 0 then return end

    -- One query for the whole page, not one per row.
    local result = crap.collections.posts.find({
        where = { id = { ["in"] = ids } },
        select = { "body" },
        limit = #ids,
    })

    local counts = {}
    for _, doc in ipairs(result.documents) do
        local _, words = string.gsub(doc.body or "", "%S+", "")
        counts[doc.id] = words
    end

    for _, row in ipairs(ctx.docs) do
        row.word_count = counts[row.id]
    end
end)
```

Notes:

- **`before_render` is global** — it fires for every admin page render,
  so scope it with the second argument: `info.page` and `info.collection`
  say exactly which page is rendering.
- **Mutate or return** — Lua tables are references, so setting
  `row.word_count` is enough; returning `ctx` is optional.
- **Read-only, as the viewer** — on an authenticated admin page the hook
  can read the database, as the signed-in user with their access rules
  applied; writes are refused. See
  [`before_render`](../../hooks/lifecycle-events.md#before_render).

### Step 4 — restart

The extracted templates are new files and the hook lives in `init.lua`, so
restart crap-cms once. The overlay directory is scanned at startup: with
`[admin] dev_mode = true`, later **edits** to these two files show up on the
next request, but a newly added template file always needs a restart.

### Verifying

```
$ crap-cms templates status
  ✓ templates/collections/items_table.hbs   —  current
  ✓ templates/collections/items_row.hbs     —  current
```

Both overrides are tracked. After upgrading crap-cms, if upstream
restructures these templates, `templates status` flags them as `behind` —
run `templates diff collections/items_row.hbs` to see what to re-port.

## How the options relate to the column picker

A template-added `<th>` / `<td>` pair is invisible to the column picker: it
renders unconditionally, alongside whatever columns the picker manages.
That suits a computed display value. A stored field (Option A) is a
first-class column instead — users pick it, sort by it and filter on it.
