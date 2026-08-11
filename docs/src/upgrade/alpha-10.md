# Upgrading to alpha.10

This guide covers the upgrade path from `alpha.9` to `alpha.10`. It
focuses on operator and plugin-author action items first; additive
features follow.

alpha.10 is a stabilization release: several Lua-facing contracts that
were loose or silently-wrong are tightened so they can freeze. Most
projects need no changes — the items below only bite definitions that
were already relying on ignored or malformed input.

Definition errors are reported as the loader hits them, one file at a
time (post-parse checks then aggregate). A project with several latent
problems may need a couple of fix-and-restart cycles before it boots
clean.

## TL;DR

- **Replace your binary, restart.** DB schema migrations apply
  automatically; no manual SQL.
- **Plugin authors: option typos now error.** Every Lua CRUD option
  table rejects unknown keys. A previously-ignored typo (e.g.
  `overrideAcces`) now fails loudly. Fix any stray keys in your
  `crap.collections.*` / `crap.globals.*` / `crap.jobs.define` calls.
- **Plugin authors: drop option keys from bulk-op *queries*.** The
  `update_many` / `delete_many` query table only accepts `where`. Move
  `override_access` / `locale` / `draft` to the options argument.
- **Plugin authors: `crap.hooks.register` rejects unknown events.** A
  typo'd event name is now an error instead of a silent no-op.
- **Check field names.** A field named `id` / `parent_id` /
  `created_at` / `updated_at`, or one starting with `_`, is now
  rejected at definition time (it always collided with a generated
  column — previously it crashed at migration or silently shadowed).
- **Schema authors: silently-ignored definition values now error at
  load.** Wrong-typed `access` rules, malformed relationship / join /
  upload / date-bound config, invalid `live.mode`, and unknown keys in
  the last few lenient sub-tables all hard-error now (item 6).
- **Hook authors: `ctx.locale` is the resolved content locale** on
  default-locale writes too, no longer `nil` (item 8).
- **API clients: job-run reads now honor the job's `access` function.**
  `GetJobRun` / `ListJobRuns` / `ListJobs` are no longer readable by *any*
  authenticated caller — they enforce the job's `access` (with
  `operation == "read"`). If your client read job runs for a job that has an
  `access` function, make sure that function allows the reader (see Security
  fixes).

## Required action items

### 0. Rename camelCase keys to snake_case (API casing unified)

The API is now uniformly snake_case. Two Lua **option keys** and the
**pagination result** fields were the last camelCase holdouts and are
renamed:

```diff
  crap.collections.posts.find(query, {
-     overrideAccess = true,
+     override_access = true,
  })
  crap.collections.posts.delete(id, {
-     forceHardDelete = true,
+     force_hard_delete = true,
  })

- local total = result.pagination.totalDocs
+ local total = result.pagination.total_docs
```

Pagination fields renamed: `totalDocs → total_docs`, `hasNextPage →
has_next_page`, `hasPrevPage → has_prev_page`, `totalPages →
total_pages`, `pageStart → page_start`, `prevPage → prev_page`,
`nextPage → next_page`, `startCursor → start_cursor`, `endCursor →
end_cursor`. Also, `crap.collections.list_versions` returns
`result.documents` instead of `result.docs` (matching `find`).

### 1. Remove unknown keys from Lua CRUD option tables

Every option table now rejects unrecognized keys
(`deny_unknown_fields`), matching the behavior the query tables already
had. This catches typos that previously defeated the option they were
meant to set — most dangerously a misspelled `override_access`, which
silently left access control **on**.

```diff
  crap.collections.posts.create(data, {
-     overrideAcces = true,   -- silently ignored before; now an error
+     override_access = true,
  })
```

Affected: the option arguments of `create`, `update`, `delete`,
`find_by_id`, `validate`, `undelete`, `unpublish`, `list_versions`,
`restore_version`, the `crap.globals` ops, and the config table of
`crap.jobs.define`.

### 2. Move bulk-op options off the query table

The `update_many` / `delete_many` query (2nd) argument previously
*declared* option keys it never read — `override_access` / `locale` /
`draft` on `update_many`, and `override_access` / `locale` on
`delete_many`. The effective values came from the options argument all
along. The query table now carries only `where`; pass the options in
the options argument.

