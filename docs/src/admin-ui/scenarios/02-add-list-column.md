# Scenario 2: Add a column to the collection list page

**Goal**: add a "Word count" column to the `posts` collection's
list view, computed from each post's body field.

**Difficulty**: medium. Two files to touch: a template overlay and
optionally a Lua hook (if the value isn't already in the document).

**You'll touch**: `templates/collections/items_table.hbs`,
`templates/collections/items_row.hbs`, optionally a
`before_render` hook.

## Approach

The list view renders rows via `items_row.hbs` with column cells in
the order specified by `items_table.hbs`'s header. To add a column,
override both partials and add a `<th>` / `<td>` pair. The cell
content can pull from any field in the document — or from a value
you compute in a `before_render` hook and stash on the page context.

## Step 1 — extract the table + row templates

```
$ crap-cms templates extract collections/items_table.hbs collections/items_row.hbs
```

This drops both files into your config dir with source-version
headers so `templates status` tracks drift later.

## Step 2 — add the column

Open `<config_dir>/templates/collections/items_table.hbs` and add a
header cell to the `<thead>`:

```hbs
<thead>
  <tr>
    {{!-- existing cells --}}
    <th>Word count</th>
  </tr>
</thead>
```

Open `<config_dir>/templates/collections/items_row.hbs` and add a
matching cell:

```hbs
{{!-- existing cells --}}
<td>{{this.word_count}}</td>
```

That's it for the markup. The remaining question is where
`{{this.word_count}}` comes from.

## Step 3 — provide the data

Two options depending on how the word count is stored:

### Option A — already a field on the collection

If `posts` has a `word_count` field (e.g., updated by a `before_change`
hook on save), the value is already in the row data. The template
renders it directly — nothing more to do. This is the cleanest
approach: the count is also queryable, sortable, and filterable.

```lua
-- <config_dir>/init.lua  — keep word_count fresh on every save.
crap.hooks.register("before_change", function(ctx)
  if ctx.collection ~= "posts" then return ctx end
  if ctx.operation ~= "create" and ctx.operation ~= "update" then return ctx end

  local body = ctx.input.body or ""
  ctx.input.word_count = select(2, string.gsub(body, "%S+", ""))
  return ctx
end)
```

Pair this with a `word_count` integer field in the `posts`
collection schema. Existing documents need a one-off migration to
backfill the column for rows saved before the hook landed.

### Option B — computed at render time

If you don't want a schema field — say, the count is too cheap to
denormalize, or you want to keep the schema clean — compute it in
a `before_render` hook:

```lua
-- <config_dir>/init.lua
crap.hooks.register("before_render", function(ctx, info)
  if info.collection ~= "posts" or info.page ~= "collection_items" then
    return
  end

  for _, doc in ipairs(ctx.items) do
    doc.word_count = select(2, string.gsub(doc.body or "", "%S+", ""))
  end

  return ctx
end)
```

Notes:

- **`before_render` is global** — it fires for every admin page render,
  so scope it with the second argument. `info.page` and
  `info.collection` say exactly which page is rendering; bailing out
  early is one line.
- **Mutate or return** — Lua tables are pass-by-reference, so
  mutating `ctx.items[i].word_count` is enough. Returning `ctx`
  explicitly is conventional and harmless.
- **Cost** — the hook runs in the request path. For 50 rows × a
  word-count regex, this is a sub-millisecond cost. For more
  expensive enrichments, prefer Option A.
- **Need to look something up?** The hook has read-only database
  access on authenticated pages, so an enrichment that needs a query
  (`crap.collections.authors.find_by_id(doc.author)`) works here too —
  it just runs once per page render, per row. Denormalizing with
  Option A stays the better answer when the value is hot.

## Step 4 — restart (or rely on dev mode)

If you're running with `[admin] dev_mode = true`, the templates are
reloaded per-request — refresh `/admin/collections/posts` and the
new column appears.

If `dev_mode = false`, restart crap-cms.

## Step 5 — how this relates to the column picker

A template-added `<th>` / `<td>` pair is invisible to the column
picker: it renders unconditionally, alongside whatever columns the
picker manages. That's the right tool for a **computed** value like
word count, which isn't a document field.

For columns that *are* document fields, you don't need a template
override at all: set the collection's default column set in its Lua
definition —

```lua
admin = {
    list_columns = { "title", "word_count_field", "_status", "created_at" },
},
```

— and users can adjust their personal selection via the column
picker (per-user selections override the default; with no
`list_columns` the built-in default is `_status` — if the collection
has drafts — plus `created_at`). See the
[definition schema](../../collections/definition-schema.md) for
`list_columns` details.

## What this scenario *doesn't* cover

- **Sorting** by the new column — that requires the column to be a
  real DB field, not a hook-computed value.
- **Filtering** by the new column — same.
- **Editing** the value — same. If you want round-trip editing, add
  a real field to the collection schema and skip the hook.

For one-off display columns (counts, derived values, indicators),
the hook-then-render pattern is the cleanest. For first-class
columns, add a schema field.

## Verifying

```
$ crap-cms templates status
  ✓ templates/collections/items_table.hbs   —  current
  ✓ templates/collections/items_row.hbs     —  current
```

Both your overrides are tracked. After upgrading crap-cms, if
upstream renames or restructures these templates, `templates status`
will flag them as `behind` — run `templates diff
collections/items_row.hbs` to see what to re-port.
