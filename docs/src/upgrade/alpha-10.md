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


## Job payload field renamed to `data`

`TriggerJobRequest.data_json` and `JobRunInfo.data_json` are now
`data` on the gRPC wire, matching what MCP and Lua have always called it.
The field numbers and types are unchanged, so binary clients are
unaffected — but JSON/grpcurl callers must rename the key:

```diff
 grpcurl -plaintext -d '{
     "slug": "reindex",
-    "data_json": "{\"force\": true}"
+    "data": "{\"force\": true}"
 }' localhost:50051 crap.ContentAPI/TriggerJob
```

The same rename applies when reading a run: `JobRunInfo.data_json` →
`data`. This lands now because the field name is frozen at the tag —
after it, jobs could never share one argument vocabulary with the rest
of the API.

## TL;DR

- **Back up first — this upgrade is one-way.** The first startup rewrites
  stored data in place (canonical text and email, typed values in nested rows,
  nested timezone dates as UTC, Postgres checkbox columns) and nothing converts
  it back. Run `crap-cms backup` (or `pg_dump`) before swapping the binary; see
  *Before you upgrade* below.
- **Do one thing before you swap the binary.** If your Lua encrypts data with
  `crap.crypto.encrypt` and you never set `[auth] secret`, decrypt it *on
  alpha.9* — the key changes and old ciphertext becomes unrecoverable
  (item 18). Everyone else: replace the binary and restart. DB schema
  migrations apply automatically; no manual SQL.
- **Warn your users about live email links.** Reset tokens, verification
  tokens, and MFA codes are now stored hashed, with no migration — every link
  already sent stops working on restart. Users request a new one; verification
  links now have a self-service page (item 19).
- **Read clients: unreadable fields are no longer filterable.** Filtering or
  sorting on a `hidden` field, or one denied by `access.read`, now fails
  instead of quietly leaking the value it was meant to strip (item 20).
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
- **API clients: two read shapes are now consistent across surfaces.** Scalar
  `has_many` lists read back as typed arrays (not a raw string) on gRPC / Lua /
  MCP, and an unset relationship-population `depth` now defaults to
  `[depth] default_depth` (`1`) everywhere instead of `0` on gRPC `Find` / Lua.
  Handle the array, and pass `depth = 0` if you want IDs only (see Behavior
  changes).

- **Schema authors: non-default-locale writes reject shared fields.** A
  write under `locale = "de"` that includes a non-localized field is now a
  validation error naming the field (it used to be silently dropped). Split
  the write, or mark the field `localized` (item 11).
- **Auth authors: methods got stricter, strategies got transactions.**
  Strategies must declare `authenticate` and `activates_on`; `methods = {}`
  and unknown `surfaces` names are load errors; strategy writes roll back
  on failed attempts (items 12–13).
- **Event subscribers: three new operations.** `undelete`, `unpublish` and
  `restore` no longer arrive as `update` — regenerate stubs and extend your
  operation switches (item 16).
- **Custom storage: remove the `url` handler and `public_url_base`.** The
  direct-URL limb is gone; everything serves via `/uploads/…` (item 17).
- **Hook authors: `after_read` lost CRUD access** — the contract always said
  so, it just wasn't enforced (item 21).
- **Schema authors: a collection and a global can't share a slug** — a load
  error now, so the server won't boot until one is renamed (item 22).
- **Write clients: relationship values must be id strings**, `NaN`/`Inf` are
  rejected on the gRPC wire, and MCP writes honor a present `null` instead of
  dropping it (item 23).
- **Subscribers: a draft save now reports the draft**, not the untouched
  published row, and a typo'd operation name errors instead of opening a dead
  stream (item 24).
- **MCP clients: an unknown tool is a `-32602` protocol error**, and a
  collection you can't see is indistinguishable from one that doesn't exist
  (item 25).
- **Access-rule authors: field `update` rules now see the stored document**
  in `ctx.document`. Read incoming values from `ctx.data` (item 26).
- **Multi-node operators: set `[auth] secret` explicitly and upgrade all nodes
  together.** With Redis in the config, loading the config fails without a
  secret; keep `rate_limit_prefix` outside the cache namespace (item 27).
- **Event subscribers: detect gaps per `publisher`**, not on `sequence` alone
  (item 28).
- **API clients: refresh tokens before they expire** — the 60-second grace
  period is gone (item 29).
- **Date writers: a local time skipped by a daylight-saving change is
  rejected** instead of being stored hours off (item 30).
- **API clients: timezone dates inside blocks and nested rows come back as
  UTC**, like every other timezone date; existing rows are converted at
  startup (item 32).
- **Duplicate emails or unique text values block startup.** Email and text
  values are now stored in one canonical form; if two values of a unique field
  or unique index become identical, startup lists them so you can resolve the
  duplicate (item 33).
- **Backups and exports carry more.** `crap-cms backup` includes the generated
  auth secret — keep backups private — and exports include trashed documents;
  add `--include-credentials` to move users between installations (item 34).
- **MCP `list_versions` snapshots read as documents.** No per-locale keys, one
  value per localized field, hidden fields removed (item 35).
- **Values inside blocks and nested rows are typed** — booleans, numbers and
  arrays instead of the strings the admin form sent; converted once at startup
  (item 36).
- **Checkbox values are validated.** A checkbox accepts a boolean, a
  recognized spelling or a number; anything else (`"maybe"`, a list, an
  object) is now a validation error instead of silently unchecked (item 37).
- **Typed clients: regenerate `typegen client`.** Every field of a generated
  read document is optional now, read documents gained `_status`,
  `_deleted_at` and `<name>_tz`, and localized collections get a
  `locale = "all"` read type (see Generated client types).
- **Filter authors: a backslash escapes `%` and `_` in `like`.** Double a
  literal backslash; a pattern ending in a lone backslash is rejected (see
  Behavior changes).
- **CLI scripts: `crap-cms user delete` trashes users of a soft-delete
  collection**; on other collections it refuses users other documents still
  reference (see Behavior changes).
- **Operators: keep `data/` on a filesystem with file locks.** `serve`, `work`,
  `mcp` and every CLI command that opens the database stop at startup when they
  can't take `data/crap.lock` (see Behavior changes).

## Required action items

### Before you upgrade: take a backup (there is no way back)

Do this first, before item 18 and before you swap the binary.

```bash
crap-cms backup --include-uploads      # → <config_dir>/backups/backup-<timestamp>/
```

`crap-cms backup` copies the **SQLite** database file and, with
`--include-uploads`, the local `uploads/` directory. Two caveats:

- **Postgres:** `backup` refuses to run and tells you to use `pg_dump`. Take
  that dump yourself.
- **Non-local upload storage:** with `[upload] storage` set to anything but
  `local`, `--include-uploads` prints a notice and skips the files — back them
  up with that service (S3 versioning/snapshot, or your custom backend's own
  tooling).

The backup also carries `data/.jwt_secret` when one exists, so keep it as
private as the secret (item 34). To go back on SQLite: `crap-cms restore
<backup-dir> --confirm` (add `--include-uploads` if the archive has them);
`restore` is SQLite-only, so on Postgres restore the `pg_dump` you took.

**Why this is not optional.** The first startup on alpha.10 rewrites stored
data in place, inside the migration transaction, and nothing converts it back:

- **Text and email values** are rewritten NFC-normalized, and emails also
  trimmed and lowercased — in columns, localized columns, array rows and
  values nested in rows (item 33).
- **Values inside blocks and nested rows** are rewritten in typed form: a
  checkbox as `true`/`false`, a number as a number, a multi-value field as an
  array, a blank as `null` (item 36).
- **Timezone dates nested in JSON rows** are rewritten as UTC; the wall-clock
  digits that were stored are gone (item 32).
- **Postgres checkbox columns** are retyped `BIGINT` → `SMALLINT`.
- **Legacy SQLite timestamps** (`YYYY-MM-DD HH:MM:SS`) are rewritten as ISO
  8601 (`…THH:MM:SS.sssZ`).

(`_ref_count` is also recomputed for every document, but that is derived data
and recomputed again by any later version.)

alpha.9 has no migration that undoes any of this — its readers and writers
assume the old forms (wall-clock nested dates, string values in rows, emails
as typed). Password-reset, verification and MFA codes issued after the upgrade
are stored hashed and would not verify there either (item 19). **Downgrading
means restoring the backup**, and everything written since is lost with it — so
take the backup immediately before the upgrade, not the night before.

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
- **Globals reject `access.create` / `access.delete` / `access.trash` /
  `access.unlock`.** A global has a single row with only `get`/`update`
  operations, and no account to lock, so these access keys never fired —
  they were silently ignored and now error at load. Use `access.read`,
  `access.draft`, `access.update`, or
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

### 6a. Rename job slugs, richtext node names, and block types to valid slugs

Three registration surfaces that previously accepted looser identifiers now
enforce the standard slug rule (lowercase ASCII letters, digits, and
underscores; not starting with an underscore):

- **`crap.jobs.define(slug, …)`** — a job slug with a hyphen, uppercase
  letter, or space now errors at load. Rename e.g. `send-digest` →
  `send_digest`.
- **`crap.richtext.register_node(name, …)`** — a node name with uppercase
  or non-ASCII characters (the old check was Unicode-aware) now errors.
  Rename to a lowercase ASCII slug.