```diff
  crap.collections.posts.update_many(
-     { where = { status = "draft" }, override_access = true },
+     { where = { status = "draft" } },
      { status = "published" },
+     { override_access = true }
  )

  crap.collections.posts.delete_many(
-     { where = { status = "archived" }, locale = "de" }
+     { where = { status = "archived" } },
+     { locale = "de" }
  )
```

`delete_many`'s `locale` now lives in the options argument, so single
and bulk deletes take locale the same way.

### 3. Fix unknown hook event names

```diff
- crap.hooks.register("on_change", fn)   -- not a real event; now errors
+ crap.hooks.register("before_change", fn)
```

The valid events are listed under
[crap.hooks](../lua-api/hooks.md#events). An unrecognized name
previously logged a warning and registered a hook list that never
fired; it is now a hard error so the typo surfaces immediately.

### 4. Rename reserved field names

A field `name` is rejected at definition time when it collides with an
automatically generated column:

- starts with `_` (reserved for system columns), or
- contains `__` (reserved for group-field column nesting), or
- is exactly `id`, `parent_id`, `created_at`, or `updated_at`.

These names always collided with a generated column — they either
failed the `CREATE TABLE` with a duplicate-column error or silently
shadowed a system column. Rename the field.

Collection **slugs** are also checked for collisions with generated
join-table names at startup: a collection slugged `posts_tags`
conflicts with the `tags` array field of a `posts` collection (both
generate a `posts_tags` table). Boot fails with a clear error instead
of one definition silently corrupting the other's table during
migration.

### 5. Remove `[live] default_mode` from `crap.toml`

This key never did anything — every collection's live mode defaulted to
`metadata` regardless of it. Set the mode per collection instead.

```diff
  [live]
  enabled = true
- default_mode = "full"
  transport = "memory"
```

```lua
-- per-collection live mode (the only control):
crap.collections.define("posts", {
    live = { mode = "full" },
})
```

If present, config load now fails with `unknown field "default_mode"`.

### 6. Fix definitions that relied on silently-ignored values

alpha.10 makes every Lua schema table strict: a present-but-wrong value
that was previously ignored or coerced is now a load-time error. A
definition only breaks if it was already relying on input that did
nothing. The full list:

- **Unknown keys error everywhere.** `crap.pages.register` options,
  `crap.richtext.register_node` specs, the per-collection
  `live = { ... }` sub-table, `mcp.operations`, and the field
  `admin.labels` sub-table now reject unknown keys (every other schema
  table already did). Typos error with a did-you-mean suggestion.
- **`access` rules must be strings.** A present-but-non-string access
  rule (e.g. `read = some_function` or `read = true`) was silently
  dropped, falling back to the default policy — a security footgun.
  String hook references and omitting the rule are unchanged.
- **Globals reject `access.create` / `access.delete` / `access.trash`.**
  A global has a single row with only `get`/`update` operations, so
  these access keys never fired — they were silently ignored and now
  error at load. Use `access.read`, `access.draft`, `access.update`, or
  the `access.versions` toggle. (An access key whose enabling feature is
  off — e.g. `access.draft` without `versions.drafts` — now logs a
  startup warning instead of being silently dead, on collections and
  globals alike.)
- **Field `admin = { ... }` values are strictly typed.** A wrong-typed
  `label` / `width` / `rows` / `features` / etc. was silently dropped;
  now it errors. Numbers where strings are expected (e.g. `width = 50`)
  still coerce.
- **`live.mode` is validated.** An unrecognized mode was silently
  coerced to `metadata`; now only `"full"` / `"metadata"` are accepted,
  and `filter` must be a string.
- **Relationship / Upload fields require a `relationship = { ... }`
  config.** Such a field without one used to migrate as a plain TEXT
  column — no populate, no ref-counting, no delete protection. Also:
  `relationship.max_depth` must be a non-negative integer, and entries
  in a polymorphic `collection` array must be strings.
- **Join fields require non-empty string `collection` and `on`.** A
  missing or malformed value used to produce a join that silently
  matched nothing.
- **`min_date` / `max_date` must be valid `YYYY-MM-DD` strings**, with
  `min_date <= max_date`. Malformed bounds used to silently never (or
  always) match.
- **Upload config is validated.** An `image_sizes` entry missing
  `name` / `width` / `height` used to vanish; an unknown `fit` value
  (e.g. `"covr"`) fell back to `cover`; a malformed
  `upload.max_file_size` silently inherited the global default. All
  three now error at definition time.
- **`[cors]` in `crap.toml` is validated.** Origins must be
  `scheme://host[:port]` exactly (no path, no trailing slash —
  `https://app.example.com`, not `app.example.com`), `"*"` must be the
  only entry when used, method/header entries must be valid tokens,
  and `allow_credentials = true` with the wildcard origin is an error.
  Invalid entries used to be silently dropped from the allowlist (or
  kept but never matched), surfacing only as blocked requests.
- **`crap.routes.register` validates `csrf` and `max_body` types.** A
  non-boolean `csrf` (e.g. `csrf = 1`) used to be silently dropped,
  leaving the route with CSRF protection **off** — a fail-open — and a
  wrong-typed `max_body` was silently ignored, leaving the default body
  limit. Both now error at load. `max_body` accepts a whole-valued
  float, so `max_body = 2^16` (a float in Lua) works instead of being
  ignored.
- **`crap.richtext.register_node` validates `inline` and `label` types.**
  `inline` was read with Lua truthiness, so `inline = "false"` (a truthy
  string) silently registered the node as inline **true**. A non-boolean
  `inline` now errors at load; `label` is read strictly too.
- **`crap.storage.register` / `crap.email.register` reject unknown handler
  keys.** A typo'd handler function (`exsits`, `sned`) used to be silently
  ignored; it now errors at load.
- **`crap.hooks.remove` validates the event name.** An unknown event name
  used to be a silent no-op (unlike `crap.hooks.register`, which already
  errored); it now errors.

### 6a. Rename job slugs and richtext node names to valid slugs

Two registration surfaces that previously accepted looser identifiers now
enforce the standard slug rule (lowercase ASCII letters, digits, and
underscores; not starting with an underscore):

- **`crap.jobs.define(slug, …)`** — a job slug with a hyphen, uppercase
  letter, or space now errors at load. Rename e.g. `send-digest` →
  `send_digest`.
- **`crap.richtext.register_node(name, …)`** — a node name with uppercase
  or non-ASCII characters (the old check was Unicode-aware) now errors.
  Rename to a lowercase ASCII slug.

### 6b. Number fields reject non-numeric input

A non-numeric value submitted for a `number` field (e.g. the string
`"abc"`) used to pass validation and be silently coerced to `NULL` on
write — silent data loss that also bypassed `required` and min/max
bounds. It is now a validation error. Send `nil` / omit the field for
"no value"; numeric strings (the admin-form encoding) still work.

### 6c. More freeze-hardening rejections (fix if you hit them)

The stabilization pass tightened a few more previously-lenient spots.
Each only bites a definition that was already relying on ignored input:

- **Unknown field `type`** (e.g. `type = "tex"`) is rejected instead of
  silently becoming a `Text` column. An omitted `type` still defaults to
  `text`.
- **Reserved field names**: a field ending in `_tz` / `_lang` (timezone /
  language companion suffixes), or colliding with an upload metadata
  column (`filename`, `url`, `width`, `focal_x`, …) on an upload
  collection, is rejected.
- **Enum-typed `admin.*` values** — `admin.position`, `admin.picker`,
  `admin.format` — reject an unrecognized value (e.g. `format =
  "lexical"`).
- **`crap.routes.register { access = true }`** is rejected — omit
  `access` for a public route, or pass a hook ref to gate it (`true`
  silently meant "public").
- **`crap.email.send { retries = N }`** is rejected — `retries` only
  applies to `crap.email.queue`.
- **Over-long generated identifiers** (>63 bytes on Postgres) are
  rejected at migration — shorten very long collection/group/field/locale
  name combinations. The error names the offending identifier.

### 7. Runtime option tables also reject unknown keys

Beyond the CRUD options in item 1, the remaining Lua option tables are
now strict too:

- **`crap.http.request`** — a typo like `timout = 5` used to silently
  run with the default 30-second timeout; now it errors. (The `timeout`
  value itself now also accepts fractional seconds, e.g. `0.5`.)
- **`crap.email.send` / `crap.email.queue`** — e.g. a typo'd `retires`
  used to silently queue with the default retry count.
- **`crap.jobs.queue`** — the options argument accepts only
  `priority`, `delay`, and `unique`.
- **MCP `where` clauses** — an unknown filter operator (e.g.
  `gretaer_than`) now fails the tool call instead of silently dropping
  that condition and returning more rows than intended.

### 8. Hook authors: `ctx.locale` is now the resolved content locale

Collection-level hooks (`before_change`, `after_change`, `after_read`,
…) used to see `ctx.locale = nil` when writing the default locale,
while field hooks, validators, and access functions saw the resolved
code (e.g. `"en"`). All hook surfaces now agree: `ctx.locale` is the
content locale the operation targets, and is `nil` only when
localization is disabled (and on the locale-agnostic `before_delete` /
`after_delete`). A hook that treated `nil` as "default locale" should
compare against the configured default locale instead.

### 9. Remove `locale` from `crap.collections.delete` options

Single delete is locale-agnostic — it removes the whole row across all
locales — and never read the key; it was a silently-ignored no-op and
is now an error. To remove one locale's content, **update** the
document with that locale's fields set to `null` (and localized
arrays/relationships to `[]`); there is no per-locale delete.

