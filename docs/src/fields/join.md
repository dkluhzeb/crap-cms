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
tabs wrapper. Setting
`required`, `localized`, or `required_locales` on a join is also a
**hard load error** (a join is virtual and read-only, so those flags
are meaningless — they are rejected rather than silently ignored).

## Behavior

- **No database column** — join fields are virtual. No migration, no storage.
- **Read-only** — displayed in the admin UI but not editable. No form input is rendered.
- **No validation** — since no data is submitted, validation is skipped entirely.
- **Admin UI** — shows a list of linked documents (at most `limit`) with clickable links to edit each one, and how many there are — "3 of 25 items" when more exist than it lists (counted among the documents the viewer may see; omitted when the viewer may not read every referencing value, so the count never reveals more than the list). Displays "No related items" when empty.
- **API responses** — at `depth >= 1`, join fields return an array of document objects from the target collection — the first `limit` in the target's default order, each populated one level less deeply (a join counts against the read's depth like a relationship). At `depth = 0`, join fields are omitted (no stored value). A join in a layout wrapper or a group is populated like a top-level one. A failed lookup fails the read's population rather than reading as an empty list.
- **Drafts** — with `draft = true`, a listed document shows its pending draft when the reader may read the target's drafts, as a populated relationship does (see [Population Depth](../relationships/population-depth.md)).
- **Access** — a join lists only target documents the viewer may read, stripped of the fields the viewer may not read. It also lists only documents whose `on` field the viewer may read: being listed says what that field holds, so a document whose `on` field is `hidden`, or denied by its `access.read` rule (judged per document, on its own data), is left out of the list and of the admin count.

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