- **Block `type` in a `blocks` field** — a block `type` (the `_block_type`
  discriminator) was the one identifier accepted verbatim; a hyphenated /
  spaced / camelCase type now errors at load. Rename e.g. `hero-image` →
  `hero_image`.

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
- **Over-long generated identifiers** (>63 bytes — Postgres's limit) are
  rejected at migration — shorten very long collection/group/field/locale
  name combinations. The error names the offending identifier. The check
  runs on **every** backend, before any table is created, so an SQLite
  project fails here rather than at its first Postgres deployment, where
  the identifier would be silently truncated and could collide.

### 6d. Stricter data validation (values that used to slip through now error)

The validation checks that ran only at the top level now also run where
the same field is nested in an array/blocks row, and the numeric checks
now cover `has_many` lists:

- **`date` `min_date` / `max_date` are enforced inside array/blocks rows.**
  A `date` sub-field with bounds now rejects an out-of-range value in a
  nested row, exactly as at the top level (previously only its *format*
  was checked there).
- **`number` `has_many` lists reject invalid elements.** A `NaN` / `Infinity`
  element, a fractional element on an `integer` field, or a non-numeric
  element (which was silently dropped on write) is now a validation error —
  matching the single-value `number` rule (6b).
- **Not an error, a relaxation:** a *draft* save no longer enforces
  `min_rows` / `max_rows` on a scalar `has_many` field, matching how draft
  saves already skipped row counts for array/blocks/relationship fields.

If a client submitted any of the now-rejected shapes, fix the value (or
save as a draft where the count rule is relaxed).

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

### 10. MCP surface: stricter tool inputs and reserved slugs

Three changes affect MCP clients:

- **Collection/global slugs can't begin with `many_` or `by_id_`.** These
  collide with the MCP tool-name grammar (`create_many_<slug>`), so they're
  rejected at load. **Action:** rename any such collection/global.
- **Write tools reject unknown field keys.** `create` / `update` /
  `create_many` / `update_many` / `validate` (and global equivalents) now error
  on a data key that isn't a declared field or a reserved meta-key
  (`id`/`locale`/`draft`/`events`/`password`). **Action:** stop sending stray
  keys; a misspelled field now errors instead of being silently dropped.
- **`create_many` now accepts a policy-checked `password` on auth collections;
  `update_many` still rejects one.** A per-item `password` in `create_many` is
  validated against `[auth.password_policy]` and hashed per document (parity with
  single `create`), so bulk-seeding auth users with distinct passwords works in
  one call. `update_many` rejects a `password` because it applies one value to
  many rows. On non-auth collections a `password` field is ordinary data.

Also: MCP `update_global` now honours `draft`, `read_config_file` redacts
`crap.toml` secrets, and the JSON-RPC layer is stricter (`jsonrpc` must be
`"2.0"`, notifications get no reply, error responses carry `id: null`). No
action needed for these.

### 11. Non-default-locale writes reject shared fields

`update(id, { title = x, slug = y }, { locale = "de" })` with a non-localized
`slug` used to succeed while silently discarding `slug`. It is now a
validation error listing every offending field, on all write surfaces.
**Fix:** write shared fields under the default locale (or without a `locale`),
or mark the field `localized` if it should vary per locale.

### 12. Auth `methods` are strict (and a silent-disable bug is fixed)

Load errors now, previously tolerated:

- a `strategy` without a non-empty `authenticate` hook ref, or without
  `activates_on` (`{ header = "x-..." }` or `{ always = true }`) — both used
  to be silently dropped, and a dropped-to-empty list silently gained the
  FULL default method set;
- an explicit empty `methods = {}` (omit the key to get the defaults);
- a wrong-typed `methods` value or a non-table entry in the list;
- an unknown surface name in `surfaces` (typos like `"gprc"` used to be
  silently skipped). New: `surfaces = "all"` means every current **and
  future** surface.

Also note a fixed defaults bug that may change behavior on upgrade:
`auth = { methods = {...} }` **without** `enabled = true` used to parse as
*disabled* (and `password_login` without `forgot_password` as
forgot-password-off) because a missing boolean read as `false`. Both now take
their documented `true` defaults — if you relied on the accidental
disablement, write `enabled = false` explicitly.

### 13. Custom auth strategy writes are transactional

`authenticate` runs inside a transaction that commits only when it returns a
user; on `nil` or an error every write it made rolls back. "Find or create
user" flows keep working (the create commits with the successful login). For
failed-attempt bookkeeping use the rate limiters and `crap.log` — persistent
writes on failed, unauthenticated attempts were an attacker-driven-growth
vector and are gone by design.

### 14. `has_many` lives inside the `relationship` table

On a relationship/upload field using `relationship = { ... }`, a top-level
`has_many = true` next to it is now a load error — it silently stored a plain
JSON array (no junction table, no populate, no ref-counting) while reading
like the real switch. Move the flag: `relationship = { collection = ...,
has_many = true }`. The legacy flat `relation_to` syntax keeps its top-level
flag.

### 15. Filter, select and tab strictness

- `exists` / `not_exists` accept **only `true`** on every surface (Lua,
  gRPC/MCP JSON, admin list URLs, access constraints). `{ exists = false }`
  used to be silently dropped (Lua) or read as the inverted `IS NOT NULL`
  (wire) — use `not_exists = true` for IS NULL.
- Unknown `select` names are errors (they used to silently select nothing).
  Valid: top-level field names + `id` / `created_at` / `updated_at` /
  `_status`.
- Every tab in a `tabs` field requires a `label`.
- **An empty group inside an `or` is a hard error.** `{"or": [{"status":
  "a"}, {}]}` — and `{"or": []}` — used to be accepted as a vacuously-true
  group, silently widening the whole `or` to match **every row**. On a
  `delete_many` that selects the entire collection. It arises naturally in
  Lua, where `{ tenant = nil }` *is* `{}`, so check any filter built from
  optional values. In an access constraint the same input now fails closed
  and denies.
- **A filter value that doesn't fit the field's type is a validation error.**
  `where = { price = { greater_than = "abc" } }` on a Number field, or a
  non-boolean on a Checkbox, used to fall back to a text comparison — which
  SQLite absorbed silently by affinity and Postgres rejected at execution
  time as an opaque 500. Every surface now returns a 400 naming the field.
  A stale or forged keyset cursor whose sort value can't bind to the sort
  column is rejected the same way, so old cursors may need to be discarded.
  (`like` / `contains` on a Number or Checkbox now cast to text, so those
  keep working on Postgres.)

### 16. Event subscribers: `undelete`, `unpublish`, `restore`

Lifecycle mutations no longer masquerade as `update`. The gRPC
`MutationOperation` enum gained `UNDELETE` / `UNPUBLISH` / `RESTORE`
(regenerate stubs), the SSE payload and the Lua `live` / `before_broadcast`
contexts see the new strings, and an empty `SubscribeRequest.operations`
means all **six** operations. Extend any switch over the operation; a
subscriber that filtered to `["update"]` explicitly will no longer receive
lifecycle mutations — add the new names if you want them.

Related gRPC detail: `Me` now resolves through the shared auth evaluator, so
its error details changed (locked-account token → `PERMISSION_DENIED`
"Account locked"; deleted user → `UNAUTHENTICATED` instead of
`NOT_FOUND`).

### 17. Custom storage: `url` handler and `public_url_base` removed

The direct/public-URL limb had no production caller — every stored URL and
every byte served goes through the `/uploads/…` proxy. Remove `url = ...`
from `crap.storage.register` (now an unknown-key load error) and
`public_url_base` from `[upload.s3]` (unknown config keys are fatal). A
CDN/direct-link story returns as an explicit signed-URL design.

### 18. Before you upgrade: rescue `crap.crypto` data

**Do this on alpha.9, before you swap the binary.** Skip it and the data is
unrecoverable.

If you use `crap.crypto.encrypt` / `decrypt` **and** you never set
`[auth] secret` in `crap.toml`, alpha.9 derived the AES key from the *empty*
config value — the SHA-256 of the empty string, a key anyone could compute.
alpha.10 fixes that by resolving the generated `data/.jwt_secret` at config
load, so every consumer keys off the same real secret. The key therefore
changes, and ciphertext written under the old one no longer decrypts.

You are affected only if **both** hold:

- your Lua calls `crap.crypto.encrypt` and stores the result, and
- `[auth] secret` is absent or empty in `crap.toml`.

Check with:

```bash
grep -n 'secret' crap.toml          # is [auth] secret set?
grep -rn 'crypto.encrypt' hooks/ collections/ globals/ init.lua
```

If both hold, decrypt on alpha.9 and re-encrypt after upgrading. A one-off
job is the simplest vehicle — define it, trigger it, then delete it:

```lua
-- jobs/rescue_crypto.lua, on alpha.9
local M = {}

M.run = crap.any.job_handler(function(_context)
  local result = crap.collections.records.find({
    limit = 500,
    override_access = true,
  })

  for _, doc in ipairs(result.documents) do
    if doc.secret_blob then
      crap.collections.records.update(doc.id, {
        secret_plain = crap.crypto.decrypt(doc.secret_blob),
      }, { override_access = true })
    end
  end
end)

crap.jobs.define("rescue_crypto", { handler = "jobs.rescue_crypto.run" })

return M
```

```bash
crap-cms jobs trigger rescue_crypto     # still on alpha.9
# upgrade, then run the mirror job that re-encrypts secret_plain
```

Page through everything — the snippet caps at 500 for brevity. Store the
plaintext in a field you delete afterwards, and keep the window short.

Setting an explicit `[auth] secret` before upgrading does *not* help: that
changes the key too. The old key was `SHA-256("")` and nothing else
reproduces it, because the helpers now refuse an empty secret outright. Do
set one after the upgrade if you want the key pinned to something you control
rather than to a generated file you must back up.

Deployments that already set `[auth] secret` are unaffected: their key is
unchanged. TOTP enrollment and signed upload URLs are unaffected on any
deployment — both are newer than alpha.9.

### 19. Tell your users: outstanding reset and verification links stop working

Password-reset tokens, email-verification tokens, and MFA codes are now
stored one-way instead of in the clear, so a database read, a backup, or a
stray query log no longer hands over a working credential. Tokens are stored
as a digest; an MFA code is keyed with `[auth] secret`, because six digits is
a small enough space that a bare digest of one could be inverted by table
lookup. The rendered mail is also dropped from the job queue once the send
completes, so the link does not linger there after delivery.

There is no migration, by design: hashing an existing plaintext token would
defeat the point of the change. **Every link and code already in someone's
inbox stops working the moment you restart.** In practice that is a window
of one reset-token lifetime (`[auth] reset_token_expiry`, one hour by
default) and up to 24 hours for verification links.

No schema change is involved — the columns keep their names and widths — so
nothing to run. What you should do:

- Upgrade at a quiet hour if you can.
- Expect a small burst of "my link doesn't work" reports. The answer is
  "request a new one".
- Users who need a new verification link can now get one themselves at
  `/admin/resend-verification` (see Additive features), which did not exist
  before. Password resets already had `/admin/forgot-password`.

### 20. Read clients: filters and sorts on unreadable fields are rejected

A field marked `hidden = true`, or one whose `access.read` rule denies the
caller, can no longer be used as a filter or sort target. The read fails
with `Cannot filter or sort on '<field>': the field is not readable in this
context` — `PERMISSION_DENIED` on gRPC, a 403 on the admin surface.

This closes a leak rather than tightening a preference: a `like` filter or an
ordering over a stripped field let a caller recover the value the read strip
had just removed, one query at a time.

**Action:** audit any client, saved view, or bookmarked admin URL that
filters or sorts on a field you have since marked `hidden` or gated with
`access.read`. Those calls now fail loudly instead of quietly leaking. If a
field genuinely needs to be filterable by everyone, drop the `hidden` flag or
the `access.read` rule.

Related, same reasoning: hidden and read-gated fields are no longer part of
the **default** full-text index, so a bare `search` no longer matches their
contents. To keep a read-gated field searchable, name it explicitly in
`admin.list_searchable_fields`. The index is dropped and rebuilt from the
table on the first migration run, so there is nothing to reindex by hand.

### 21. Hook authors: `after_read` can no longer call `crap.*` CRUD

The contract always said `after_read` has no database access. It did not
enforce it: the hook inherited the read's transaction, and because
`after_read` is fail-open, a write from a hook that then errored still
committed. The call now raises on every surface.

**Action:** if an `after_read` hook reads or writes through
`crap.collections.*` / `crap.globals.*`, move the lookup into `before_read`
and hand the result over through `ctx.context`, which is shared across one
read:

```lua
-- before_read: do the query once, stash it
function M.load_authors(ctx)
  ctx.context.authors = crap.collections.users.find({
    where = { role = "author" },
    override_access = true,
  })
  return ctx
end

-- after_read: read from the stash, no CRUD
function M.attach_author(ctx)
  ctx.data.author_name = lookup(ctx.context.authors, ctx.data.author_id)
  return ctx
end
```

`before_read` runs once per read and keeps full CRUD access, so a per-document
loop in `after_read` becomes one query up front — usually faster too. For a
plain relationship join, prefer population (`depth`) over either hook.

### 22. Schema authors: a collection and a global may not share a slug

A slug now identifies exactly one thing. Defining a collection and a global
under the same name is a load error, so the server will not boot until you
rename one.

This is a security fix as much as a naming rule: the MCP surface keys its
exposure and its `access.mcp` gate by slug alone, so a global that
`access.mcp` denied stayed executable through the identically-named
collection's tools.

**Action:** none unless the loader tells you otherwise. Re-defining the *same*
kind under one slug is still legal — that is the documented plugin pattern for
extending a collection, and it is unaffected.

### 23. Write clients: three values that used to be accepted now error

Each of these used to be coerced or ignored and is now a validation error.
All three were silently corrupting data.

- **A relationship or upload value must be an id string.** A number, a
  boolean, a populated document object (a `depth > 0` read sent straight
  back), or a list containing non-strings used to be stored as text: a
  dangling reference that was never existence-checked, never ref-counted, and
  unresolvable on the next populate. Write `"abc123"`, or `"posts/abc123"` for
  a polymorphic target. If your client round-trips a populated read back into
  a write, map the objects back to ids first.
- **`NaN` and `±Inf` are rejected on the gRPC wire.** A non-finite
  `double_value` was converted to `null`, which under the present-null
  contract *cleared* the field. It is now `INVALID_ARGUMENT`, matching Lua.
- **MCP write tools no longer drop `null`.** `update_posts {"id": …,
  "subtitle": null}` used to keep the old value; a present null now clears the
  field, the contract gRPC and Lua already had. Removing a translation —
  documented as writing that locale's fields as null — was impossible over MCP
  before. An unknown field name is now rejected whatever its value.

### 24. Subscribers and draft writers: two contract corrections

- **A draft save now reports the draft.** `update(..., draft = true)` returned
  the untouched *published* row, so the operation's return value, the
  `after_change` hook, and the emitted event all carried the pre-edit
  document. All three now carry the stored draft, stamped
  `_status = "draft"`. The published row is still untouched. **Action:** a
  subscriber or hook that read the returned document expecting published
  content must branch on `_status`.
- **Subscribe rejects an unknown operation name.** `operations: ["creat"]`
  used to open a stream that connected cleanly and then never delivered
  anything. It is now an error at subscribe time. **Action:** none, unless you
  had such a typo — in which case this is the first time you will hear about
  it.

### 25. MCP clients: unknown tools are protocol errors

Two changes to how the Model Context Protocol server reports a tool it will
not run:

- **An unknown tool name is now a JSON-RPC error**, code `-32602`, message
  `Unknown tool: <name>` — not a successful response carrying
  `isError: true`. This is what the MCP specification requires. A tool that
  *ran* and failed still reports in-band with `isError: true`, unchanged.
  `resources/read` on an unknown URI now answers `-32002` instead of `-32603`.
- **A collection you cannot see is reported as if it did not exist.**
  Whether a collection is filtered out by `include_collections` /
  `exclude_collections`, hidden by its `access.mcp` rule, or simply absent,
  a direct tool call now gets the identical `Unknown tool` error. Previously
  the first two answered `Tool not available: <slug>`, which let a client walk
  a slug list and learn which collections it was being kept away from.

**Action:** a client that distinguished those cases by message text needs to
stop. A client that checked `isError` on the result now has to handle a
protocol-level error for the unknown-tool case as well.

### 26. Access-rule authors: field `update` rules see the stored document

A field-level `access.update` rule used to receive the *incoming* write as
`ctx.document`. It now receives the stored document, as the documentation
always said — on single updates, bulk updates, global updates, and validation
dry-runs. `ctx.data` is still the incoming value's level. `access.create` rules
are unchanged: there is no stored document yet, so `ctx.document` is the
incoming one.

This closes a hole: a rule like the one below passed for any caller who put
their own id into `owner` in the same request.

```lua
-- Only the document's owner may change the salary.
return crap.any.access(function(ctx)
  return ctx.user ~= nil and ctx.document ~= nil
    and ctx.document.owner == ctx.user.id
end)
```

**Action:** find field `access.update` rules that read `ctx.document`. A rule
that meant "the stored value" needs no change. A rule that meant "the value
being written" must read `ctx.data` instead.

### 27. Multi-node operators: explicit secret, separate Redis namespaces

Two configurations that used to load now fail — in the server and in every CLI
command that reads `crap.toml`:

- **An empty `[auth] secret` while a Redis cache, event transport, or
  rate-limit backend is configured.** Each node would generate its own secret,
  so a session from one node fails on the next, and MFA codes, TOTP secrets,
  `crap.crypto` values and signed URLs made on one node fail on the others.
  On Postgres without Redis the config loads, but a warning is logged.
- **An `auth.rate_limit_prefix` that overlaps the cache namespace on the same
  Redis.** Cache keys now live under `{cache.prefix}cache:`, and a cache clear
  deletes only that namespace. Previously a clear deleted every key under the
  cache prefix — with the defaults, including every login lockout.

**Action:**

1. **Set `[auth] secret`, identically on every node.** Which value depends on
   what you already have:
   - If a node already runs with a generated `data/.jwt_secret`, copy *that*
     file's contents into `secret` (for example via
     `secret = "${JWT_SECRET}"`) on every node. Existing sessions, MFA codes,
     TOTP secrets and `crap.crypto` values made on that node keep working.
     Nodes that had generated a different file lose their sessions once.
   - Only on a fresh deployment, generate a new value
     (`openssl rand -hex 32`).
   - If you follow item 18 (re-encrypting `crap.crypto` data), set the final
     secret **first** and re-encrypt under it. Data re-encrypted under a
     secret you later replace is unrecoverable again.