## Admin UI behavior

- **List pages return 400 on invalid query params.** A
  present-but-invalid `where[...]` filter (unknown operator or field,
  system column, malformed key), an unknown/unsortable `sort` field, or
  an invalid `_status` value used to be silently ignored — the list
  rendered unfiltered or default-sorted results. They now render a 400
  Bad Request page naming the offending parameter (parity with MCP and
  gRPC, which already hard-error). URLs produced by the admin filter UI
  are unaffected; only hand-edited or stale bookmarked URLs with
  since-renamed fields can be affected.

## Security fixes

- **`crap.collections.update(id, data, { unpublish = true })` now enforces
  access.** The `unpublish` option used a bespoke path that skipped access
  evaluation, so a caller whose `access.update` filter didn't match a document
  could still unpublish it (and the returned document skipped the read/API-hidden
  strips). It now routes through the same service path as
  `crap.collections.unpublish`. **Action:** none, unless you relied on the
  missing check — that was a bug.
- **Version restore now verifies the version belongs to the target document.**
  A caller with `update` access to document A could restore document B's snapshot
  onto A (cross-document snapshot injection). Restore now rejects a version whose
  `_parent` doesn't match the target id, on every surface. **Action:** none.
- **Live event streams run the field-read strip before the per-subscriber
  `after_read` hook**, matching normal reads. Previously `after_read` ran first,
  so a hook copying a read-denied field's value into an unprotected field could
  leak it to a denied subscriber. **Action:** none.
