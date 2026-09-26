# Join

A virtual, read-only field that displays documents from another collection that reference the current document. No data is stored — results are computed at read time by querying the target collection.

## Lua Definition

```lua
crap.fields.join({ name = "posts", collection = "posts", on = "author" })
```

This reads as: "Show me the documents in the `posts` collection where `posts.author` equals this document's ID" — at most `limit` of them (default 10).

## Properties

| Property | Type | Required | Description |
|----------|------|----------|-------------|
| `name` | string | yes | Field name (display only, no column created) |
| `type` | `"join"` | yes | Must be `"join"` |
| `collection` | string | yes | Target collection slug to query |
| `on` | string | yes | Field on the target collection that references this collection — a has-one relationship or upload field |
| `limit` | integer | no | Most documents listed per document (default `10`, or `[pagination] max_limit` when that is lower). At least 1, at most `[pagination] max_limit` — a larger value is a load error. |
| `admin` | table | no | Admin display options (`label`, `description`, …) |
| `hidden` | boolean | no | Strip the join from every read response and the admin form |
| `access` | table | no | `read` only — a function ref deciding who sees the join (`create` / `update` are load errors) |
| `hooks` | table | no | `after_read` only — shapes the listed documents on read (the write-side hooks are load errors) |

Both `collection` and `on` are required non-empty strings — a missing,
wrong-typed, or empty value is a hard error at load time, and so is a
`collection` that is not a defined collection. `on` must name a
**has-one, single-target** `relationship` or `upload` field at the top level
of the target collection (layout wrappers — row, collapsible, tabs — are
transparent) whose target is the collection that owns the join; anything else
— an unknown name, a field referencing another collection, a has-many or
polymorphic relationship, a field inside a group — is a load error, since the
join would never list anything. For the same reason a join is rejected in a
global: nothing can reference a global. A join is also rejected anywhere
inside an **array or blocks row** (including a group inside a row): it lists
the documents referencing the whole document, so every row would repeat the
same list. Place it at the top level, in a group, or in a row / collapsible /
tabs wrapper.

A join accepts only the keys in the table above. It stores no value and no
write ever carries it, so every other field key — `required`,
`required_when`, `unique`, `index`, `localized`, `required_locales`,
`validate`, `default_value`, `mcp` — is a **hard load error** when present,
even set to `false`, and so are `access.create` / `access.update` and the
`before_validate` / `before_change` / `after_change` hooks. They could never
have an effect, so they are refused rather than silently ignored.

## Behavior

- **No database column** — join fields are virtual. No migration, no storage.
- **Read-only** — displayed in the admin UI but not editable. No form input is rendered.
- **No validation** — since no data is submitted, validation is skipped entirely.
- **Admin UI** — shows a list of linked documents (at most `limit`) with clickable links to edit each one, and how many there are — "3 of 25 items" when more exist than it lists (counted among the documents the viewer may see; omitted when the viewer may not read every referencing value, so the count never reveals more than the list). Displays "No related items" when empty.
- **API responses** — at `depth >= 1`, join fields return an array of document objects from the target collection — the first `limit` in the target's default order, each populated one level less deeply (a join counts against the read's depth like a relationship). At `depth = 0`, join fields are omitted (no stored value). A join in a layout wrapper or a group is populated like a top-level one. A failed lookup fails the read's population rather than reading as an empty list.
- **Drafts** — with `draft = true`, a listed document shows its pending draft when the reader may read the target's drafts, as a populated relationship does (see [Population Depth](../relationships/population-depth.md)) It is listed under the document its **stored** `on` value names — the value the lookup matched — even when its pending draft points `on` elsewhere; the draft's new target lists it once the draft is published.
- **Access** — a join lists only target documents the viewer may read, stripped of the fields the viewer may not read. It also lists only documents whose `on` field the viewer may read: being listed says what that field holds, so a document whose `on` field is `hidden`, or denied by its `access.read` rule (judged per document, on its own data), is left out of the list and of the admin count. The left-out documents do not use up the `limit`: the join reads on down the target's order until it lists `limit` documents the viewer may see or none are left, so a reader whose newest referencing documents are all hidden from them still gets the next ones. On a join whose `on` field has a per-document read rule this may read more rows than `limit`.

## Example

Given an `authors` collection and a `posts` collection where each post has a `relationship` field called `author`:

```lua
-- collections/authors.lua
crap.collections.define("authors", {
    fields = {
        crap.fields.text({ name = "name", required = true }),
        crap.fields.join({ name = "posts", collection = "posts", on = "author" }),
    },
})
```

When editing an author, the "posts" join field displays all posts where `posts.author` equals the current author's ID.