2. If you changed `rate_limit_prefix` or `[cache] prefix`, make sure neither
   is a prefix of the other's namespace. The defaults (`crap:rl:` and
   `crap:cache:`) are fine.
3. **Upgrade all nodes that share a Redis together** (stop the old version on
   every node, then start the new one). During a mixed rollout, old nodes still
   clear every key under the cache prefix — wiping login lockouts and the new
   nodes' cache — while new nodes no longer clear the old nodes' cache keys, so
   old nodes can serve stale related documents until they are upgraded.
4. Optional, once every node runs the new version: delete the cache entries the
   previous version wrote. They are only ever read by old nodes, and without
   `max_age_secs` they never expire:

   ```bash
   redis-cli --scan --pattern 'crap:populate:*' | xargs -r redis-cli del
   ```

   Replace `crap:` with your `[cache] prefix`. This pattern matches only the
   old cache entries — not rate-limit counters, and not the live-update
   channels (those are not keys).

While you are editing the config of a multi-node deployment, also check the
two constraints now spelled out in
[Multi-Server Deployment](../deployment/multi-server.md#configuration-notes):
every scheduler node needs the same `[jobs] heartbeat_interval`, and a rollout
that changes indexes must finish before an older node restarts.

### 28. Event subscribers: detect gaps per publisher

Every server process numbers the events it publishes from 1. On a shared
Redis event transport, events from several nodes arrive on one stream, so
`sequence` alone is neither unique nor gap-free. Events now carry a
`publisher` id — gRPC `MutationEvent.publisher` (field 8) and `publisher` in
the admin SSE payload — and `sequence` is monotonic within each publisher.

**Action:** if you detect dropped events, track the last `sequence` per
`publisher` and compare within it. gRPC clients: regenerate from
`proto/content.proto` to get the field. A single-node deployment sees one
`publisher` per process start; the logic above covers restarts too. While a
multi-node rollout is in progress, events from nodes still on the previous
version arrive with an empty `publisher` — treat those as one unordered source
until every node is upgraded.

### 29. API clients: no grace period after a token expires

Bearer tokens, gRPC session tokens and MFA-pending tokens were accepted for up
to 60 seconds after their `exp`. They are now rejected at `exp`, like reset
links, MFA codes and signed URLs. Admin sessions refreshed near
`auth.session_absolute_max_age` also get a shorter token, so the refresh never
extends a session past that ceiling.

**Action:** refresh a token before its `exp`, not after a request fails. If a
client's clock drifts, refresh a little earlier.

### 30. Date writers: nonexistent local times are rejected

For a date field with `timezone = true`, a local time inside a daylight-saving
gap — `2024-03-31T02:30` in `Europe/Berlin` — does not exist. It used to be
stored as if it were UTC, hours off, without an error. It is now a validation
error with the key `validation.nonexistent_local_time`.

**Action:** if you import dates in bulk, handle the new error (for example by
moving the time past the gap). The message ships in English and German; to
show it in another language, add `validation.nonexistent_local_time` to that
language's translation file (it receives `field`, `value` and `timezone`).
Without it, the English text is shown.

### 31. CLI scripts: pass passwords on standard input

`crap-cms user create` and `crap-cms user change-password` accept
`--password-stdin`, which reads the password from the first line of standard
input. `-p <PASSWORD>` still works but now warns: an argument is visible to
other local users in the process list.

**Action:** none required. In provisioning scripts, prefer:

```bash
printf '%s\n' "$ADMIN_PASSWORD" | crap-cms user create -e admin@example.com --password-stdin
```

### 32. API clients: nested timezone dates are UTC

A date field with `timezone = true` stores its value as UTC plus the IANA zone
in a `{field}_tz` companion. That was true for top-level fields and for the
direct fields of an array row — but a date inside a **blocks row**, inside a
**group within an array or blocks row**, or inside a **nested array row** was
stored as the wall-clock digits entered (`2024-01-15T09:00`). Those are now
converted to UTC on write (`2024-01-15T08:00:00.000Z` for `Europe/Berlin`), and
existing rows are converted once at the first startup.

**Action:**

- If a client reads such nested dates, treat them like top-level timezone
  dates: the value is UTC; use the `_tz` companion next to it to display local
  time.
- If a client *writes* them, nothing changes: send local wall-clock time plus
  the `_tz` value, or a value with an explicit offset.
- Filters on nested dates now compare UTC with UTC. A filter written against
  the old local digits needs the UTC value instead.
- Large databases: the first startup walks every blocks and array join table
  once.

### 33. Email and text are stored in canonical form

Email fields are now stored **trimmed, NFC-normalized and lowercased**, and
Text, Textarea and Email values are NFC-normalized on every write (including
`crap-cms import`). Logins, password resets, verification, uniqueness and
filters on these fields all compare that canonical form. This fixes accounts
with non-ASCII capital letters that could not log in on SQLite, and duplicates
that differed only by case or Unicode form.

Stored email and text values are converted at startup: their columns,
localized columns, array rows, and values inside blocks and nested rows. The
conversion runs again for a collection or global whose email or text fields
change — a field added, or retyped to `email` — so no stored value is left in
another form. Version snapshots keep the form they were taken in; restoring an
older version stores its values in canonical form.

**Action:**

- **If startup stops with "Email and text values in '…' are now compared in
  one canonical form":** two live documents hold the same value, once compared
  this way, in a unique field or unique index (for example `ÄRGER@example.com`
  and `ärger@example.com`, or a title typed with a combining accent and one
  without). The message lists the field or index, the value and the document
  ids. Merge the documents or change one value with the previous version or
  directly in the database, then start again. A trashed document counts
  toward a unique index spanning several fields, but not toward a unique
  field.
- Clients that compared stored email values case-sensitively will now see
  lowercase addresses.
- If a hook or filter matched an email with different casing, match the
  lowercase form.
- Large databases: this is the widest of the first-startup passes — it reads
  every email, text and textarea value of every collection and global,
  including their localized columns and their array, blocks and nested rows.
  Expect the first restart to take noticeably longer than the others.

### 34. Backups and exports: secret, credentials and trash

- **`crap-cms backup` now includes `data/.jwt_secret`** (when present), and
  `restore` writes it back. Keep backups as private as the secret itself.
  Backups made by earlier versions don't contain it: if you rely on a
  generated secret, copy `data/.jwt_secret` alongside those backups yourself.
- **`crap-cms export --include-credentials`** adds each account's password
  hash, lock, session version, verification and TOTP state to the export.
  Without the flag, exports stay credential-free and `import` warns about
  accounts left without a password. Treat an export made with the flag like a
  database dump.
- **Exports now include trashed documents**, timezone companions, and every
  locale's rows of a localized array, blocks or has-many field (as
  `{ "<locale>": rows }`); `import` restores them.
- **`import` runs in one transaction.** A failure leaves nothing imported,
  where it used to keep the collections imported before the failing one.

**Action:**

- To move users between installations with an export, add
  `--include-credentials`.
- Accounts with TOTP enrollment only import into an installation that uses the
  same auth secret: `import` refuses them otherwise, naming the accounts. Copy
  the secret over first (a backup carries a generated one), or export without
  `--include-credentials` and have those users enroll again.
- Scripts that imported collection by collection to recover from a partial
  failure can import the whole file again after fixing the cause.

### 35. MCP clients: version snapshots read as documents

`list_versions` returned each version's `snapshot` as stored: a localized field
appeared under per-locale keys such as `title__en` and `title__pt_BR`, and a
hidden localized field was not removed. A snapshot now has the shape of a read
of the document in the default locale — groups nested, each localized field as
one value — with hidden and read-denied fields removed.

**Action:**

- If a client reads a version's content, read `title` instead of
  `title__<locale>`.
- A snapshot comes back in the default locale unless the tool call passes
  `locale`: a locale code returns that locale's values, and `"all"` returns
  every locale as `{"en": …, "de": …}` per field, as `find_by_id` does.
- Restoring a version is unchanged: it still writes back every locale.

### 36. API clients: values inside blocks and nested rows are typed

Values inside a blocks row, and inside any group, array or blocks nested in a
row, used to be stored as the writing surface sent them: rows saved in the admin
form held strings (`"on"`, `"3"`, a list as JSON text), rows written over gRPC or
Lua held typed values. They are now stored in one typed form — a checkbox as
`true`/`false`, a number as a number, a multi-value field as an array, a blank
value as `null` (timezone dates are covered by item 32) — and existing rows are
converted once at the first startup.

**Action:**

- If a client or hook reads nested checkbox, number or multi-value values as
  strings, read them as booleans, numbers and arrays.
- Large databases: the first startup walks every blocks and array join table
  once.

### 37. API clients: checkbox values are validated

A checkbox used to accept any value and store everything it didn't recognize
as unchecked, so `"maybe"`, `[]` or `{}` passed silently. A checkbox value now
has to be a boolean, a recognized spelling (`1`/`0`, `true`/`false`,
`yes`/`no`, `on`/`off`, any case, surrounding whitespace ignored) or a number
(any value other than `0` is checked; `"2"` and `2` agree). Anything else is a
validation error on every surface, at the top level and inside groups, arrays
and blocks. `null` and an absent key are unchanged (`required` decides those).

**Action:** if a client sent something else to a checkbox, send a boolean.

### 38. Hook authors: the document id is `ctx.id`, not `ctx.document_id`

Lua hook contexts now expose the affected document's id as **`ctx.id`**,
matching the field-hook, validator and access contexts, which always spelled it
that way. The old `ctx.document_id` key is gone — a hook that still reads it
gets `nil`, silently, with no error. This is the one rename in this release
that fails quietly, so grep for it:

```bash
grep -rn 'ctx\.document_id' hooks/ collections/ globals/ jobs/ routes/ init.lua
```

```diff
-  local id = ctx.document_id
+  local id = ctx.id
```

`ctx.id` is set across the write lifecycle — `update` / `delete` before- and
after-hooks, `after_change` on create (the freshly assigned id), `after_read`,
and `before_broadcast` — plus `"default"` for globals. It is `nil` in create's
*before*-hooks, where no row exists yet, so keep any nil guard you already had.

### 39. Access-rule authors: empty constraint tables deny, and only equality operators are allowed

Two changes to what a collection or global `access.*` function may return.
Both are enforced at the single chokepoint every access evaluation passes
through, so they apply uniformly to direct reads, Lua CRUD, relationship and
join population, and live event streams.