- **`crap.collections.ref_count` now gates on read access.** It was the only
  read-shaped Lua op with no access check. It now performs a read-visibility
  check and errors for a document the caller can't read. **Action:** none, unless
  a hook read counts for documents the current user can't see — gate accordingly
  or pass a privileged user.
- **A richtext node-attr custom `validate` function that errors now fails the
  write** (it was logged and the document saved — fail-open, unlike top-level
  and sub-field validators). **Action:** none.
- **`crap.env.get` hides the `CRAP_SECRET_*` prefix from hooks.** Config `${VAR}`
  substitution still reads it at load, but a hook reading a `CRAP_SECRET_*` var
  now errors. **Action:** store secrets that must stay out of userland Lua under
  the `CRAP_SECRET_*` prefix; other `CRAP_*` vars remain hook-readable.
- **Data-aware field access is now consistent across transparent layout
  wrappers.** An `access.read` / `access.update` rule that keys on a sibling
  field's value (`ctx.data`) produced a different keep/strip decision depending
  on whether the field sat directly at its level or inside a
  Row/Collapsible/Tabs wrapper — the wrapper re-snapshotted `ctx.data` after
  earlier siblings had already been stripped. With an inverted rule this could
  keep a field that should have been stripped. Wrappers now evaluate against the
  same pre-strip sibling view as a direct sibling. **Action:** none; layout
  wrappers were always documented as transparent — this restores that behavior.
- **Version history no longer leaks other owners' draft snapshots under a
  filtered draft rule.** If `access.draft` returns a filter table (e.g.
  `{ author = ctx.user.id }`), the version surfaces (`ListVersions` / `GetVersion`
  and the admin version sidebar) now enforce that filter against the parent
  document — a non-match shows published snapshots only. Previously a filtered
  draft rule was treated as full draft access, exposing any readable document's
  draft version snapshots. **Action:** none; boolean `draft` rules are
  unaffected, and filtered rules now behave like the live `find_by_id` draft
  gate.
