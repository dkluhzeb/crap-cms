# Concurrent Editing

Two editors open the same document, both change it, both save. Without a
check, the second save silently overwrites the first — including array rows or
blocks the first editor added, because the second form never saw them. Crap CMS
prevents this with **optimistic locking**: every document carries a revision
counter, and a write can require that the document still be at the revision it
was based on.

## The revision

Every collection document and every global carries a `_revision` system field:
an integer that starts at `0` and moves one forward with **every write that
changes the document**, on every surface (admin, gRPC, MCP, Lua — hooks
included — and the upload API):

- an update, whether it publishes or saves a draft — a draft save moves the
  revision even though the published row's content stays the same;
- an unpublish;
- a version restore;
- each document a bulk `update_many` writes;
- each document `crap-cms import` overwrites. A document the import creates
  instead takes the revision it was exported at — no form can be open on a
  row that did not exist — so an export → import round trip reproduces it.

It does not move for bookkeeping that is not an edit of the document:
reference-count changes, account state (login lockout, session version,
password-reset and verification tokens), moving a document to the trash and
back, or a background image conversion filling in a derived format URL — a
form opened before the conversion finished never sends that URL back, so it
cannot overwrite it.

Every read returns it — `find`, `find_by_id`, `get_global`, the draft view,
and the document a write returns (a draft save included) — as the `_revision`
key beside the fields. A `select` projection always keeps it. It is never
recorded in version snapshots, cannot be written, and is not a filter or sort
column.

## Requiring a revision (`expected_revision`)

A write that sends the revision it read is refused when the document has been
written since:

| Surface | Option | Refused with |
|---------|--------|--------------|
| gRPC | `UpdateRequest.expected_revision`, `UpdateGlobalRequest.expected_revision` (also honored with `UpdateRequest.unpublish`) | `ABORTED` |
| MCP | `expected_revision` argument of `update_*`, `unpublish_*`, `global_update_*` | a tool error containing `Revision conflict` |
| Lua | `expected_revision` option of `crap.collections.update` / `.unpublish` and `crap.globals.update` / `.unpublish` | a Lua error containing `Revision conflict` |
| Upload API | `_revision` form field of `PATCH /api/upload/{slug}/{id}` | `409` |
| Admin | the edit form's hidden `_revision` input | the conflict page (below) |

A refused write changes nothing. The error names the revision you sent and the
one the document is at now: re-read the document and apply your change again,
or — to overwrite the other change on purpose — resend with the current
revision.

Omitting the option writes unconditionally (last write wins), exactly as
before; scripts, imports and bulk updates that don't care keep working
unchanged.

The check is atomic: it runs inside the write's transaction, after the access
check and under the document's row lock, and compares and advances the
revision in a single statement. Of two writers that read the same revision,
exactly one lands on SQLite and PostgreSQL alike. A caller the access rules
refuse is told nothing about the revision.

```lua
-- Read-modify-write that never overwrites a concurrent change
local post = crap.collections.posts.find_by_id(id)
crap.collections.posts.update(id, { views = post.views + 1 }, {
    expected_revision = post._revision,
})
```

## In the admin UI

Every collection and global edit form carries the revision it was loaded at in
a hidden `_revision` input, and every save from it — publish, save draft,
unpublish, a file replacement — sends it back. When someone else saved the
document in the meantime, the save is refused and the form comes back with:

- the editor's unsaved values still in place;
- a notice saying the document was saved by someone else, offering
  **Reload** (show the saved document, discarding the edits) and
  **Overwrite** (save these edits over the other change, with the same action
  — publish or draft — the refused save used).

The re-rendered form carries the document's *current* revision, so saving it
again is the deliberate overwrite. A validation error re-render keeps the
revision the form was loaded at, so the corrected save is still checked. A
file chosen for upload is not carried over — a browser never pre-fills a file
input — so choose it again before overwriting.

An **Unpublish** from a stale form is refused with a notice instead: it posts
no field values, so there is nothing to keep. Reload the document, look at the
other change, and unpublish again.

A [template override](../admin-ui/index.md) of `collections/edit.hbs` or
`globals/edit.hbs` has to keep the hidden input (and the
`partials/revision-conflict` notice) to keep this protection; a form that
submits no `_revision` saves unconditionally.