**An access function that returns a table now needs at least one filter.** In
Lua, `{ tenant_id = ctx.user.tenant_id }` collapses to the empty table `{}`
when `tenant_id` is `nil` — the constructor drops nil-valued keys. That empty
constraint used to AND *nothing* into the query, so a rule written to restrict
matched every row. A table that produces no filters (an empty table, a
nil-valued key, an empty operator table like `{ score = {} }`) is now
**Denied**, with a warning naming the rule.

```diff
  return function(ctx)
+     if ctx.user == nil or ctx.user.tenant_id == nil then
+         return false
+     end
      return { tenant_id = ctx.user.tenant_id }
  end
```

If you used `return {}` to mean "allow everything", return `true` instead.

**Only equality and membership operators are accepted.** An access constraint
may use `equals`, `not_equals`, `in`, `not_in`, `exists`, `not_exists`.
`like`, `contains`, `greater_than`, `greater_than_or_equal`, `less_than` and
`less_than_or_equal` are a hard error naming the operator and field — access
constraints are matched both in SQL and in memory (live events, populated
targets), and those operators can disagree between the two in a way that
biases toward showing the row. Re-model them as an exact match or a membership
set; *user* filters keep the full operator set.

Rejected for the same reason, in the same place:

- a dotted path (`author.id`) — denormalize to a flat own column
  (`author_id`);
- a constraint on a **localized** field — it is stored per locale, so the
  target is ambiguous;
- a constraint on a system column (`_status` and friends) — with one
  exception: a bulk update on a drafts collection may constrain `_status`,
  since the operation injects it itself.

User-facing `where` filters are unaffected by all of this.

### 40. Auth collections: `email` must be `type = "email"` and `unique = true`

If you declare the `email` field of an auth collection yourself, it must now be
typed `email` and marked unique. A `text`-typed one dodges the
case-insensitive uniqueness check (which is scoped to the Email field type),
and a non-unique one lets two accounts share an address that logins then
collapse into one. Both now stop the boot:

```
Auth collection field 'email' must have type 'email' (got 'text') — the
case-insensitive uniqueness and login lookup depend on it
Auth collection field 'email' must be unique = true — it is the login identity
```

```diff
  crap.collections.define("users", {
      auth = { enabled = true },
      fields = {
-         { name = "email", type = "text" },
+         { name = "email", type = "email", unique = true, required = true },
      },
  })
```

Collections that never declared an `email` field are unaffected — one is still
injected for them, typed and unique. Before restarting, check for accounts
that differ only by case or Unicode form: they will also trip the canonical-form
check in item 33.

### 41. OAuth callbacks: scope the route when you have more than one auth collection

The un-scoped callback route `/admin/auth/callback/{name}` can only bind a
session when the target auth collection is unambiguous. It now resolves to the
**single** auth collection when there is exactly one, and **fails closed**
otherwise — with two or more auth collections it logs the ambiguity and
redirects to the login page instead of guessing.

**Action:** if your project defines more than one auth collection, point the
provider's redirect URI at the collection-scoped route:

```diff
- https://example.com/admin/auth/callback/github
+ https://example.com/admin/auth/callback/users/github
```

The hook (`hooks.auth_callback.{name}`) is unchanged; either route dispatches
to it, and either way the user it returns must exist in the bound collection.
Projects with exactly one auth collection can keep the un-scoped URL.

### 42. Filter clients: one operator grammar on every surface

The comparison operators had per-surface spellings. All surfaces now share one
verbose grammar — `equals`, `not_equals`, `greater_than`,
`greater_than_or_equal`, `less_than`, `less_than_or_equal`, `like`,
`contains`, `in`, `not_in`, `exists`, `not_exists` — read from a single
mapping, so no surface can drift again. An unrecognized operator is rejected
with the list of valid ones.

**Action, admin list-view URLs and anything that builds them** (saved views,
bookmarked filter links, links generated by your own code):

```diff
- ?where[price][gt]=10&where[stock][lte]=5
+ ?where[price][greater_than]=10&where[stock][less_than_or_equal]=5
```

`gt → greater_than`, `gte → greater_than_or_equal`, `lt → less_than`,
`lte → less_than_or_equal`.

**Action, MCP clients:** the `greater_than_equal` / `less_than_equal` aliases
are gone — use `greater_than_or_equal` / `less_than_or_equal`.

gRPC, Lua and service-layer filters already used the verbose forms and are
unchanged.

### 43. Subscribers: live streams gate drafts and trash per view

Mutation events (gRPC `Subscribe` and the admin SSE stream) used to check
collection access once at connect and then apply only the `read` rule's row
constraints — there was no status-aware filtering, so a subscriber with `read`
received events for **draft** documents and for soft deletes regardless of
whether it could see that content.

Each event is now gated by the content view it belongs to: a published
document's event needs `read`, a draft's needs `access.draft`, a soft delete
needs `access.trash`, and a hard delete is gated by the view the document was
last in. The views are independent — a reviewer granted `draft` but denied
`read` receives draft events and no published ones. Events carry this view
metadata regardless of the collection's `live` mode, so `metadata` mode and
delete events (whose payload is empty) are gated too.

**Action:**

- A subscriber that relied on seeing draft or soft-delete events needs the
  matching `access.draft` / `access.trash` rule; without it those events stop
  arriving.
- If you hand-wrote a `{ _status = "published" }` constraint on `access.read`
  to filter drafts out of a stream, drop it — the view model does this now, and
  a system-column constraint is rejected outright (item 39).
- An event that arrives without view metadata is dropped rather than guessed
  at. During a rolling upgrade an alpha.9 node publishing into a shared Redis
  produces exactly that, so upgrade all nodes that share a Redis together
  (item 27).

### 44. Admin XHR clients: the back-references endpoint returns an object

`GET /admin/collections/{slug}/{id}/back-references` returned a bare JSON
array. It now returns an object, because referrers the viewer cannot read are
dropped from the list and reported only as a flag:

```diff
- [ { "collection": "posts", "field": "author", "count": 3, … } ]
+ {
+   "references": [ { "collection": "posts", "field": "author", "count": 3, … } ],
+   "has_inaccessible": true
+ }
```

Each group's `count` now covers only documents the viewer may read — through
whichever view applies (`read`, `access.draft`, `access.trash`), with row
constraints matched. `has_inaccessible` is `true` when at least one referrer
was dropped; it is deliberately not a count.

**Action:** read `response.references` instead of the array, and surface
`has_inaccessible` as an unquantified note. Only custom tooling that called
this endpoint is affected — the bundled admin UI is updated. The delete
*block* itself is unchanged: it still uses the raw `_ref_count`, which stays
visibility-blind so the database cannot be left with orphaned references. The
delete page and edit sidebar no longer print that raw count as a number
("Referenced by other content" instead of "Referenced by N documents"), since
it aggregates references the viewer cannot see.

### 45. Hook authors: `crap.pages.list()` returns tables, and oversized HTTP responses error

Two `crap.*` return-value changes:

- **`crap.pages.list()`** returns a list of `crap.PageInfo` tables
  (`{ slug, section?, label?, icon?, access = "public"|"gated" }`), not a list
  of slug strings — mirroring `crap.routes.list()`.

  ```diff
  - for _, slug in ipairs(crap.pages.list()) do
  + for _, page in ipairs(crap.pages.list()) do
  +     local slug = page.slug
  ```

  The order is iteration order and is not stable across runs; sort by `slug`
  if you need determinism.

- **`crap.http.request`** errors when the response body exceeds
  `[hooks] http_max_response_bytes` (default 10 MB). The limit existed but the
  oversized body was returned truncated, so a hook silently parsed half a
  document. Raise the limit if a hook legitimately downloads large files, or
  handle the error with `pcall`.

### 46. CI scripts: `status --check` and `jobs healthcheck` exit 2 on warnings

So a pipeline can distinguish a clean audit from one that found problems:

| command | 0 | 1 | 2 |
| --- | --- | --- | --- |
| `crap-cms status --check` | no warnings | — | warnings found |
| `crap-cms jobs healthcheck` | healthy | unhealthy (stale running job) | warning (recent failures, long-pending or never-run scheduled jobs) |
| `crap-cms update check` | up to date | an update is available | — |

**Action:** a script that treated any non-zero exit from `status --check` as a
hard failure now also trips on warnings; branch on `2` if you want warnings to
be non-fatal. Note that `jobs healthcheck` uses `1` for the *worse* outcome —
it mirrors `update check`, where `1` means "action needed".

### 47. Seeding scripts: passwords are policy-checked on every write surface

`[auth.password_policy]` used to be applied by some write paths and not
others, so a password set through Lua or a bulk create could land in the
database below the configured minimum. The check now lives
at the single create/update chokepoint the service layer runs for every
surface and every operation — admin form, REST, gRPC, MCP, Lua, single and
bulk — so no weak password can reach the database whichever caller wrote it.
A context that fails to pass the configured policy falls back to the *default*
policy (minimum 8 characters, maximum 128 bytes, no character-class
requirements), never to no enforcement.

An empty `password` still means "leave it alone" on update; on **create** it is
now rejected on every surface, which used to produce a passwordless auth
document.

gRPC `CreateMany` no longer drops a per-document `password`: a bulk create
used to discard it silently (the user came out unable to sign in); each item's
password is now validated against the policy and hashed, as on every other
surface. `UpdateMany` still rejects a `password`, because it would apply one
credential to every matched row.

**Action:** a seed script or fixture that created auth users with a short
throwaway password (`"test"`, `"1234"`) through Lua, a bulk create, or any API
now fails validation with a `password` field error. Use a policy-compliant
value, or relax `[auth.password_policy]` for that environment.
(`crap-cms import --include-credentials` carries password *hashes*, not
plaintext, so imports are unaffected.)

### 48a. Write clients: publishing means "the latest draft plus this request"

An update with `draft = false` while a draft is pending now takes the latest
draft snapshot as its base and applies the request's fields on top — on every
surface. A bare gRPC/Lua/MCP update used to publish only the fields it sent
and leave the rest of the draft pending; a draft saved with a new file never
published that file. Now the drafted values, rows and file become live, a
field the request sends wins, and a publisher who may not write a field
cannot publish a drafted change to it.

**Action:** a client that published with a partial update while a draft was
pending, and relied on the draft's other fields staying unpublished, must
discard the draft first (restore the published version) or send the intended
values explicitly.

Files: a stored upload file is deleted only when nothing references it — it
survives as long as any draft or version snapshot of its document names it,
so storage on versioned upload collections
grows with retained versions; `max_versions` pruning deletes the files it
releases — lowering `max_versions` on an existing upload collection deletes
old files on the next write to each document. Queued-format conversions of a
drafted file run at publish.

### 48. Write clients: `locale = "all"` is rejected on writes

`locale = "all"` is a read shape (every locale as a per-locale map). A write
that passed it — `create`, `update`, `create_many`, `update_many`,
`update_global`, `validate`, on any surface — used to write the default locale
silently and skip the shared-field lock. It is now a validation error on the
`locale` field.

**Action:** pass one locale code on writes (or none for the default locale).

### 49. Operators: graceful shutdown drains running jobs before exiting

A stop (SIGTERM, `crap-cms serve --stop`, `crap-cms work --stop`) used to exit
at once, killing running jobs mid-run: they kept a fresh heartbeat, so stale
recovery waited for them, and a queued bulk run (one attempt) became
terminally stale. The scheduler now waits for running jobs before exiting, and
`serve --stop` / `work --stop` wait for the same deadline before sending
SIGKILL: the longest
configured `[jobs.queues.*] timeout` plus five minutes — 3900 s with the
defaults, instead of a fixed 10 s.

**Action:** if your deployment expects a faster stop, lower the relevant
`[jobs.queues.<name>] timeout`. A Lua job's own `timeout` is not part of the
deadline, so raise the matching queue timeout for long Lua jobs. `/ready`
returns 503 until startup stale-job recovery has completed, and gRPC
`Subscribe` streams are closed at shutdown, so subscribers must reconnect.

## Admin UI behavior

### Template overrides: the duplicate locale-picker keys are gone

The admin page context used to carry two parallel descriptions of the same
editor locale. The `has_locales` / `current_locale` / `locales` set has been
removed; `has_editor_locales` / `editor_locale` / `editor_locales` — which
every shipped template already used — is now the only one.

**Action:** if an overridden `collections/edit.hbs`, `globals/edit.hbs` or
`layout/header.hbs` reads a removed key, rename it. The values are identical.

| Removed | Read instead | Shape |
|---|---|---|
| `has_locales` | `has_editor_locales` | boolean; absent entirely when `[locale] locales` is empty |
| `current_locale` | `editor_locale` | the active locale code (`"de"`) — what the hidden `_locale` input submits |
| `locales` | `editor_locales` | array of `{ value, label, selected }`: the code, its upper-case label, `selected` for the active one |