- **Un-verifying a user now tears down their live-update streams.**
  `UnverifyAccount` bumps `_session_version` (revoking login when email
  verification is required) but didn't signal stream invalidation, so an open
  SSE/`Subscribe` stream kept running on the revoked session. It now publishes the
  invalidation like the lock/password-reset flows. **Action:** none.
- **Job-run reads now honor the job's `access` function.** `GetJobRun` and
  `ListJobRuns` previously applied no authorization beyond authentication — any
  authenticated caller could read any job's run payloads (`data_json`,
  `result_json`, `error`), even for a job whose `access` restricted who could
  *trigger* it. All three job-read RPCs (`GetJobRun`, `ListJobRuns`, `ListJobs`)
  now enforce the job's `access`, invoked with `operation == "read"` (trigger
  stays `operation == "trigger"`), so one function can gate both or branch.
  Reads are a permissive union: `ListJobRuns`/`ListJobs` omit jobs the caller
  may not read; `GetJobRun` returns `not_found` for a denied/unknown run. Jobs
  with no `access` function stay readable by any authenticated caller. **Action:**
  if a client reads runs for a job that has an `access` function, ensure that
  function returns true for the reader (it now receives `ctx.operation`).
- **Admin SSE events no longer carry the editor's identity.** The
  `/admin/events` payload used to send `edited_by` as a full
  `{ id, email }` object to every subscriber — anyone with read access
  to a collection learned which user (including their email) made each
  change. The payload now carries a server-computed `self` boolean
  (`true` when the subscriber is the editor) instead. **Breaking for
  custom SSE consumers that read `edited_by`** — switch to `self`.
  Server-side, the `live` filter and `before_broadcast` hook contexts
  still receive the complete `edited_by`; the gRPC `MutationEvent`
  never carried identity.
- **Checkbox columns become `SMALLINT` on Postgres.** They were stored
  as `BIGINT`. A one-time, idempotent, introspection-guarded migration
  retypes existing columns (locale variants and array join tables
  included) on first startup — expect it once; no manual action. The
  `ALTER` takes an exclusive lock per table, so very large tables make
  that first startup correspondingly slower. SQLite is unaffected.
- **Login rate limiting fails closed on backend errors.** With the
  Redis rate-limit backend, an outage used to silently disable
  login/forgot-password brute-force protection. A backend error now
  blocks the attempt and logs the infrastructure error — an outage
  degrades login availability instead of security.
- **Version history visibility follows the content views, plus a new
  `access.versions` toggle.** `list_versions` and reading a single snapshot were
  gated by `access.read`, so any reader could enumerate every past snapshot
  (including unpublished states). *Which* snapshots are visible is now the same
  composite as document reads: published snapshots need `read`, draft snapshots
  additionally need `access.draft` (a published-only reader sees only published
  history). A new **`access.versions`** rule gates whether history is visible at
  all — a toggle that, like `draft`/`trash`, falls back to `update` when unset
  (so a published-only reader sees no history by default; an editor does). It
  returns `true`/`false` (a filter table is a configuration error). `restore`
  requires **both** `access.update` and `access.versions` (it resurrects
  historical content). **Action:** if you relied on version history being visible
  to plain readers, set `access.versions` to an explicit permissive rule (e.g.
  `access.anyone`); to lock history behind a stricter policy than editing, set it
  to that rule. The admin version sidebar degrades gracefully (no list rather
  than an error) for viewers who cannot see history.
- **Reading drafts now requires edit-level access (`access.draft`).** Draft
  reads were gated by `access.read`, so any reader could pull unpublished
  content by opting in (`draft = true` / `use_draft` / `include_drafts`, or a
  `_status = "draft"` filter) — a public `read` rule exposed drafts. A new
  `access.draft` hook gates draft reads and **falls back to `access.update`**
  (the same way `trash` falls back to `update`), so by default only editors can
  preview drafts and `read` covers published content only. Uniform across
  collections, globals, and every surface. **Action:** only if you deliberately
  exposed drafts to readers who lack edit access — set `access.draft` to permit
  them. The admin list/search/edit views request every view unconditionally and
  let the service downgrade per the viewer's access, so a read-only admin simply
  sees published content (no denial) while an editor sees drafts.
