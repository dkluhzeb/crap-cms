# Array / Blocks row identity and diff-based writes

Status: **PLANNED** (design only — not yet implemented)

## Problem

An `update` that writes an array or blocks field **rebuilds the whole junction
row set from scratch**: `set_array_rows` / `set_block_rows` `DELETE` every row
for `(parent_id[, _locale])` and re-`INSERT` the incoming rows with freshly
minted `nanoid` primary keys. A sub-field that is *absent from the incoming row*
— because the field-access write strip removed it (the caller lacks write access
to it) or because the surface simply didn't send it — is written as `NULL`,
**destroying its stored value**.

This is a real data-loss bug. It also breaks the invariant the scalar path
already guarantees:

- **Scalar update is column-preserving.** `update_inner` builds the `SET` clause
  from *only the columns present in the (already write-stripped) input*
  (`src/db/query/write/update.rs`). A write-denied or simply-absent scalar column
  is never named in the `SET`, so its stored value is preserved. A locked field
  survives an update by an unprivileged caller.
- **Array/blocks update is destructive.** There is no per-row "update only the
  present columns" path, because a junction row has **no stable identity across
  writes**. The read path returns each row's `id`
  (`find_array_rows` → `map.insert("id", …)`), but that id is **not round-tripped
  to the write path**: the admin form emits no per-row hidden `id`, and the
  Lua / gRPC / MCP row shapes carry none. So on write the server cannot tell
  which incoming row *is* which stored row, and falls back to positional
  delete-and-reinsert.

The same gap makes reordering lossy in principle (identity is positional) and
makes any future per-row concern (per-row audit, per-row optimistic locking)
impossible.

### Why the obvious quick fixes are unsatisfying

- **Positional merge** (match incoming row *i* to stored row *i*, merge absent
  columns from it): wrong the moment a row is inserted, removed, or reordered —
  it silently merges values from an unrelated row.
- **Content hashing** (match rows by the hash of their present columns):
  ambiguous for duplicate rows, and unstable precisely for the edited row we
  care about.
- **Server-only id via snapshot position**: still positional; same failure.

None give a *correct* answer under insert/delete/reorder. The only correct
mechanism is an explicit, round-tripped row identity.

## Target mechanism

Give every relational-spine array/blocks row a **stable identity that
round-trips end-to-end**, and replace the delete-and-reinsert write with a
**diff-based upsert** that is column-preserving per row — the exact array analog
of the scalar `SET`-only-present-columns rule.

1. **Identity = the existing junction-row `id`.** No schema change: the `id`
   nanoid PK already exists and is already returned on read. New rows have no id;
   the server mints one.

2. **Round-trip the id on every surface** (read → edit → write):
   - **Admin form:** render a hidden `id` per array/blocks row; parse it back
     into each incoming row.
   - **Lua / gRPC / MCP:** accept an optional `id` on a row object; echo it on
     read (already present for Lua/read shapes — verify each surface).

3. **Diff-based save**, replacing `set_array_rows` / `set_block_rows`:
   - Load the stored rows for `(parent_id[, _locale])` keyed by `id`.
   - **Match** each incoming row that carries a *known* id to its stored row:
     `UPDATE` only the columns present in the incoming row (write-stripped),
     leaving unlisted columns — including write-denied and absent sub-fields — at
     their stored value. Set `_order` from the incoming position.
   - **Insert** each incoming row with no id, or an id not in the stored set
     (see security note): mint a fresh id, `INSERT`. Absent columns default to
     `NULL` (a genuinely new row has no prior value to preserve).
   - **Delete** each stored row whose id is absent from the incoming set.

4. **Field-strip integration is then automatic:** because a matched row's
   `UPDATE` names only the present columns, a write-denied sub-field the strip
   removed is never in the `SET` and keeps its stored value — identical to the
   scalar guarantee. No separate "merge denied from snapshot" step is needed; the
   preservation falls out of the column-preserving `UPDATE`, same as scalars.

### Security note (client-supplied id is a merge key)

A row `id` sent by a client is only a *merge key*, never a trusted PK:

- The diff matches an incoming id **only against the stored id set for that exact
  `(parent_id, field, _locale)`**. An id that is not in that set is treated as a
  **new** row and the server mints a fresh id — a client cannot choose a PK, nor
  address a row under a different parent/field/locale, nor merge across locales.
- Because matching is scoped, a client cannot use a guessed id to read-through or
  overwrite another document's row: an unknown id can only ever create a new row
  under the parent the caller is already authorized to write.

## Scope boundary — relational spine only

Identity and diffing apply to the **relational spine**: a top-level array/blocks
field (and a group-nested one under its `group__field` join key), whose scalar
sub-fields are real columns. Anything nested *inside* a row — a group, a nested
array (array-in-array), nested blocks — is stored as JSON in that row's column
and is replaced wholesale with the row, exactly as today. That JSON has no
per-element identity and no per-element column-level access strip, so there is
nothing to preserve at that level; a denied leaf inside nested JSON is handled by
the existing nested strip walking the value before it is written. If per-element
identity inside nested JSON is ever needed, it layers on top of this design; it
is explicitly out of scope here.

Blocks: identity is the row `id`; a changed `_block_type` on the same id is
treated as a **replace** (delete + insert), because the column set differs.

## Back-compatibility

- Existing databases already have row ids; no migration or backfill.
- A surface that does **not** yet send ids (an old client, or a surface before
  its round-trip lands) degrades to today's positional delete-and-reinsert for
  *its* writes — the loss is unchanged for that path, never worse. The admin
  form and each programmatic surface adopt id round-trip in Phase 3; until a
  surface does, its behavior is exactly the status quo.

## Ref-counting

`after_update` recomputes outgoing references from the full new document versus
the stored one via the shared recursive walker; it does not depend on *how* the
junction rows were written. The diff-based save must still leave the final row
set identical to what a full rebuild would produce (same values, same `_order`),
so ref-count computation is unaffected. A regression test asserts ref counts are
identical after a diff-based update and after an equivalent full rebuild.

## Phased implementation plan

- **Phase 0 — audit & pin.** Confirm on every surface (admin, Lua, gRPC, MCP)
  whether the row `id` is present on read and whether it survives to the write
  input. Write the failing regression test first (below). Pin the current
  destructive behavior in a test so the change is visible.
- **Phase 1 — read/id round-trip.** Ensure every read surface emits the row
  `id`; thread a hidden `id` through the admin array/blocks templates and their
  form parser; accept optional `id` on Lua/gRPC/MCP row objects.
- **Phase 2 — diff-based save.** Replace `set_array_rows` / `set_block_rows` with
  a load-stored → diff (update-present-columns / insert-new / delete-removed)
  routine. Keep the empty-input and locale-scoping semantics. Column-preserving
  `UPDATE` per matched row.
- **Phase 3 — surface adoption.** Wire each surface's write path to pass row ids
  into the save routine. Until wired, a surface keeps status-quo behavior.
- **Phase 4 — verification.** Full regression suite (below), ref-count parity
  test, e2e for the admin array edit-preserves-locked-subfield path.

## Regression tests (write-first)

1. **The bug:** create a doc with an array whose row has a `secret` sub-field
   with `access.update = false`; as an unprivileged caller, update the row's
   *visible* sibling; assert `secret` **retains its stored value** (today: it
   becomes `NULL`).
2. **Reorder preserves values:** move row B before row A; assert every row's
   full value set is intact (no positional bleed).
3. **Insert/delete:** add one row, remove another; assert the surviving rows keep
   their ids and values, the new row gets a fresh id, the removed row is gone.
4. **Unknown/foreign id is a new row:** an incoming id not in the parent's stored
   set mints a fresh row and never merges across parent/field/locale.
5. **Ref-count parity:** ref counts after a diff-based update equal those after
   an equivalent full rebuild.
6. **Blocks `_block_type` change on same id:** treated as replace, no column
   bleed between block shapes.

## Definition of done

- All six regression tests pass; test #1 fails before the change, passes after.
- Scalar and array/blocks writes share one stated contract: *an update sets only
  the columns the caller supplied and is authorized to write; every other stored
  column is preserved.* Add this line to `docs/src/internals/frozen-contracts.md`
  once implemented.
- No surface regresses to *worse-than-today* behavior at any phase.