### Navigation now partial-swaps `#main`

Admin nav links target `#main` (htmx partial swap): the server returns only
`<title>` + main content for htmx navigations; the shell (head, scripts,
sidebar, component singletons) stays in the DOM. Direct loads and htmx
history-restores still get the full document.

**If your template overlay overrides `layout/base.hbs`**, port the new
`{{#if htmx_partial}}` branch from the default layout — without it, htmx
navigations nest a full document inside `#main`. Overridden page templates
whose links still use `hx-target="body"` keep the old full-page swap and
continue to work.



- **List pages return 400 on invalid query params.** A
  present-but-invalid `where[...]` filter (unknown operator or field,
  system column, malformed key), an unknown/unsortable `sort` field, or
  an invalid `_status` value used to be silently ignored — the list
  rendered unfiltered or default-sorted results. They now render a 400
  Bad Request page naming the offending parameter (parity with MCP and
  gRPC, which already hard-error). URLs produced by the admin filter UI
  are unaffected; only hand-edited or stale bookmarked URLs with
  since-renamed fields can be affected.

- **Custom routes reject `csrf = true` on safe-method-only routes.** CSRF is
  enforced only on mutating methods, so `csrf = true` on a GET/HEAD/OPTIONS-only
  route was inert. Such a registration now **fails to load**. **Action:** if you
  set `csrf = true` on a safe-method route, either drop it (the handler must not
  mutate state) or add a mutating method (POST/PUT/PATCH/DELETE), which the CSRF
  check then covers.

- **Draft-only documents are now reachable from Delete and Versions.** A document
  saved only as a draft no longer 404s on its delete-confirm or version-history
  page. **Action:** none.

- **The edit-page version sidebar now honors per-user version access.** It was
  evaluated as anonymous, so on collections/globals whose `versions`/`read`
  access depends on the user it rendered empty and logged an error; it now passes
  the current user. **Action:** none.

- **Sorting a list by a has-many field returns 400, not 500.** A has-many
  relationship/upload isn't sortable (no column); it's now rejected at the param
  gate. **Action:** none.

- **Some admin JSON endpoints now return real status codes.** Version-restore
  denials return 403 (was a silent redirect); back-references and
  evaluate-conditions return 404/403/500 instead of `200` with an error body. A
  client that checks `response.ok` and skips on failure keeps working. **Action:**
  none.

## Security fixes

- **The upload API returns `401` for an unusable token.** A token of a locked
  account, a revoked session or a deleted user used to upload as an anonymous
  caller when the collection allowed that. If a client relied on it, log the
  user in again.
- **gRPC `TriggerJob` returns `NOT_FOUND` when the caller may not trigger the
  job** (was `PERMISSION_DENIED`), even when the payload is malformed. Treat
  both as "not available to this caller". The job's access rule now runs
  before a malformed payload is rejected, with `ctx.data` set to `nil` — if
  the rule reads `ctx.data`, guard against `nil`.
- **A locked account's token answers "Account locked".** Every request now
  reads the lock from the stored account, so a token issued before a lock is
  refused as locked — gRPC `PERMISSION_DENIED`, upload API `401` — where it used
  to answer "Session invalidated" (`UNAUTHENTICATED`). A client that recovered
  only from `UNAUTHENTICATED` should treat both as "log in again".
- **Custom auth strategies:** a locked account no longer signs in through a
  strategy, and a verified account of a collection with `verify_email` now
  does. Strategy hooks don't have to copy `_locked` / `_verified` onto the
  document they return.
- **Custom page access rules must return `true`.** A rule that returns a filter
  table now denies the page and hides it from the sidebar; return
  `true`/`false` instead.
- **MCP job tools no longer show bulk runs of collections hidden from MCP**,
  including bulk runs that finished before the upgrade.
- **gRPC `Login` no longer bypasses MFA — and gRPC now completes it.** The
  RPC previously minted a full JWT on the password alone, silently bypassing
  the second factor the admin login enforces. On a collection with
  `mfa = "email"` it now returns `mfa_required = true` plus a short-lived
  `mfa_challenge` token (and NO session token); the emailed 6-digit code is
  redeemed via the new `VerifyMfa` RPC, which mints the JWT. Code guessing
  shares the admin MFA rate limiters (same per-identity/IP budget).
- **Unpublishing a non-versioned global no longer publishes the form data.**
  The admin handler's versioning guard silently fell through to a normal
  update when versioning was off — saving (and publishing) the submitted
  form under an action labeled "unpublish". The capability gate now lives in
  the service layer and every surface gets a typed error instead.
- **Lua CRUD reads inside hook transactions no longer share the process-wide
  populate singleflight.** An in-transaction populate fetch could broadcast
  uncommitted rows to concurrent requests (and receive another connection's
  stale fetch). No action needed; hook-transaction reads now populate
  un-deduplicated on their own connection.

- **Admin MFA could be bypassed with only the password.** The MFA-pending cookie
  was a valid session token; an attacker who knew the password could use it as a
  session and skip the email code. Tokens now carry a `token_use` claim and only
  `session` tokens authenticate. **Action:** none — existing sessions keep
  working (legacy tokens decode as `session`).