- **Embedded relationships and join fields gate draft targets by the target's
  `draft` access.** Populating related content at depth is a read of the target
  collection. Join fields previously applied no status filter (any reader with
  `read` saw a target's draft rows through a join — even anonymous callers on
  public surfaces), and relationship fields gated drafts only by the parent
  read's opt-in, not the target's `draft` access. A draft target is now embedded
  only when drafts are requested **and** the viewer holds the target collection's
  `draft` access. **Action:** only if you relied on embedded drafts showing to
  readers without the target's `draft` access — grant `access.draft` on the
  target collection.
- **`find_by_id` hides never-published drafts.** Fetching a single
  document by id (including the public `GET /{collection}/{id}` surface)
  did not inject the `_status = 'published'` filter that the `find` /
  `search` list paths apply, so a document created as a draft and never
  published was returned to readers that did not opt into drafts. It now
  applies the same draft-visibility rule; pass an explicit draft opt-in
  (`use_draft` / Lua `draft = true`) to read the draft.
- **Unpublished globals are hidden from public reads.** After a global
  was unpublished, `get_global` still served the now-draft content to
  every reader. A non-draft read now serves the last published version
  snapshot (or empty content when nothing was ever published); the admin
  edit form opts into drafts so the global stays editable. The Lua
  `crap.globals.get` and MCP global-read surfaces now also hide an
  unpublished global by default — a behavior change only for globals that
  have been unpublished. To read the draft on purpose, pass the new
  `draft = true` option (Lua `crap.globals.<slug>.get({ draft = true })`,
  the MCP `draft` arg, or the gRPC `GetGlobalRequest.draft` field),
  symmetric with the collection `find_by_id` draft opt-in.
- **MFA codes expire at their exact timestamp** (`now < exp` rather than
  `exp >= now`), matching the reset/verification token checks. A code is
  no longer honored for one extra second past expiry.
- **Live validation enforces field-level write access.** The gRPC
  `Validate` RPC and MCP `validate` tool skipped field-level write-access
  denials (their write-hooks bundle had no DB connection), so the dry-run
  validated fields the caller cannot write. They now evaluate denials like
  the real write path.

## gRPC clients (regenerate from `proto/content.proto`)

Wire-contract changes — regenerate your gRPC stubs and adjust:

- **Removed always-true `success` fields** from `DeleteResponse`,
  `ForgotPasswordResponse`, `ResetPasswordResponse`, `VerifyEmailResponse`,
  and `AccountActionResponse`. A non-error response is the success signal;
  drop any `if (!resp.success)` branches. `DeleteResponse.soft_deleted`
  remains.
- **Removed `JobDefinitionInfo.handler`** (the internal Lua function
  reference) from `ListJobs`.
- **`JobRunInfo` is now the shared job-run message.** `ListJobRuns` returns
  `repeated JobRunInfo`; `GetJobRun` returns `GetJobRunResponse { run }`
  wrapping a `JobRunInfo` (was a flat `GetJobRunResponse`). Read a single
  run via `response.run`.
- **Closed-set string fields became enums:** `MutationEvent.operation` /
  `.target`, `VersionInfo.status`, `JobRunInfo.status` / `.scheduled_by`,
  and the `ListJobRunsRequest.status` filter. Use the generated enum
  accessors (`event.operation()` etc.); the zero value is `*_UNSPECIFIED`,
  which for the `ListJobRunsRequest.status` filter means "all statuses".
- **Account RPCs now return `UNAUTHENTICATED` before any collection-shape
  error.** A client calling `LockAccount`/`VerifyAccount`/etc. without (or
  with an invalid) token now gets `UNAUTHENTICATED` even when the
  collection is unknown or lacks `verify_email` — previously it could get
  `NOT_FOUND` / `INVALID_ARGUMENT` / `FAILED_PRECONDITION` first. Authenticate
  before relying on those shape errors.
- **Additive:** `CountRequest.trash` counts soft-deleted documents
  (mirrors `FindRequest.trash`). Non-breaking.
- **Additive:** `GetGlobalRequest.draft` reads the unpublished draft of an
  unpublished global (mirrors `FindByIdRequest.draft`). Non-breaking.
- **Doc-only:** `Create`/`Update` now document that a UNIQUE-constraint
  conflict maps to `ALREADY_EXISTS` (the runtime mapping was already
  `ALREADY_EXISTS`; only the proto comment was stale).

## Bug fixes (no action needed)

