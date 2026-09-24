# Join

A virtual, read-only field that displays documents from another collection that reference the current document. No data is stored — results are computed at read time by querying the target collection.

## Lua Definition

```lua
crap.fields.join({ name = "posts", collection = "posts", on = "author" })
```

This reads as: "Show me all documents in the `posts` collection where `posts.author` equals this document's ID."

## Properties

| Property | Type | Required | Description |
|----------|------|----------|-------------|
| `name` | string | yes | Field name (display only, no column created) |
| `type` | `"join"` | yes | Must be `"join"` |
| `collection` | string | yes | Target collection slug to query |
| `on` | string | yes | Field on the target collection that references this collection — a has-one relationship or upload field |

Both `collection` and `on` are required non-empty strings — a missing,
wrong-typed, or empty value is a hard error at load time, and so is a
`collection` that is not a defined collection. `on` must name a
**has-one, single-target** `relationship` or `upload` field at the top level
of the target collection (layout wrappers — row, collapsible, tabs — are
transparent) whose target is the collection that owns the join; anything else
— an unknown name, a field referencing another collection, a has-many or
polymorphic relationship, a field inside a group — is a load error, since the
join would never list anything. For the same reason a join is rejected in a
global: nothing can reference a global. Setting
`required`, `localized`, or `required_locales` on a join is also a
**hard load error** (a join is virtual and read-only, so those flags
are meaningless — they are rejected rather than silently ignored).

## Behavior

- **No database column** — join fields are virtual. No migration, no storage.
- **Read-only** — displayed in the admin UI but not editable. No form input is rendered.
- **No validation** — since no data is submitted, validation is skipped entirely.
- **Admin UI** — shows a list of linked documents with clickable links to edit each one. Displays "No related items" when empty.
- **API responses** — at `depth >= 1`, join fields return an array of document objects from the target collection. At `depth = 0`, join fields are omitted (no stored value).
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