- **Rate-limit hardening (login/MFA).** Per-account login/reset limiters no longer
  reset per email-casing variant; MFA-code email issuance is throttled per user;
  a successful login refunds only its own attempt on the shared per-IP limiter
  (instead of wiping other accounts' failures); and email-verify / reset each get
  their own per-IP keyspace. **Action:** none.

- **A crafted upload filename could crash the file-serve request.** A control
  byte in a stored filename reached `Content-Disposition` and panicked the
  request task; control chars are now stripped and the header is built without
  panicking. **Action:** none.

- **The dashboard leaked `access.admin`-hidden collections.** Dashboard cards now
  apply the same `access.admin` gate as the sidebar nav (evaluated under
  operation `"admin"`). **Action:** none — if you relied on a collection showing
  on the dashboard while `access.admin` denied it, that was a leak.

- **Custom routes now receive the static security headers.** Merged custom routes
  were served with no `X-Frame-Options` / nosniff / referrer / permissions / HSTS
  headers; those now apply to the full router. **Action:** none.

- **A revoked session could keep receiving live events after an invalidation
  burst.** The gRPC `Subscribe` stream's revocation handler swallowed a lagged or
  closed invalidation broadcast and kept streaming, while the event handler
  treats a lag as fatal. If enough revocations were published while a subscriber
  was busy (past the invalidation bus's capacity), it could lag past its own
  revocation and keep receiving events on a revoked token. It now fails closed: a
  lagged/closed invalidation drops the subscriber and forces a reconnect (which
  re-authenticates). **Action:** none — clients already reconnect. (This is
  distinct from the "Un-verifying a user tears down their live-update streams"
  fix below, which is about *publishing* the invalidation; this one is about
  *receiving* it reliably.)
- **A negative gRPC `limit` no longer triggers an unbounded read.** `ListJobRuns`
  and `ListVersions` accept an `optional int64 limit`. A client sending
  `limit = -1` bound as SQLite `LIMIT -1` (= no limit), returning the entire
  job-run / version history and bypassing the 1000-row cap. The limit is now
  floored at 0 on both. **Action:** none.

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
  authenticated caller could read any job's run payloads (`data`,
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
- **Restoring an old version leaves localized rows alone.** Version snapshots
  now keep each locale's array, blocks and has-many relationship rows apart, so
  a restore puts every locale's rows back where they belong. Snapshots taken
  before this release don't carry that split: restoring one restores the
  document's fields but leaves localized array, blocks and relationship rows as
  they currently are, rather than mixing every locale's rows into the default
  locale as before. No action needed.
- **Old SQLite timestamps are rewritten once.** Databases created by early
  versions stored some timestamps as `YYYY-MM-DD HH:MM:SS`, which sorted and
  filtered incorrectly against current ISO 8601 values. The first startup
  rewrites them in place (every collection, global, version and job table) —
  expect that startup to take longer on large databases; no action needed.
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
- **Stored tokens are hashed.** Reset and verification tokens are written as
  a SHA-256 digest; an MFA code is written as an HMAC keyed with
  `[auth] secret`, since six digits is small enough to invert a bare digest by
  table lookup. Every lookup hashes what the caller presented and compares in
  constant time. A completed `_system_email` job has its payload emptied, so
  the rendered link stops living in `_crap_jobs` after the send. **Action:**
  see item 19 — links already sent stop working.
- **The gRPC create/update codec no longer applies the password policy.** The
  wire decoder ran the policy check while unpacking a request, *before* the
  access rule was consulted, so an unauthenticated caller could probe the
  configured minimum length and character classes by watching which passwords
  came back rejected. The check now runs only in the write path, after the
  access check — where it always also ran, so nothing is now unvalidated. The
  codec keeps one shape check, which reveals nothing about the policy: a
  `password` that is not a string is `INVALID_ARGUMENT` rather than coerced
  to `""`. A present-but-empty password on a *create* is also rejected now,
  on every surface, instead of quietly producing a passwordless account.
  **Action:** none, unless a client sent an empty or non-string password and
  relied on it being ignored.
- **MCP no longer confirms which collections exist.** Covered in item 25.
- **A password change invalidates an outstanding reset link.** The reset flow
  cleared the token, but a logged-in password change (or the CLI) left it live
  for the rest of its window. One password-update statement now always clears
  it. **Action:** none.
- **An expired reset link is refused before the form is shown.** The page
  validated the token without checking expiry, so a dead link rendered the
  form and only failed on submit — and then always as "invalid", because the
  expired branch matched a type the service never returns. **Action:** none.
- **`LoginResponse.user` is stripped like every other document.** The login
  and MFA lookups read the user row raw, so a `hidden` field or one denied by
  its `access.read` rule rode along on the login response while `Me` removed
  it. **Action:** a client that read such a field off the login response will
  no longer find it — read it through `Me` with an authorized user instead.
- **Changing a user's email requires re-verification.** On a collection with
  `verify_email`, an email change kept the `_verified` flag, so a user could
  move their account to an address they had never proven they control.
  **Action:** none, but expect a verification mail on email edits.
- **Read-denied fields are no longer a query oracle.** Covered in item 20.
- **Hook errors no longer carry the Lua stack traceback, or absolute server
  paths, to clients.** An `error()` in a hook returned the whole traceback —
  including `{config_dir}/hooks/….lua` — to the API caller, disclosing the
  deployment's filesystem layout. Chunk names are now config-relative and the
  traceback is stripped. **Action:** a client that parsed the traceback out of
  an error message needs to stop; the message itself is unchanged.
- **`read_config_file` redaction is structural.** The MCP tool redacted
  secrets by matching lines, so a dotted key, an inline table, or a
  multi-line string slipped a secret through. It now redacts by parsing the
  document. **Action:** none.
- **`restore --include-uploads` extracts only the `uploads` member.** A
  crafted archive could write anywhere under the config directory, including
  over your Lua. **Action:** none.
- **`db console` no longer puts the Postgres password in `argv`.** It was
  visible to any local process listing. **Action:** none.
- **A revoked admin session could keep receiving live updates after logout.**
  Session invalidation blocked new requests but did not tear down an open
  stream. **Action:** none.
- **`McpApiKey` no longer prints the key through `Display`.** **Action:**
  none.
- **`io.popen` is no longer reachable from Lua hooks.** `os.execute` was
  already removed; `io.popen` was the surviving process-spawn path.
  **Action:** a hook that shelled out through it now errors. There is no
  replacement — process execution is outside the sandbox contract.
- **A read of all locales applies field-read access per locale.** **Action:**
  none.
- **MCP job tools no longer leak raw backend text on internal errors.**
  **Action:** none.
- **gRPC account RPCs authenticate before checking the collection.**
  `LockAccount` / `UnlockAccount` / `VerifyAccount` / `UnverifyAccount` ran the
  "is this an auth collection / does it have `verify_email`" checks *before*
  the authentication check, so an unauthenticated caller could probe which
  collections exist and how they are configured. They now answer
  `UNAUTHENTICATED` first; shape errors reach authenticated callers only.
  **Action:** a client that distinguished those shape errors while
  unauthenticated now sees `UNAUTHENTICATED` instead.
- **`admin.access` / `access.admin` is a boolean gate.** A rule that returned a
  filter table used to pass it — there is no row scope at the admin gate, so
  `Allowed` and `Constrained` were treated alike. A filter table is now logged
  as an error and **denies**, matching the fail-closed `access.mcp` twin; a
  hook error or an exhausted connection pool denies too. **Action:** an
  `admin` / `mcp` access rule must `return true` or `return false`. Returning a
  table now locks the admin UI out.
- **Relationship population enforces the target collection's read access.**
  Populating a relationship or upload field at `depth > 0` embedded the target
  document after checking only the draft filter — never the target
  collection's `read` rule or its row constraints — so a user denied read on a
  collection could still see its documents embedded inside one they could
  read. (Join fields already enforced this.) Population now resolves the
  target collection's access and hides denied targets: a has-one resolves to
  `null`, a has-many drops the entry. The populate cache was reworked to hold
  raw, user-independent documents and apply access per request, so one user's
  cached document is never served to another. **Action:** a client that assumed
  a populated relationship is always an object must handle `null`, and a
  has-many list that comes back shorter than its stored id list. Reading with
  `depth = 0` (ids only) is unaffected.
- **MCP reads bypass access, consistently with MCP writes.** MCP is a
  single-token, full-access surface with no per-user identity. Its writes
  already bypassed collection and field access, but its reads did not — so a
  client could `update` a row that its own `find` had just refused to return,
  and read-access hooks ran against a `nil` user. MCP reads now set the same
  override, matching writes and the documented "MCP operates with full access"
  contract. **Action:** treat the MCP API key as a full-access credential.
  Scope what MCP can reach with `[mcp] include_collections` /
  `exclude_collections` and the per-collection `access.mcp` gate — not with
  `access.read`, which no longer narrows it.

## gRPC clients (regenerate from `proto/content.proto`)

Wire-contract changes — regenerate your gRPC stubs and adjust:

- **New RPC: `ResendVerification`.** Additive, so existing stubs keep
  working; regenerate to call it (see Additive features).

- **`ListVersions` / `RestoreVersion` on a non-versioned collection now
  return `INVALID_ARGUMENT`** (was `FAILED_PRECONDITION`). The versioning
  gate moved into the service layer — uniform with `Unpublish`/`Undelete`,
  and MCP/Lua now get a typed error instead of a raw database error.

- **Document `data`/`fields` are now typed `DataMap`/`FieldValue`, not
  `google.protobuf.Struct`.** Every `google.protobuf.Struct` used for
  document content — `Document.fields`, the `data` on `Create` / `Update` /
  `UpdateGlobal` / `UpdateMany` / `Validate` / `ValidateGlobal`, the
  `CreateMany.documents`, and `MutationEvent.data` — is now a `DataMap`
  (`map<string, FieldValue>`, still keyed by Lua field name so adding a
  field never changes the proto). A `FieldValue` is a `oneof` over
  `null_value` / `int_value` (`int64`) / `double_value` / `string_value` /
  `bool_value` / `struct_value` (nested `DataMap`) / `list_value`
  (`FieldList`). Read values through the oneof accessors instead of
  `Struct`'s `Value.number_value` — and read integers from `int_value`, not
  `double_value`. This also fixes the old precision loss: integers above
  2^53 (~9.0e15) were silently rounded when they went through `Struct`'s
  only numeric kind (a `double`); they now round-trip exactly via
  `int_value`. Regenerate stubs and update any code that constructed or
  read `Struct` for document data. If you use the built-in
  `crap-cms typegen client` generator, regenerate it too — the Rust (`-l rs`)
  output now decodes the typed `FieldValue` (the other languages emit type
  definitions only and are unaffected).
- **`CreateMany` now accepts a policy-checked `password` for auth collections.**
  It previously dropped it silently, then (earlier in this cycle) rejected it with
  `INVALID_ARGUMENT`; a per-item `password` is now validated against
  `[auth.password_policy]` and hashed per document (parity with single `Create`).
  `UpdateMany` still rejects a `password` (it applies one value to many rows).
  A **non-string** `password` (e.g. a number) is rejected the same way — it used
  to be silently dropped, creating a passwordless auth account.
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
- **`DescribeCollection` for a global reports `timestamps: true`** and the
  global's real `drafts` setting (both were always `false`). If your client
  skipped `created_at`/`updated_at` for globals because of that flag, read
  them.
- **Additive:** `FieldInfo.relationship_collections`, `FieldInfo.has_many` and
  `FieldInfo.timezone` describe polymorphic targets, value lists and timezone
  dates. Non-breaking.
- **Doc-only:** `Create`/`Update` now document that a UNIQUE-constraint
  conflict maps to `ALREADY_EXISTS` (the runtime mapping was already
  `ALREADY_EXISTS`; only the proto comment was stale).

## Generated client types (`typegen client` / `typegen proto` — shapes changed)

The `crap-cms typegen client -l <lang>` output
(`types/client.{ts,go,py,rs}`) and the Rust `crap-cms typegen proto` decoder
gained proper types for several things they previously flattened. If you check
generated types into your project, **regenerate and adjust the consuming
code** — this is a breaking change to the generated *shapes*, not the wire.

Regenerate:

```bash
crap-cms typegen client -l ts,go,py,rs
crap-cms typegen proto            # only if you use the Rust gRPC decoder
```

What changed:

- **Relationships are populate-aware, no longer bare id strings.** A
  relationship or upload field can arrive as an id (`depth = 0`) or a populated
  document (`depth >= 1`); the generated type now models both.

  | Language | Before | After |
  |---|---|---|
  | Rust | `String` | `Rel<T>` — `enum { Doc(Box<T>), Id(String) }` |
  | Go | `string` | `Rel[T]` — a struct whose custom JSON decodes an id or an object |
  | TypeScript | `string` | `string \| TDocument` |
  | Python | `str` | `str \| T` |

  Unwrap before use: Rust `match rel { Rel::Id(id) => …, Rel::Doc(doc) => … }`
  (generated `rel.as_id()` / `rel.as_doc()` helpers return `Option`); Go
  `rel.ID` / `rel.Doc`; TS/Python narrow with `typeof x === "string"` /
  `isinstance(x, str)`.

- **A single (non-`has_many`) relationship is now optional on read**, even when
  the field is `required` on write — it can be absent after the target is
  soft-deleted or you lack read access. Handle the `null` / `None` / `nil`
  case.

- **`select` fields become a named type, and Rust/Go keep unknown values.**

  | Language | After |
  |---|---|
  | Rust | `enum { …, Other(String) }` (`serde(from/into)`; an unknown value → `Other`) |
  | Go | `type XStatus string` + `const`s (an unknown value still assigns) |
  | TypeScript | string union — `"a" \| "b"` |
  | Python | `Literal["a", "b"]` |

  Rust and Go round-trip a value that was removed from the schema after you
  generated; TypeScript and Python narrow to the known set.

- **Polymorphic relationships (a relationship targeting multiple collections)
  are typed** instead of `String`: Rust an untagged `enum` discriminated by a
  `#[serde(tag = "collection")]` ref enum, TS/Python a union of the target
  documents, Go `interface{}`.

- **Every field of a read document is optional.** A draft read can return a
  `required` field empty, and field read access and `select` leave keys out,
  so the generated read types no longer mark any field as always present —
  groups and array rows included. A TypeScript read field is `?: T | null`,
  since an empty value reads as `null`. In TypeScript `…Document` no longer extends
  `…Data`; `…Data` keeps its required fields as the input for creating (an
  update accepts `Partial<…Data>`), and each group and array row type has a
  `…Data` input variant that keeps its required fields too (`PostsSeoData`
  beside the read type `PostsSeo`); the row type of an array stored in its own table
  (not nested inside another row) gains an optional `id` — send it back on update
  to keep the stored row. Rust fields become `Option<T>`, Go fields
  pointers or nil-able values — booleans and single groups included (`*bool`,
  `*PostsSeo`) — and Python fields `Optional[...] = None`. Handle the absent
  case where your code relied on a required field; in Go, dereference booleans
  and single groups.

- **Read documents carry `_status`, `_deleted_at` and `<name>_tz`** when the
  collection has drafts, soft delete or timezone dates.

- **New `locale = "all"` read types.** A collection or global with localized
  fields also gets `…LocalizedDocument` (TypeScript) / `…Localized` (Rust, Go,
  Python), where each localized field is a per-locale map whose value is `null`
  for a locale without one (`Localized<T>` — `{ [locale: string]: T | null }` —
  in TypeScript, `HashMap<String, Option<T>>` in Rust, `map[string]*T` in Go,
  `dict[str, Optional[T]]` in Python). Use it to decode `locale = "all"`
  responses, and handle the locales a document has no value for.

- **New `CollectionSlug` type** enumerating the known slugs (Rust/Go a named
  type with constants, TS/Python a string-literal union).

- **`typegen client` now errors on a type-name collision** (two constructs that
  would generate the same type name — e.g. a collection slugged `posts_status`
  and the `status` select of `posts`) instead of silently emitting one wrong
  type. If generation fails with a collision error, rename one construct.

- **Rust `typegen proto` and `typegen client -l rs` compile together again.**
  The proto decoder had drifted — `select`/polymorphic fields stayed `String`,
  single relationships were non-optional, and a relationship nested inside a
  group/array/blocks decoded as id-only. It now matches the client types
  field-for-field, including decoding a **populated** nested relationship
  (`Rel::Doc`) at any depth. Regenerate both artifacts together.

## Bug fixes (no action needed)

Two carry a caveat about data written before the upgrade — read those first
if you use versions on a localized collection.

- **Versions no longer lose every other locale's content.** A snapshot was
  built from the row as resolved under the *writing* locale, so it held one
  value per localized field. Restoring it wrote that value into the default
  locale's column and `NULL`ed the rest: a German edit restored over the
  English title and the other translations vanished. Snapshots now record
  every locale's column. **Caveat:** snapshots taken *before* the upgrade
  only ever held one locale, so restoring an old snapshot is still lossy —
  there is nothing in the row to recover the other translations from. Treat
  pre-upgrade snapshots on localized collections as single-locale.
- **Version pruning no longer deletes the last published snapshot.**
  `max_versions` could prune away the only published snapshot, leaving a
  collection with no publishable history. The newest published snapshot is
  now exempt from pruning. **Caveat:** snapshots already pruned are gone.
- **Login works on a localized auth collection.** The user lookups selected
  bare column names, which do not exist when a field is localized, so login
  returned INTERNAL and every authenticated request came back UNAVAILABLE. If
  you marked a field on your auth collection `localized` and then found auth
  entirely broken, this was why.
- **Undelete works on a localized soft-delete collection**, and **searching
  the trash view returns results** — both previously failed or returned
  nothing for the same bare-column reason.
- **MCP schemas match the documents the server stores.** Block rows are named
  by `_block_type` (the schema said `blockType`, which writes rejected), array
  and blocks rows carry their `id`, has-many text and number fields are lists,
  timezone dates have a `<name>_tz` property, and polymorphic references are
  `collection/id`. Agents that cached a schema should fetch it again.
- **Generated Lua hook types name the operations hooks receive** (`"delete"`
  for collections, `"get"` for global reads), and `crap.doc.*` marks every
  field optional with `_status`, `_deleted_at` and `<name>_tz` where they
  exist. `types/hooks.lua` regenerates on the next dev-mode start or
  `crap-cms typegen lua`.
- **Version history shows dates.** `created_at` was stored but no query
  selected it, so every version rendered an empty date. Existing rows have
  the data; they just start displaying it.
- **Restore on a `versions = { drafts = false }` collection works.** It wrote
  `_status`, a column only created for drafts-enabled collections, and died
  with a raw backend error after the hooks had already run.
- **A trashed document's pending draft is no longer served as live.**
- **Restoring a version refreshes the search index** (it used to go stale)
  and no longer leaves a localized timezone Date's timezone wrong.
- **Keyset (cursor) pagination no longer drops rows with a NULL sort value.**
  If you paginate on an optional field, pages that silently skipped rows now
  include them.
- **`default_value = true` on a Checkbox field is stored as true.** It was
  silently stored as false. If you worked around this by setting the value in
  a `before_change` hook, you can drop the workaround.
- **A collection added after the initial ref-count backfill is backfilled.**
  Delete protection on collections defined later was counting from zero.
- **`crap.hooks.register` / `remove` no longer half-apply at runtime.**
- **Job auto-purge measures retention from completion, not creation.**
  Long-running jobs were purged too early; expect runs to stick around
  slightly longer than before.
- **`VerifyMfa` reports real error codes** instead of INTERNAL for every
  failure.
- **A reference to a mistyped or stale id reports as a client error**, not an
  internal one.
- **Email templates render "expires in 60 minutes"**, not "expires
  in60minutes".

- **`updated_at` sorts correctly on SQLite after a publish/unpublish.** A status
  change stamped `updated_at` via SQLite's `datetime('now')` (space separator,
  no milliseconds or `Z`) while ordinary edits use the ISO-8601 form. Since
  `updated_at` is a lexically-compared sort and cursor key, status-changed rows
  collated ahead of edited ones, breaking "sort by last updated" and keyset
  pages. SQLite's current-time expressions now emit the same ISO-8601 `…Z` shape
  as everything else. Existing rows are normalized to ISO on read, so no data
  migration is needed. (Postgres was never affected.)
- **Lua `crap.collections.find(...)` reports null fields as `nil`.** A field that
  was null or unset came back from a list read as the `NULL` sentinel (truthy),
  while `find_by_id` returned `nil` — so `doc.field == nil` disagreed between the
  two for the same document. Both now return `nil`. If a hook worked around this
  by comparing against the sentinel instead of `nil`, switch it to a plain
  `== nil` check.
- **Field names that are SQL reserved words (`order`, `select`, `group`, …)
  now work.** They passed the identifier validator but were interpolated into
  generated SQL unquoted, so writing to such a field failed with a syntax error
  (and would have broken on Postgres regardless). All generated identifiers are
  now quoted. If you renamed a field to dodge this, you can rename it back —
  though renaming a field migrates its column, so weigh the churn.
- **Private uploads on S3 / custom storage backends are reachable again.** The
  per-document access gate compared the request path against the backend's
  direct `public_url` rather than the served `/uploads/{key}` proxy path, so on
  non-local storage every access-gated file returned 404 to authorized users.
  Local storage was unaffected (the two URLs coincide there).
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
- **Upload create/update/delete return the right HTTP status for conflicts
  and transient failures.** A unique-constraint violation now returns `409`
  (was `500`) and a transient DB error `503` (was `500`, a retryable failure
  reported as permanent), matching the JSON REST and gRPC surfaces. A client
  that specifically special-cased `500` on these endpoints should treat
  `409`/`503` as the conflict/retry signals instead.
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

- **Login and Me run the auth collection's read hooks.** `before_read` and
  `after_read` now run on the user document Login and Me return, as on a
  find: a masking `after_read` changes the returned user, and a `before_read`
  that errors makes Login and Me fail instead of returning the user.
  **Action:** check that read hooks on an auth collection work for a request
  that is signing in — the user is the document being read.
- **Upload write responses and events carry `sizes`.** The document an upload
  create or update returns (REST `POST/PUT /api/upload/...`, gRPC, MCP, Lua)
  and the event it publishes now have the shape a read returns: the per-size
  values sit in `sizes.<name>.url/width/height`, and the flat `<name>_url`,
  `<name>_width`, `<name>_height` keys are gone from them. **Action:** a client
  that read `thumbnail_url` from the upload response reads
  `sizes.thumbnail.url` instead, as it already must from a read.
- **A finished image conversion is reported as an update.** When a queued
  conversion writes its format URLs, the document's `updated_at` changes and
  subscribers receive an `update` event carrying `sizes` like any other write;
  no hooks run and no version is created. Subscribers that treat every update
  as an editor's change should expect these.
- **Trashing an upload keeps its queued image conversions.** A conversion
  queued before a soft delete still runs, so a restored upload has its format
  variants. Only a permanent delete (or the trash purge) cancels them.
- **Reads return a code field's language choice.** A code field with
  `admin.languages` now stores the editor's pick and reads return it as
  `<name>_lang` (a per-locale map when read with `locale = "all"`, a key of the
  group object inside a group). Array rows get the column at the first startup.
  If a client rejects unknown keys, allow it, or regenerate typed clients — the
  generated types include it, and gRPC `FieldInfo.companions` lists a field's
  companion keys (`_tz`, `_lang`).
- **Admin relationship pickers and labels read in the editor's locale.** Search
  results and labels of a localized target collection show the locale being
  edited, not the default locale.
- **Numbers ignore surrounding whitespace; any non-zero number checks a
  checkbox.** `" 5"` is accepted and stored as `5` in single and multi-value
  number fields (a single field used to reject it). A checkbox written as a
  number is checked for any value other than `0` — `2` and `1.0` used to store
  unchecked in a top-level checkbox.
- **`crap-cms user delete` goes through the service layer.** On a soft-delete
  auth collection the user is moved to the trash (empty it with
  `crap-cms trash purge`, which refuses a user other documents still
  reference); on other collections such a user is refused right away; delete
  hooks run. Scripts that relied on an immediate hard delete should purge the
  trash afterwards.
- **`crap-cms restore` and `migrate fresh` refuse while a server, worker, stdio
  MCP process or another CLI command uses the database**, not only the server,
  and those refuse to start while either runs. They share the lock file
  `data/crap.lock`; don't delete it while they run. **Action:** keep the data
  directory on a filesystem that supports file locks — a local disk does; NFS
  needs its lock service. Without them, these processes stop at startup with
  "Failed to take the instance lock". A read-only data directory works once
  `data/crap.lock` exists, except for `restore` and `migrate fresh`, which need
  it writable.
- **Two indexes with the same name stop startup.** Index names are built as
  `idx_{slug}_{fields}`, so an index of one collection could get the name of
  another's — within one collection, or across two whose slugs and field names
  join the same way — and one silently replaced the other. Startup now names
  both. **Action:** if it stops, rename a field or collection so the names
  differ.
- **A backslash escapes `%` and `_` in `like` filters.** `like` now uses
  `ESCAPE '\'` on every backend: `\%`, `\_` and `\\` match a literal `%`, `_`
  and backslash, and a pattern ending in a lone backslash is rejected. If a
  `like` value (in a filter, access constraint or saved admin URL) contains a
  literal backslash, double it.
- **A timezone date's `<name>_tz` follows its date in reads.** Read with
  `locale = "all"`, a localized timezone date's `<name>_tz` is now a
  per-locale map like the date (it used to come back as flat
  `<name>_tz__en` keys); inside a group, `<name>_tz` is a key of the group
  object (it used to sit beside the group as `<group>__<name>_tz`). If a
  client read those flat keys, read the new places instead.
- **Text inputs no longer carry the browser `maxlength`/`minlength`
  attributes.** Length is checked by server validation, which counts
  characters. If a custom template or script relied on those attributes, read
  `data-max-length`/`data-min-length` or the field definition instead.
- **Bulk writes are gated by the write-side access rule, uniformly.**
  `update_many` is gated by `access.update`, `delete_many` by
  `access.trash`/`access.delete` (trash falls back to `update`), evaluated
  once up front at the service layer — `Denied` errors before anything is
  matched, a filter constraint scopes the match-set. Previously gRPC
  pre-scoped bulk writes with `access.read`'s filters instead: if your
  collection has a *constrained* `read` rule but **no** `update`/`delete`
  rule, gRPC bulk writes are no longer narrowed by the read filter — add the
  write-side rule if you relied on that. (Per-document checks still run
  inside the transaction; MCP's trusted override is unchanged.)
- **gRPC `DeleteMany` now matches draft documents too.** The codec used to
  inject a published-only filter (a gRPC-only quirk); bulk delete on every
  surface now behaves like Lua/MCP always did. `UpdateMany` keeps the
  published-only default with the `draft = true` opt-out, uniformly.
- **`ctx.ui_locale` now reaches read access checks and `after_read` for
  collections on every surface.** It was always `nil` there (only writes and
  global reads carried it). For API surfaces the value is the authenticated
  user's stored UI-locale preference. An access hook or `after_read` that
  branches on `ctx.ui_locale` may start seeing values where it saw `nil`.
- **An `access.update` rule may constrain on `_status` for bulk updates.**
  On a drafts-enabled collection, `update_many` itself targets published
  rows (unless `draft = true`), so a `{ _status = ... }` constraint from the
  update rule is accepted there. Everywhere else the system-column rejection
  is unchanged.
- **Verification emails from nested Lua creates now send.** A hook running
  inside an update/delete/undelete/unpublish/bulk/restore transaction that
  creates a `verify_email` auth document had its verification mail silently
  dropped (only creates carried the queue). Similarly, mutation events from
  nested CRUD inside version-restore hooks now publish after commit.
- **The admin trash view respects an explicit column sort.** It used to
  always force newest-deleted-first; that is now only the default when no
  sort is chosen.

- **Job-handler writes now emit live-update events and invalidate the
  populate cache.** Previously a job handler's `crap.collections.create` /
  `update` / `delete` silently published nothing (despite the `events` option
  defaulting to `true`) and never invalidated the populate cache. Live-update
  subscribers (gRPC `Subscribe`, admin SSE) will now start receiving
  job-driven events; pass `{ events = false }` in the handler for quiet
  writes (e.g. bulk seeding). Relatedly, standalone processes — `crap-cms
  work` workers and the stdio MCP server — now build the `[live]` transports
  from config, so with `transport = "redis"` their writes reach the app
  servers' subscribers and can tear down live sessions on auth-document
  deletes/locks (previously they could not).

- **Admin JSON/XHR endpoints use consistent error status codes.** The
  back-references, delete-dialog, and empty-trash endpoints previously
  disagreed on the status code for the same error; they now share one set of
  helpers. The one visible change: deleting a document still referenced by
  others returns **409 Conflict** (was 400) on the dialog path, with the same
  `{"error": …}` body. The admin UI reads `response.ok` and the `error` field,
  so it is unaffected; only a custom client that branched on the exact `400`
  would notice.

- **Scheduler reliability + defaults.** A crashed worker's in-flight jobs are
  now requeued and re-run by surviving nodes (at-least-once) — **make job
  handlers idempotent** if they aren't already. Job-history retention
  (`[jobs] auto_purge`) now defaults to **30 days** (was 7; unset it to disable).
  A latent bug where frequent cron jobs fired on only every other window is
  fixed, so a minutely cron now runs every minute instead of every two. These
  are frozen contracts going forward — see the [Frozen Contracts](../internals/frozen-contracts.md)
  reference.

- **Whole-valued `number` fields now serialize as integers.** Because
  `number` is stored as floating-point, an integer round-tripped through
  the database as `42.0` and was emitted that way on every read surface
  (REST/Lua/MCP/admin). Whole values now serialize as `42`; genuine
  fractions are unchanged (`42.5` stays `42.5`). JSON treats `42` and
  `42.0` as the same number, so virtually all clients are unaffected —
  the only thing that changes is a consumer that *string-matched*
  `"42.0"`, which will now see `"42"`. (Over gRPC, the same whole value
  now arrives as `FieldValue.int_value` rather than the old always-`double`
  representation — see the gRPC wire-contract change below.)

- **JSON-decoded floats now keep full precision.** crap-cms now uses a
  correctly-rounded JSON float parser, so a `number` value read back
  from a JSON-backed path — the `blocks` `data` column, MCP arguments,
  keyset pagination cursors — is bit-identical to what was written.
  Previously the parser could be off by up to one ULP for some
  magnitudes (very large/small exponents). This only makes values *more*
  exact, so no action is needed. (Over gRPC, integers no longer round-trip
  through a `double` at all: `FieldValue` carries an exact `int_value`
  (`int64`) for whole numbers and `double_value` only for fractional ones,
  so the old silent rounding of integers above 2^53 (~9.0e15) is gone —
  see the gRPC wire-contract change below.)

- **Scalar `has_many` lists read back as typed JSON arrays on every surface.**
  A `has_many` list on a `Text` / `Number` / `Select` / `Radio` field is stored
  as a JSON array in its column, but the read path used to be field-type-blind,
  so gRPC / Lua / MCP returned the raw **string** (`"[\"a\",\"b\"]"`) while the
  admin UI parsed it — even though the generated client types already declared an
  array (`StrList` / `NumList`). Reads now return the array on every surface
  (`["a","b"]`, and `Number` lists as numbers `[1,2]`), and a list stored via the
  admin form and one stored via the typed API now persist identically. **Action:**
  a client that consumed the raw string from a non-admin surface must now handle
  an array; no change if you already used the generated client types. On
  **Postgres**, a pre-existing `Number` `has_many` column (wrongly typed numeric,
  which rejected writes) is migrated to `TEXT` automatically on the next startup —
  no manual step.

- **Relationship-population `depth` defaults consistently to `[depth] default_depth`.**
  gRPC `Find` and the Lua reads previously defaulted an unset `depth` to `0`
  (IDs only), while gRPC `FindById` and MCP defaulted to the configured
  `default_depth` (`1`). Every surface now resolves an unset `depth` to
  `default_depth`, floors a negative `depth` to `0`, and caps at `max_depth`.
  **Action:** if you relied on gRPC `Find` / Lua returning bare IDs by default,
  pass `depth = 0` explicitly (or set `[depth] default_depth = 0`).

- **`unpublish` / `undelete` on a collection that doesn't support them now errors.**
  Calling `unpublish` on a collection without versioning, or `undelete` on one
  without soft-delete, previously silently fell through to a normal update on
  gRPC and the admin UI (only Lua errored). All surfaces now return a clear
  error. **Action:** none, unless you called these operations on a collection
  that never supported them — enable `versions` / `soft_delete`, or stop calling
  them there.

- **Restoring a version preserves the snapshot's publication status.**
  `restore_version` used to force-publish the document whatever the snapshot's
  status was. A restore now returns the document to its exact state at that
  point in time: a draft snapshot restores as a draft, a published one as
  published. On a collection without a status axis every snapshot is
  `published`, so nothing changes there. **Action:** if a workflow relied on
  "restore always publishes", publish explicitly after restoring a draft
  snapshot.

- **The `bulk` queue is seeded with its own defaults.** Queued bulk operations
  (`queue = true` on the gRPC/MCP bulk ops) run as `_system_bulk` job runs on a
  queue named `bulk`, which is now seeded with `concurrency = 1` (a run holds a
  write transaction for its whole batch, so two at once would contend),
  `timeout = 3600` seconds (large batches are the reason to queue at all) and
  `retries = 0` (the batch is atomic, but a crash between its commit and the
  completion mark would make a retry re-apply the whole thing — re-queue
  explicitly instead). **Action:** none by default — a worker with no `--queues`
  filter serves every queue. If you run `crap-cms work --queues …` with an
  explicit list, add `bulk` to it or queued bulk ops sit pending forever.
  Override any of the three defaults under `[jobs.queues.bulk]` in `crap.toml`;
  `bulk`, like `images` and `email`, is exempt from the "configured queue that
  no job uses" startup warning.

## Additive features (alpha.10)

### `access.unlock` — a dedicated gate for account lock/unlock

Auth collections gain an `access.unlock` key. It authorizes the account
lock/unlock operations (`LockAccount` / `UnlockAccount` on gRPC and their
admin equivalents) and, when unset, falls back to `access.update` — so
existing projects behave exactly as before.

```lua
crap.collections.define("users", {
    auth = { enabled = true },
    access = {
        update = "hooks.access.self_or_admin",
        unlock = "hooks.access.admins_only",   -- only admins may unlock
    },
})
```

`Verify` / `Unverify` are **not** covered by it — they keep using
`access.update`. Setting `unlock` on a non-auth collection logs a warning (the
lock/unlock operations do not exist there), and a global rejects it at load
(item 6).

Additive; no action needed.

### gRPC MFA completion (`VerifyMfa`) + the `mfa_when` gate

- New `VerifyMfa` RPC completes an MFA-gated gRPC login (see Security fixes
  above). `LoginResponse` gained optional `mfa_required` / `mfa_challenge`
  fields — regenerate stubs; existing clients on non-MFA collections are
  unaffected.
- New optional `mfa_when` key on the `password_login` auth method: a Lua
  hook deciding WHETHER a verified login needs the second factor, called
  with `{ collection, user, surface, headers }`. Return `false`/`nil` to
  skip, truthy to require — so MFA can apply per surface
  (`ctx.surface == "grpc"`) or per user field (`ctx.user.mfa_enabled`).
  No hook = MFA always required; a hook error fails closed.

### Custom cache backend (`[cache] backend = "custom"`)

Previously accepted-but-inert; now real. Register a Lua handler with
`crap.cache.register({ get, set, delete, clear, has? })` in `init.lua` and
the populate cache delegates to it — for shared stores the built-in
backends don't cover. Selecting the backend without a registration fails
startup. See [crap.cache](../lua-api/cache.md).

### Self-service verification resend

A user whose verification email was lost or has expired no longer needs an
administrator. `/admin/resend-verification` takes an address and mails a fresh
link; the login page links to it whenever email is configured and at least one
collection sets `verify_email`. The same flow is available to API clients as
the `ResendVerification` RPC:

```bash
grpcurl -plaintext -d '{
    "collection": "users",
    "email": "user@example.com"
}' localhost:50051 crap.ContentAPI/ResendVerification
```

Issuing a link retires the previous one, so only the newest email works. Like
`ForgotPassword`, the call always reports success — an unverified account, a
verified one, a locked one, and an address that was never registered are
indistinguishable in the response, so the endpoint cannot be used to test which
addresses exist. It shares the forgot-password rate-limit budget, per address
and per IP.

This matters most right after the upgrade: item 19 invalidates every
verification link already in flight, and this is how users recover without
opening a ticket.

### `crap.validation_error` — reject a write on a named field

A hook could only fail with `error("…")`, which reaches the caller as an
opaque hook error and, on an admin form, as a general message. There is now a
structured form:

```lua
function M.check(ctx)
    if ctx.data.title and #ctx.data.title > 80 then
        crap.validation_error({ title = "keep the title under 80 characters" })
    end
    return ctx
end
```

It takes a table of field name to message and never returns — it raises,
aborting the write. Every surface reports it where a built-in validator's
message would go: `INVALID_ARGUMENT` over gRPC, an error under the `title`
input on an admin form. Pass at least one field; an empty table is itself an
error, so a mistake in the hook cannot let the write through. Plain
`error("…")` still works and still means "opaque failure".

### MCP accepts JSON-RPC batches

Both transports now take an array of request objects in place of a single one.
Every member runs and the reply is an array holding one response per member
that carried an `id`, in the order sent. A batch of nothing but notifications
gets no reply at all (HTTP `204`).

A batch may hold at most `[mcp] max_batch_members` members (default `50`);
an over-long batch and an empty array are refused whole with a single
`-32600` error rather than expanding into unbounded work. Setting
`max_batch_members = 0` disables batching entirely — every batch is then
refused with "Batching is disabled". The `initialize` handshake may not appear
in a batch, so a batch never opens a session.

### Smaller additions

- `[mcp] http_max_body_bytes` — configurable `POST /mcp` body cap
  (default 1 MiB, filesize strings accepted).
- `crap-cms db console` opens `psql` on PostgreSQL (was SQLite-only).
- `crap-cms jobs healthcheck` exits `0`/`2`/`1` for healthy/warning/
  unhealthy — CI gates on it now actually fire (it always exited `0`).
- `crap.globals.<slug>.unpublish()` / `.validate()` accessor sugar.

### Custom MFA delivery (`mfa = "custom"` + `mfa_deliver`)

The MFA mode gained a third value: `mfa = "custom"` keeps the built-in code
generation, storage, verification, rate limiting, and challenge flow, but
hands delivery to the required `mfa_deliver` Lua hook
(`{ collection, user, code, expires_in }`) — SMS, push, chat, anything.
Startup validates the pairing (`custom` without the hook, or the hook
without `custom`, is a boot error). Works identically on the admin MFA page
and the gRPC `Login`/`VerifyMfa` flow, and composes with `mfa_when`.

### gRPC wire-parity additions

- `UpdateGlobalRequest` gained optional `draft` (save a global as an
  unpublished draft — parity with MCP/Lua/admin).
- `UndeleteRequest` gained optional `events` (quiet restore — parity with
  every other write RPC).
- `ListVersionsRequest` gained optional `offset` (version-list pagination —
  parity with MCP/Lua, which already took one).
- `CreateManyRequest.locale` is now **honored**: it existed in the proto but
  the handler silently ignored it. Bulk create now routes the locale through
  the same chokepoint as single `Create` — an explicit default locale is
  accepted, and a NON-default locale is rejected with `INVALID_ARGUMENT`
  (create in the default locale first, then translate via update), exactly
  like single create, instead of silently writing the default columns.

### One `where` grammar on every surface

gRPC, MCP, and Lua CRUD now decode `where` through one shared decoder:
scalar shorthand (`{ field = value }` ⇒ equals), operator objects, and `or`
groups everywhere. Concretely new: **MCP accepts `or` groups** (previously
rejected). Two behavior notes: MCP boolean shorthand now compares as
`true`/`false` instead of `1`/`0` (identical matches — the SQL edge coerces
per column type), and a non-scalar element inside `in`/`not_in` is now an
ERROR on MCP instead of being silently dropped (a dropped element silently
changed the match set).

### Validate dry-runs: one body, real access semantics

All eight validate endpoints (collection + global on gRPC/MCP/Lua/admin) run
one shared operation body. Two behavior changes: **MCP validate now runs
with the same trusted override as MCP's real writes** (it previously
evaluated field-access as an anonymous user, so its dry-run could report
field strips the actual write would never apply), and **gRPC/MCP validate
now run inside a rolled-back transaction** like the admin endpoint always
did — side effects of `before_validate` hooks during a dry-run are
discarded instead of persisting.

Additionally, **validate now follows the target operation's access rules**:
the dry-run evaluates `access.create` (create mode) / `access.update`
(update mode and globals) exactly like the write it previews — an anonymous
or unauthorized caller gets `PERMISSION_DENIED` instead of validation
results (previously an ungated dry-run let anyone probe `unique` collisions,
e.g. registered emails). This applies to gRPC, admin (unchanged — it already
gated at the endpoint), and Lua (`crap.collections.validate` now runs the
access check for the hook user; pass `override_access = true` for trusted
internal dry-runs, same as the other Lua CRUD calls). MCP is unaffected
(trusted override). Collections without an access rule on the target op are
unchanged.

### MCP + Lua option parity

- MCP `find_*` / `find_by_id_*` accept `select` (field-name projection);
  `count_*` accepts `search` and `locale`; `unpublish_*` / `undelete_*`
  accept `events` — the same query now means the same thing on MCP, gRPC,
  and Lua.
- Lua `crap.collections.unpublish(id, opts?)` accepts `events = false` for a
  quiet unpublish, matching `crap.collections.update{ unpublish = true }`.
- Lua `crap.collections.undelete(id, opts?)` accepts `events = false` for a
  quiet restore (previously rejected as an unknown key).
- `create_many` accepts `locale` on every surface (MCP argument, Lua option,
  honored gRPC field) — bulk create in a non-default locale.
- MCP `global_update_*` tool schemas now advertise the `draft` argument the
  codec already accepted.

### `[server] public_schema_introspection` — gate schema discovery

New boolean, default `true` (unchanged behavior). The gRPC schema-introspection
RPCs (`ListCollections`, `DescribeCollection`) are readable without auth by
default, as in a headless CMS. Set it to `false` in production to require an
authenticated caller — the schema shape (collection and field names/types) is
then hidden from anonymous clients. It never gates document data, which is
always access-controlled.

```toml
[server]
public_schema_introspection = false   # require auth to read the schema
```

### Separate read/write connection pools (`[database] write_pool_max_size`)

Reads and writes now draw from independent connection pools instead of one
shared pool. Under SQLite WAL an unlimited number of readers run concurrently
while a single writer serializes; with one pool, a burst of concurrent writers
could consume every connection and starve readers (read latency and error rate
spiked under mixed read/write load). Reads now use a large pool and writes a
small separate pool, so read throughput stays independent of write load.

`pool_max_size` (default `64`, unchanged) now sizes the **read** pool — the one
that governs read concurrency — and a new `write_pool_max_size` (default `4`)
sizes the write pool:

```toml
[database]
pool_max_size = 64        # read pool (was: the single shared pool)
write_pool_max_size = 4   # write pool (SQLite only)
```

Writes take `BEGIN IMMEDIATE` and serialize on SQLite's single writer, so a
small write pool is correct — excess concurrent writers queue on checkout
instead of starving readers. Raising `write_pool_max_size` does **not** increase
SQLite write throughput (the engine still serializes writers). On **Postgres**,
which handles concurrent writers via MVCC, reads and writes share one pool and
`write_pool_max_size` is ignored.

**Action:** none. Defaults preserve behavior; `pool_max_size` keeps sizing the
pool that matters for read concurrency. Tune `write_pool_max_size` up only if a
write-heavy deployment sees write-pool checkout timeouts under sustained
concurrent writes.

### Elastic Lua VM pool (`[hooks] max_vm_pool_size`)

The hook-runner VM pool is no longer fixed-size. Previously, once concurrent
hook execution exceeded `vm_pool_size`, further requests blocked up to 5 seconds
waiting for a VM. The pool now **pre-warms** `vm_pool_size` VMs and **grows on
demand** up to a new `max_vm_pool_size` cap, reusing returned VMs across threads;
it waits only when every VM up to the cap is busy.

```toml
[hooks]
vm_pool_size = 8          # VMs pre-warmed at startup (was: the hard cap)
# max_vm_pool_size = 64   # hard cap; grows up to this (default: cores × 8, min 32)
```

`vm_pool_size` changes meaning from a hard cap to the **pre-warm count**.
`max_vm_pool_size` (default `cores × 8`, min 32) bounds how many VMs the pool can
create — each holds the full registry/Lua state, so this bounds worst-case
memory. It is clamped up to `vm_pool_size` if set lower.

**Action:** none. Defaults raise the effective concurrency ceiling without config
changes. If you had raised `vm_pool_size` purely to avoid the 5-second blocking
under load, that ceiling is gone — you can lower it back toward the pre-warm you
actually want and let the pool grow. Lower `max_vm_pool_size` if you need to cap
VM memory more tightly.

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