- **Bulk `update_many(draft = true)` saves a draft instead of
  publishing.** The bulk path accepted the `draft` flag but ignored it
  on the write side, writing the main row directly — so a bulk "save as
  draft" silently published every matched document. It now routes to the
  version table and leaves the published row untouched, matching the
  single-document update.
- **Draft edits to a join field nested in a group no longer vanish.** An array,
  blocks, or has-many relationship nested inside a group, edited as a draft, was
  silently dropped (the snapshot rebuilt the group's join data from the DB). The
  draft overlay now restores group-nested join data at any depth. (Pre-existing;
  found during review.)
- **Draft edits to group sub-fields no longer revert.** Saving a draft
  that edited a group sub-field via the nested data shape
  (`{ seo: { title } }`, as the gRPC/MCP/admin surfaces send) lost the
  edit on restore, because the snapshot kept both the stale flat column
  and the new nested object. The draft overlay now flattens group data
  before merging.
- **Bulk operations are now atomic.** `create_many`, `update_many`, and
  `delete_many` run in a single transaction on every surface (gRPC, Lua,
  admin, MCP). Previously they committed in batches of 500, so a
  failure partway through left earlier batches committed — partial
  state. Any failure now rolls the whole operation back. The trade-off:
  a very large bulk op holds the write transaction for its whole
  duration — cap it with `[server] bulk_max_documents` if untrusted or
  over-broad bulk calls are a concern.
- **Transient storage failures serve 503, not a false 404.** Serving an
  upload from a remote backend (S3 / custom) now distinguishes a genuine
  missing key (404) from a transient infrastructure failure, which
  returns a retryable 503 instead of a cacheable 404 for a file that
  exists. Custom storage `get` handlers should return `nil` for a
  missing key and raise only on real failures.
- **`crap-cms serve --only grpc`** is now accepted (matching the
  `[server] grpc_*` config keys). `--only api` still works as an alias,
  so no script changes are required.
- **`auth.password_policy` validation error** now names the real config
  path (it previously referred to a non-existent `auth.password.*` key).
- **Config backend selectors** (`[database] backend`, `[upload] storage`,
  `[email] provider`, `[cache] backend`, `[auth] rate_limit_backend`,
  `[live] transport`) are now validated at config load — a typo'd value
  fails immediately with the list of valid values instead of at server
  startup. Valid values are unchanged, so no config edits are needed.

- **Lua `find` / `find_by_id` honor `[depth] max_depth`.** Relationship
  population depth was previously clamped to a hardcoded maximum of 10,
  ignoring the configured ceiling. It now clamps to `[depth] max_depth`,
  matching the gRPC read path. Deployments with a `max_depth` other than
  10 will see the configured value take effect on the Lua surface.
- **No more spurious orphan-column warning for MFA collections.** The
  `_mfa_code` / `_mfa_code_exp` columns are now recognized as system
  columns, so migrations no longer log a false "column exists but is not
  in the Lua definition" warning for them.
- **MCP `delete` no longer crashes on localized upload collections.**
  The MCP delete tool now passes the configured locale to the service
  layer, matching gRPC and admin.
- **Reference counting now recurses into nested relationships.** A
  relationship nested inside a group within an array, a group within a
  block, or a has-many relationship inside a block was not counted
  toward delete-protection — so a referenced document could be
  hard-deleted while still in use, and counts could drift. All nesting
  depths are now counted. Existing databases recompute their
  `_ref_count` values once on the next startup (the backfill is
  version-gated); no action needed.
- **MCP hard-delete now cleans up upload files.** The MCP `delete` /
  `delete_many` tools now delete a removed document's uploaded files,
  matching gRPC and admin. (Soft-deletes still keep the files.)

## Behavior changes (likely no action)

- **Whole-valued `number` fields now serialize as integers.** Because
  `number` is stored as floating-point, an integer round-tripped through
  the database as `42.0` and was emitted that way on every read surface
  (REST/Lua/MCP/admin). Whole values now serialize as `42`; genuine
  fractions are unchanged (`42.5` stays `42.5`). JSON treats `42` and
  `42.0` as the same number, so virtually all clients are unaffected —
  the only thing that changes is a consumer that *string-matched*
  `"42.0"`, which will now see `"42"`. (gRPC is unaffected — it always
  carried numbers as `double`.)

- **JSON-decoded floats now keep full precision.** crap-cms now uses a
  correctly-rounded JSON float parser, so a `number` value read back
  from a JSON-backed path — the `blocks` `data` column, MCP arguments,
  keyset pagination cursors — is bit-identical to what was written.
  Previously the parser could be off by up to one ULP for some
  magnitudes (very large/small exponents). This only makes values *more*
  exact, so no action is needed. (gRPC is unaffected — it carries
  numbers as protobuf `double`, not JSON.)

## Additive features (alpha.10)

### Hook refs accept per-config `options` (`ctx.options`)

Any hook reference — collection/global lifecycle hooks, field hooks,
`access` rules, field `validate` / `required_when`, and the other ref
sites — can now be written either as a bare string or as a table:

```lua
before_change = { ref = "hooks.shared.slugify",
                  options = { from = "title", to = "slug" } }
```

The `options` table reaches the hook as `ctx.options` (`nil` for a
bare-string ref), so one hook function can be reused across collections
and fields with different configuration. Hook contexts also gained more
data across the board this release (e.g. display conditions see
`ctx.operation` / `ctx.user` / `ctx.locale`, field hooks see `ctx.id`)
— see the CHANGELOG for the full list.

### MCP writes accept `locale`, `draft`, and `force_hard_delete`

The MCP `create` / `update` / `update_many` tools now accept a `locale`
and (for single create/update) `draft` argument, `delete` accepts
`force_hard_delete`, and `global_read` / `global_update` accept
`locale` — so localized and draft content is reachable over MCP,
matching the gRPC and Lua write surfaces. These are reserved top-level
arguments, excluded from the document's field data like the existing
`id` / `password`. See [MCP overview](../mcp/overview.md).

### Per-operation MCP tool descriptions

A collection or global can override the description of an individual MCP
tool via `mcp = { operations = { delete = "...", create = "..." } }`
(keyed by `find` / `create` / `delete` / … for collections, `read` /
`update` / `validate` for globals). The collection-level `mcp.description`
now also folds into **every** generated tool (not just `find`), and the
auto-generated descriptions mention `draft=true` (drafts) and
`force_hard_delete` (soft-delete) where applicable — so no configuration
is needed to surface the non-obvious behavior. See
[MCP overview](../mcp/overview.md#per-operation-descriptions). Additive;
no action needed.

### Validate without persisting, on every surface

Collections and globals can now be validated without writing:

- **Collections** — gRPC `Validate` (existing), Lua
  `crap.collections.validate`, and the MCP `validate_<collection>` tool.
- **Globals** — newly added: gRPC `ValidateGlobal`, Lua
  `crap.globals.validate`, and the MCP `global_validate_<global>` tool.
  Global validation previously existed only in the admin UI.

All run the full before-write pipeline (coercion, validators, unique
checks, `before_validate` hooks) and return per-field errors. Globals
always validate in update mode against their singleton row.

### `number` fields accept `integer = true`

A `number` field can be restricted to whole values:
`{ type = "number", integer = true }`. Fractional input is rejected at
validation and the admin renders an integer stepper. Storage is
unchanged (floating-point — no migration), and whole values already
serialize as integers (see *Behavior changes* above). Composes with
`min` / `max` / `has_many`.

### gRPC `CountRequest.trash`

`Count` can now count soft-deleted (trashed) documents via a `trash`
flag, mirroring `FindRequest.trash`.

### Lua parity: empty trash, global drafts, and global unpublish

- **`crap.collections.delete_many(slug, query, { trash = true })`** now
  permanently removes already-soft-deleted rows (empty the trash) — a
  hard delete of trashed documents gated by `access.delete`. This was
  impossible from Lua before (the query surface can't filter the system
  `_deleted_at` column).
- **`crap.globals.update(slug, data, { draft = true })`** performs a
  version-only save (main row unchanged), matching
  `crap.collections.update`'s `draft` option.
- **`crap.globals.unpublish(slug, opts?)`** is new — it reverts a
  versioned global's `_status` to draft without changing field data,
  mirroring `crap.collections.unpublish`.

Additive; no action needed.

## Reference

- `CHANGELOG.md` at the project root — the full alpha.10 entry with
  every change.
- [crap.collections](../lua-api/collections.md)
- [crap.hooks](../lua-api/hooks.md)
- [Fields overview](../fields/overview.md#reserved-field-names)
