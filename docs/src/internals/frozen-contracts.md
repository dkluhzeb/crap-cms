# Frozen Contracts

This page lists the parts of crap-cms that are a **permanent contract** as of
the stabilization release. Each is correct as-is — it is recorded here so a
future change is a deliberate, breaking decision rather than an accidental
"cleanup". Anything on this list can be **extended** (new variants, new keys)
but not renamed, removed, or reshaped without breaking existing users' configs,
stored data, or clients.

**Freeze policy during the alpha series:** every alpha release *targets* a
complete freeze of this page. If a contract nonetheless turns out to be wrong,
it is fixed **properly** — a clean breaking change with an upgrade-guide entry
and migration gate — never preserved through a compatibility workaround, and
the freeze target moves to the next alpha. The project moves to **beta** only
once an alpha cycle has passed with this page untouched; from beta on, the
freeze is unconditional.

## On-disk / storage (changing any = a data migration)

- **System column namespace.** All `_`-prefixed columns (`_status`,
  `_deleted_at`, `_ref_count`, `_order`, `_locale`, `_block_type`, the auth
  columns `_password_hash`/`_reset_token`/… , version columns
  `_parent`/`_version`/`_latest`/`snapshot`) and the non-prefixed `id` /
  `parent_id` / `created_at` / `updated_at`.
- **Companion/derived column suffixes.** `{field}_tz` (timezone), `{field}_lang`
  (code language). The suffixes are reserved from user field names.
- **Upload metadata columns.** `filename`, `mime_type`, `filesize`, `width`,
  `height`, `url`, `focal_x`, `focal_y`, plus per-size/format variants.
- **Naming schemes.** Group columns `group__field`, localized columns
  `field__locale` (and `group__field__locale`); join tables `{collection}_{field}`
  and `{collection}_{group}__{field}`; global tables `_global_{slug}`; version
  tables `_versions_{slug}`. Identifiers are capped at **63 bytes** (Postgres).
- **JSON storage shapes.** The blocks `data`-column split (`id` + `_block_type`
  hoisted out, everything else in `data`) — the `_block_type` discriminator key
  has one canonical source, `core::BLOCK_TYPE_KEY`, read through it by every
  block-aware surface (the one unavoidable literal, the admin `BlockRow` serde
  `rename`, is pinned to it by a test) — the "relational spine vs nested JSON"
  boundary (top-level array/blocks/relationship get join tables; anything nested
  inside a row is JSON), and the version-snapshot JSON shape (restore must read
  every snapshot ever written). A **group** nested in a row (array or block) is
  stored as a JSON **object** (`{…}`), not a one-element array — the block form
  parser selects a row's sub-field defs from its `_block_type` so the group is
  recognized as a single-object composite.
- **Values inside JSON-stored rows have one typed form.** In a blocks row, and in
  any group, array or blocks nested inside a row, a value is stored the same way
  whichever surface wrote it:
  - a checkbox is `true`/`false` — a truthy spelling (`1`, `true`, `yes`, `on`,
    trimmed, any case) or a number other than `0` is checked;
  - any other blank string is `null`;
  - a number, date, text, textarea, email or scalar has-many list is what its
    column would hold — a number as a number (surrounding whitespace ignored), a
    timezone date as UTC, a list as a typed array; a value the column can't hold
    stays as sent (validation rejects it);
  - a has-many relationship or upload is its id list — a JSON array of id
    strings (`collection/id` when polymorphic), whether the write carried a
    list, a JSON array as text or the admin form's comma-separated ids;
  - every other type — JSON, rich text, code, select, radio and single
    references — is stored as sent.

  A missing value stays missing: only the admin form reads a checkbox absent
  from a submitted row as unchecked, because HTML omits unchecked boxes.

  The one-time conversion that brings existing rows to this form reads both
  shapes a scalar has-many list was ever written in: a JSON array, and the
  **comma-separated string** (`"a,b"`) the admin form stored before. A
  conversion that only understood the newer shape would leave the older rows
  as a single-element list holding the whole string — silently wrong data, and
  unrecoverable once the gate is stamped. Any future conversion of a stored
  form owes the same debt: read every shape the value was ever written in, not
  only the one written last.
- **A stored upload file is deleted when, and only when, no live row, draft or
  version snapshot of its document references it, after the write commits.**
  Reference is decided by the same rule that derives keys from a row
  (`upload_file_entries`) applied to the live row and to every snapshot of the
  document; there is no stored counter. Pruning a snapshot releases the files
  it was the last reference to — the release point is the settle step of the
  write that pruned; purge deletes them all.
- **Publishing takes the latest draft as its base.** An update with
  `draft = false` while a draft is pending merges the latest draft snapshot
  under the request's fields at the service write chokepoint, so every
  surface publishes the same thing; the request wins per field, the write
  access strip still applies to adopted values. The pending draft is one unit:
  a publish in one locale publishes every locale's drafted values and the
  draft's shared (non-localized) values. Unpublishing keeps the pending draft
  as the pending draft (no new snapshot); without one it snapshots the live
  row. Globals follow the same rule.
- **Every version write goes through one path** (`create_version_and_prune`):
  it locks the parent row, inserts the snapshot and prunes to `max_versions` —
  publish, draft save, unpublish and restore alike. There is no second way to
  write or prune a version.
- **Timestamp write format is one ISO-8601 `…Z` shape on every backend.** Both
  the app-side clock (`utc_now()`, bound as a parameter) and the SQL "current
  time" expression (`DbConnection::now_expr()`, plus `date_offset_expr()` for job
  retry scheduling) produce `YYYY-MM-DDTHH:MM:SS.mmmZ`. This matters because
  timestamp columns are `TEXT` and compared **lexically** (sort keys, cursor
  pagination, `retry_after <= now`), and a column such as `updated_at` is written
  by both paths — a status change uses `now_expr()`, an ordinary edit binds
  `utc_now()`. SQLite must not fall back to `datetime('now')` (space separator,
  no millis/`Z`), which collates before the ISO form. Legacy rows written before
  this are normalized to ISO on read (`normalize_timestamp`).
- **Column types.** Timestamps and dates are `TEXT` (ISO-8601) on every backend;
  numbers are floating point (`REAL`/`DOUBLE PRECISION`); integers/flags are
  `BIGINT` on Postgres. Whole-valued numbers serialize back as JSON integers.
  A **scalar `has_many` list** (`Text`/`Number`/`Select`/`Radio` with `has_many`,
  i.e. `FieldDefinition::is_has_many_scalar`) is stored as a JSON array in a
  `TEXT` column regardless of the base type (a numeric column can't hold the
  array and Postgres rejects it). One `ColumnSpec::ddl_type` decides this for the
  CREATE and reconcile paths (the collection and global alter paths share one
  `reconcile_scalar_list_column`, which flips a column that drifted to numeric
  on an older Postgres database back to `TEXT`); the write edge canonicalizes each element to the
  field's type (`coerce_has_many_scalar`) and the read path parses it back
  (`parse_has_many_scalar`), so the list round-trips identically across surfaces.
- **Every stored has-many list is NULL or a JSON array.** A scalar `has_many`
  column (every locale's, every `group__` one, an array row's), a scalar
  has-many value inside a JSON-stored row, and the id list of a has-many
  relationship or upload stored in a row hold nothing else; the list filters
  expand them unguarded. Writes keep it so; the schema sync
  (`db::migrate::has_many_lists`, gated per table on a versioned fingerprint of
  its list fields) rewrites a value a definition change left behind — a single
  value becomes a one-element list, read by the write's own list reading
  (`stored_list`) — and refuses to start, naming the documents, on a value
  holding nothing of the field's type rather than dropping it. Text that isn't
  a JSON array reads by where it is stored (`ListPlace`): in a document's own
  column (top-level, per locale, `group__`) it is **one value**, since no
  release stored a list there in any other form; inside an array or blocks row
  (an array table's column, a row's JSON) it is **comma-separated values**,
  the form earlier admin forms stored a row's list in. Every write path and
  `crap-cms import` maintain the invariant; reads and filters do **not** guard
  against a value that breaks it. A row written around the application — raw
  SQL, an external ETL job — that leaves non-JSON text in such a column makes
  every filter on that field fail with a query error (the JSON expansion
  rejects it) rather than silently mis-match: fail loud, never wrong. The
  schema sync repairs such values the next time the table's has-many fields
  change; keep external writers to JSON arrays or NULL.
- **The soft-delete rebuild preserves data and drops only inline UNIQUE.**
  Enabling `soft_delete` on a table with unique fields rebuilds it to replace
  inline `UNIQUE` (which would block re-inserting a value whose row is trashed)
  with a partial `WHERE _deleted_at IS NULL` index. The rebuild trigger walks the
  flattened column specs (so a unique field nested in a group/row/tabs is
  covered), and re-adds orphan columns before copying so no row data is lost —
  the same preserve-orphans, drop-nothing contract the rest of the alter path
  follows.
- **The image decompression-bomb check fails closed.** If an upload's header
  dimensions can't be read, it is rejected rather than passed through to a full
  decode — the pixel-count and pixel-per-byte caps run before any decode, never
  after.
- **System tables** (`_crap_meta`, `_crap_migrations`, `_crap_cron_fired`,
  `_crap_user_settings`, `_crap_jobs`) and the `_crap_meta` one-time-migration
  gate keys — renaming a gate key re-runs the migration on every existing
  database. The gate **value** is the intended re-run lever; never rename the
  key. The keys in use, every one scoped to a single target so no conversion
  can be skipped for a collection added later:
  `ref_count_backfilled:{slug}`, `checkbox_columns_smallint:{slug}`, and —
  keyed by *table*, so a collection and a global of the same slug cannot share
  a gate — `legacy_timestamps:{table}`, `nested_values:{table}`,
  `canonical_text:{table}` (`posts`, `_global_site`, …). Two keys are records,
  not gates: `locale_config` holds the locale fingerprint (default locale +
  sorted codes) so startup can warn when the default locale changed against
  existing data, and `locale_shape:{table}` holds `{version}:{sorted localized
  columns}` so a flip of a field's `localized` flag moves its values exactly
  once per flip, in either direction.
- **The user-settings blob shape.** `_crap_user_settings` holds one JSON object
  per user: `ui_locale` at the top level, per-collection list preferences under
  `collections.{slug}` (`{"columns": [...]}`). Entries written before the
  `collections` namespace (`{slug}.columns` at the top level) are still read and
  move under `collections` on that collection's next save; the reader
  (`service::user_settings::UserSettings`) must keep reading them.
- **A retired gate key is deleted, never left behind.** A database must carry
  no key naming a pass nothing reads any more — an orphaned whole-database flag
  is exactly what makes a later-added collection skip its conversion forever.
  A conversion that replaced an earlier one removes the retired key itself
  (`legacy_timestamps_normalized`, `nested_timezone_dates:{slug}`); the ones
  with no surviving owner go in `RETIRED_META_KEYS`, deleted at startup
  (`ref_count_backfilled`, the whole-database flag the per-slug gates
  replaced).
- **Generated SQL identifiers are always quoted.** Every column/table name
  interpolated into generated SQL (CREATE/ALTER/INSERT/UPDATE/SELECT and FTS
  sync) goes through `quote_ident`, so a field named after a SQL reserved word
  (`order`, `select`, `group`, …) is valid on both backends. SQLite runs with the
  double-quoted-string misfeature disabled (`SQLITE_DBCONFIG_DQS_DDL`/`_DML`
  off), so a double-quoted token is unambiguously an identifier — a reference to
  a missing column errors rather than silently reading as the literal string.
  Never emit an unquoted user-derived identifier, and never re-enable DQS.

- **Redis key layout.** Cache keys live at `{cache.prefix}cache:{key}`, and a
  cache clear deletes only `{cache.prefix}cache:*`; rate-limit counters live under
  `auth.rate_limit_prefix`. Startup refuses a rate-limit prefix that overlaps the
  cache namespace on the same Redis. The rate-limit keyspace names shared by
  every surface (`ip_reset_password`, `ip_verify_email`, `mfa_issue`,
  `resend_verification`, `ip_resend_verification`) are part of the layout:
  renaming one resets its live counters.

- **Every timezone date is stored as UTC.** A date with `timezone = true` holds
  an ISO 8601 UTC value (`…Z`) and its IANA zone in the `{field}_tz` companion —
  in its own column at the top level and in array rows, and inside the JSON of
  blocks rows, groups within rows and nested rows. Writes convert local
  wall-clock input with the zone; a value that already carries an offset is
  stored as given, so re-saving never shifts a date.

- **When a companion is written depends on whether it gives the value its
  meaning.** A companion bound to the value — a timezone date's `{field}_tz`,
  without which the date is ambiguous — is written **whenever the value is**,
  so the two can never drift apart. Any other companion — a code field's
  `{field}_lang`, the editor's language pick, which the value is readable
  without — is written **only when its own key is sent**, so a write that omits
  it keeps the stored pick instead of clearing it. One table
  (`companion_descriptors`) carries the rule per companion and every write path
  reads it; a surface never decides per suffix. `{field}_lang` exists only for a
  Code field with a non-empty `admin.languages` allow-list — with no allow-list
  there is nothing to pick from, so no column is created.

- **Version snapshots record localized join fields per locale.** For a
  localized array, blocks or has-many relationship field, a snapshot holds each
  locale's rows under a flat `{field}__{locale}` key (`{group}__{field}__{locale}`
  inside a group, the locale code in column form: `pt-BR` → `pt_BR`), next to the
  bare key holding the default locale's rows — the same convention as localized
  scalar columns. Restore writes each locale back
  from its own key; a snapshot without per-locale keys (taken before alpha.10)
  leaves localized join rows untouched. A draft save changes only the saving
  locale's key, and builds on the latest draft snapshot when one exists.

- **Email and text values are stored canonical.** An `email` field value is
  stored trimmed, NFC-normalized and lowercased; Text, Textarea and Email values
  are NFC-normalized on every write (nested row values included). Account
  lookups (login, password reset, verification, CLI), uniqueness and filter
  operands on these fields use the canonical form — in SQL and in the in-memory
  constraint matcher alike, a `like` pattern judged after canonicalization.
  Stored values are rewritten at startup, per collection or global, whenever
  the set of its email and text columns differs from the last pass (gate:
  `_crap_meta` key `canonical_text:{table}` — the *table*, so `posts` for a
  collection and `_global_site` for a global of the same slug cannot share one
  gate — value `{version}:{fingerprint}`);
  a value colliding once canonical in a unique field or unique index stops
  startup.
- **Index names are unique per database.** Startup rejects two indexes — of
  one collection or of two — that would get the same `idx_{slug}_…` name.
- **Array / blocks row tables carry a `parent_id` index** named
  `idx__rows_{table}` on `(parent_id)`, or `idx__lrows_{table}` on
  `(parent_id, _locale)` for a localized one; the prefixes are disjoint from
  every `idx_{slug}_…` and `idx__ver_…` name. A name that would pass 63 bytes
  keeps its prefix and the first bytes of the table name, then `_` and the
  first 16 hex digits of the table name's SHA-256 — 63 bytes exactly. The
  sync drops any other `idx__rows_` / `idx__lrows_` index of the table, so
  the name form can change without leaving a stale index.
- **A join's `on` is a top-level, has-one, single-target relationship or
  upload field of the target that references the owning collection** —
  checked at load; a join in a global is rejected.

## Client-visible shapes

- **Field read shapes are the same at every nesting depth and on every
  surface**: a checkbox is `true`/`false` (never the column's `0`/`1`), a
  `json` field is the parsed value, a scalar has-many list is a JSON list, a
  number that is whole is an integer. One decode (`decode_value`) produces
  them for columns, group columns and array-row columns; JSON-stored rows are
  written in that form.
- **Returned document shape.** `id`, the field columns, `created_at`,
  `updated_at`. Localized fields under `locale = "all"` are a per-locale map
  (`{ en = .., de = .. }`); single-locale reads return the scalar.
- **Pagination object** (`result.pagination`): snake_case fields `total_docs`,
  `limit`, `page`, `has_next_page`, `has_prev_page`, `total_pages`, `page_start`,
  `prev_page`, `next_page`, `start_cursor`, `end_cursor`.
- **Result array key is `documents`** across `find` / `create_many` /
  `list_versions`. Bulk count keys: `created` / `modified` / `deleted` +
  `skipped`.
- **`like` / `contains` matching.** ASCII-case-insensitive on every backend
  and in memory; `%` matches any run of characters including line breaks, `_`
  one character, and `\` escapes the next character (`ESCAPE '\'`). A `like`
  pattern ending in a lone backslash is rejected by filters and never matches
  when a constraint is evaluated in memory.
- **Polymorphic relationship read format** `"collection/id"`.
- **Filter DSL.** The operator set (`equals`, `not_equals`, `like`, `contains`,
  `greater_than`, `less_than`, `greater_than_or_equal`, `less_than_or_equal`,
  `in`, `not_in`, `exists`, `not_exists`) is the **one grammar every surface**
  speaks — the gRPC/JSON `where` API, the admin list URL, MCP, the Lua filter
  representation, and (since alpha.10) the access-constraint tables — all
  single-sourced through `FilterOp::op_name` / `FilterOp::scalar_from_name`
  and decoded by `decode_where_map`; the grammar's *description*
  (`FILTER_OP_SPECS`: names, value shapes, docs) is likewise single-sourced
  and pinned to the enum by a consistency test. An empty group inside `or`
  is a hard error on every surface (it would vacuously match every row). (Alpha ≤10 spelled the ordered operators
  differently per surface — the admin URL's terse `gt`/`gte`/`lt`/`lte` and MCP's
  `greater_than_equal`/`less_than_equal` — those short forms were removed in
  favor of the single verbose grammar.) Empty-`in` → no match / empty-`not_in` →
  all match, plus the dot-notation nested-path grammar. The lenient checkbox
  filter value (`1/true/yes/on`) is a **deliberate, permanent** leniency; a
  filter value that does not fit the field's type (a non-numeric number) is a
  validation error.
- **Has-many filters are element-wise.** A filter on a list — a scalar
  `has_many` field, a has-many relationship/upload's `.id` — quantifies over
  its elements: `equals`, `like`, `contains`, `in`, the ordered comparisons and
  `exists` match when some element does; `not_equals`, `not_in` and
  `not_exists` when no element matches the positive operator. An empty or
  unset list holds no elements. A has-many relationship/upload inside an array
  or blocks row reads its stored id list the same way, a polymorphic entry by
  the id after its `collection/`. SQL (`EXISTS` / `NOT EXISTS` over the
  expanded list or the junction rows) and the in-memory evaluator apply the
  same reading. A has-many list is never a sort key. Array and blocks rows
  stay records: a sub-field filter asks for some row that satisfies it.
- **Cursor token format** (base64url JSON) — kept decodable for in-flight URLs.
- **Timestamp formats are deliberately per-concern and must not be "unified".**
  Persisted document date values normalize to millisecond ISO 8601 UTC
  (`YYYY-MM-DDTHH:MM:SS.000Z`, via `utc_now` / `normalize_date_value`). The
  event-stream payload `timestamp`, the Lua `now()` helper, and the export /
  backup / scaffold manifest timestamps are RFC 3339 (`to_rfc3339`), an outward
  contract for their consumers. The scheduler's cron-window dedup keys
  (`_crap_cron_fired`) are full-precision RFC 3339 compared only against each
  other. These serve distinct consumers; forcing one format would either break a
  wire contract or lose scheduler precision.

## Generated client types (`typegen client` / `typegen proto`)

These are the frozen output *shapes* of the code generators. The files are
regenerated by the user, but consumers write code against these shapes, so
changing a representation is a breaking change to every consumer.

- **Relationships are populate-aware and dual-form.** A relationship/upload
  field generates a type that is *either* an id string (`depth = 0`) *or* the
  populated document (`depth >= 1`): Rust `Rel<T>` (`#[serde(untagged)] enum {
  Doc(Box<T>), Id(String) }`), Go `Rel[T]` (struct + custom JSON), TypeScript
  `string | TDocument`, Python `str | T`; a has-many field is a list of that. Do
  not flatten either side back to a bare id. This is the READ side only: the
  write (`…Data`) types carry every reference as its id (`string` /
  `string[]`, a polymorphic one as its `"collection/id"` string), because every
  write surface rejects a populated document.
- **Read and write types come from two wire shapes, never one from the other**
  (`core::upload::read_shape`). The read shape drops `hidden` fields at any
  depth and folds an upload's per-size columns into `sizes`; the write shape
  drops virtual `join` fields and the server-derived upload columns
  (`CollectionUpload::derived_field_names`, the set the write chokepoint
  strips), keeps each field's own `required` (a required relationship is
  required), and — on an auth collection — adds an optional `password`. The
  Lua `crap.input.*` / `crap.partial.*` / `crap.partial_many.*` classes (the
  last never with `password`: `update_many` refuses one) follow the write
  shape and `crap.doc.*` the read shape; `crap.data.*` stays the stored shape a
  write hook's `ctx.data` holds, and an `after_read` hook's context
  (`crap.read_hook.*`) types `ctx.data` with the read shape. The typed
  `crap.where.*` keys and `order_by` values follow the queryable columns — a
  `hidden` field's are never among them.
- **A collection read type declares the populated `collection` tag** as the
  one-value literal of its slug (TypeScript `collection?: "posts"`, Python
  `Optional[Literal["posts"]]`, Go `*string`, Lua `collection? "posts"`); Rust
  leaves it to the polymorphic enums' serde tag. It is omitted when a field of
  the collection is itself named `collection`, and it never renames a field: a
  Go tag member whose name a field's member already holds takes the suffix
  (`Collection_2`). `_status` is `"draft" |
  "published"` in TypeScript, Python and Lua and a plain string in Rust and Go.
- **Every field of a read type is optional**, independent of the write-side
  `required` flag, and in the group and row types nested inside it. A read can
  omit any key: a draft read returns required fields empty, field read access
  strips them, and `select` leaves them out. In TypeScript the optionality is
  also nullable (`?: T | null`) because an empty value reads as `null`; in Go a
  boolean and a single group read through a pointer, so an absent field is
  distinguishable from `false` / an empty group. The write (`…Data`) types keep
  their required fields — a read type is never derived by extending a write
  type.
- **`select` narrows to a named type, and Rust/Go are lossless.** Rust
  `enum { …, Other(String) }` (`serde(from/into)`), Go a `string` newtype with
  consts — both must preserve an unknown value, not reject it. TypeScript a
  string union, Python a `Literal`.
- **Polymorphic relationships are a discriminated type over their targets**
  (Rust untagged enum + a `#[serde(tag = "collection")]` ref enum, TS/Python a
  union of the target documents, Go `interface{}`) — never a bare string.
- **In TypeScript every collection and global splits `…Data` (the write
  shape) and `…Document` (the read shape, adds `id` + timestamps); Rust, Go and
  Python emit the read type only. `CollectionSlug` enumerates the slugs.**
- **Identifier safety and collision policy are frozen.** A name that collides
  with a language's rules is sanitized per language with the **wire key
  preserved** (never rename the wire key to fix a language identifier); a
  type-name collision is a hard generation error, not a silent rename.
- **Rust `typegen proto` decodes into the `typegen client -l rs` structs** — the
  two are one contract and must compile together, including decoding a populated
  relationship (`Rel::Doc`) nested at any depth. Guarded structurally (both
  parse with `syn`, and every decoder builds exactly the fields of its client
  struct — `typegen::golden_tests`); a real compile of the pair needs a Rust
  toolchain at test time and is not part of the suite.

## Generated Lua types (`typegen lua`)

- **Class and alias names are `crap.<namespace>.<PascalSlug>`** (and
  `…global_<slug>` for the global hook contexts), the slug `PascalCase`d but
  otherwise unchanged — a leading digit stays (`crap.data.2fa`), since a
  dotted LuaLS type name only has to start with a letter. Hook authors write
  these names in `---@type` annotations; never rename them.
- **A key that isn't a bare Lua identifier is a quoted index**: a leading
  digit, a Lua reserved word, or a LuaLS field-scope word (`public`,
  `protected`, `private`, `package`) is declared `---@field ["2fa"]? string`
  and bound `crap.collections["2fa"]`; LuaLS keys it by the string, so
  `data["2fa"]` is typed. The typing factories are declared on the
  class-bound accessor local (`function _coll_<slug>.hook(fn) end`), never as
  `function crap.collections.<slug>.hook` (a syntax error for such a slug, and
  a field LuaLS refuses to inject for every slug).
- **A class-name collision is a hard generation error**, as in the client
  generators: `PascalCase` isn't injective (`a1` / `a_1`), and LuaLS would
  merge the two declarations.
- **The output checks clean under LuaLS.** Guarded hermetically against the
  annotation grammar (`typegen::lua::luals_check`) and by a real
  `lua-language-server --check` of the kitchen sink plus `types/crap.lua`
  when one is installed (`CRAP_LUALS`, `PATH`, or a Mason install).

## gRPC wire format (`proto/content.proto`)

- **Document values use the typed `FieldValue` / `DataMap` / `FieldList`
  messages**, not `google.protobuf.Struct`. `FieldValue` mirrors JSON but splits
  numbers into `int_value` (`int64`, exact) and `double_value`. A producer sets
  exactly one variant; an integer that fits `i64` uses `int_value`, a fractional
  value or an out-of-`i64` integer uses `double_value`. This is the frozen shape
  for every `data` / `fields` field — do not revert it to `Struct` (that would
  re-introduce the >2^53 rounding this replaced).
- **The soft-delete table rebuild is build-copy-drop-rename, never
  rename-first.** With SQLite foreign keys on, renaming the live table
  rewrites every child's `REFERENCES` clause and dropping it afterwards
  cascades into the children. The replacement is created under
  `_rebuild_{slug}` (`create_collection_table` takes the target *table*
  name), filled, then the original is dropped and the replacement renamed,
  with enforcement off for that sync and `PRAGMA foreign_key_check` before
  commit. The table's own unmanaged indexes and triggers, and every view and
  trigger elsewhere that depends on it (which would fail the rename), are
  recreated from their stored statements in the same transaction. Postgres
  drops the constraints in place and never rebuilds.
- **Every document (and global) update holds that row's lock from before the
  first read the write builds on until commit.** On `SQLite` that is
  subsumed by `BEGIN IMMEDIATE`; on Postgres it is `FOR UPDATE` on the
  document row. Acquisition order is parent row → relationship targets.
- **A driver error is classified in one place, by type.** `db::constraint_kind`
  and `db::is_transient` downcast to the driver's error (SQLite extended
  result codes, Postgres SQLSTATE) — never to its message text, which is
  locale-dependent on Postgres. A unique violation maps to `ALREADY_EXISTS`
  / HTTP 409, a foreign-key violation to `FAILED_PRECONDITION` / 409, a
  transient failure to `UNAVAILABLE` / 503, on every surface.
- **JSON-string escape hatches are intentional and permanent** — do NOT promote
  them to typed messages: `FindRequest.where` (a JSON filter string, so new
  operators need no wire change), `FieldInfo.type` (field-type name as a free
  string), and the job `data` / `result_json` payloads.
- **Schema introspection is a one-way lossy projection.** `DescribeCollection`
  flattens `tabs` sub-fields into `fields` (tab grouping is not reconstructable),
  and `FieldInfo.name` is the **Lua** field name (nested), never the flattened
  DB column (`group__sub`).
- **Enum defaults.** Every enum has an explicit `*_UNSPECIFIED = 0`. A value the
  server can't map collapses to `UNSPECIFIED` (e.g. a hand-written
  `scheduled_by` in `JobScheduledBy`, a non-`published`/`draft` version
  status) rather than erroring; adding an enum value is wire-safe,
  removing/renumbering is not.
- **Removed proto fields are compacted, not reserved.** While the wire format is
  pre-freeze (alpha), a removed field's tag is reclaimed by renumbering the
  survivors so the message stays gap-free. After the freeze, removed tags must
  instead be `reserved`.
- **The CRUD request messages are GENERATED — never hand-edit them.** Their
  field names, types, tags, and comments are pinned in the single-source wire
  spec (`service::op::wire_proto::PROTO_MESSAGES`, layered on the wire model
  `service::op::wire`); `cargo xtask gen-proto` renders them and `--check`
  gates CI. Tags are append-only by construction: renumbering or retyping a
  shipped field means editing the pinned spec, which the wire-parity tests
  and the regenerated diff both surface. Everything outside those messages
  (responses, auth, jobs, subscribe, the service block) stays hand-written.

- **`MutationEvent.publisher` (field 8)** identifies the publishing server
  process, and `sequence` is monotonic *per publisher*, not globally. Gap
  detection keys on the `(publisher, sequence)` pair; the admin SSE payload
  carries the same `publisher`. Empty only on an event relayed from a node that
  predates the field.

- **`FieldInfo` field metadata** carries `relationship_collections` (the
  targets of a polymorphic relationship), `has_many` (a text, number or select
  value list), `timezone` (a date with a `<name>_tz` companion) and
  `companions` (the suffixes of every companion key the field carries — `_tz`,
  `_lang` — so a later companion is described without a new field) alongside
  the existing keys; a global's `DescribeCollection` reports
  `timestamps: true`.

- **A finished bulk run's stored payload is `{"queued_by": …, "collection": …}`**
  — the queuer and the collection the visibility rules need; the request body
  is dropped once the run reaches any terminal status (completed, failed or
  stale).
- **`TriggerJob` answers `NOT_FOUND` for a job the caller may not trigger**, the
  same status as an unknown slug — also for a malformed payload, which is
  reported only to a caller the job's access rule lets through.
- **The upload API and `/uploads` resolve credentials like the admin UI**: an
  unusable credential is `401` on the upload API and anonymous on `/uploads`.
- **Account state is read from the stored account.** Every credential the auth
  evaluator resolves — bearer, session cookie, strategy — is refused when the
  account's stored `_locked` is set, and a strategy's user of a collection that
  requires verification when its stored `_verified` isn't (a flag a strategy
  hook sets on the document it returns counts too). A locked account's token
  answers `Locked` before its session version is compared.
- **A custom page's sidebar entry follows its route's access rule**: only an
  outright allow shows it; a filter table hides it. A page without a rule is
  listed and renders — `[access] default_deny` applies to collections and
  globals, not pages.
- **The sidebar's custom-page context is a view, not the registration.**
  `nav.custom_pages` entries carry `slug`, `label`, `section` and `icon` —
  never the page's `access` rule — and `nav.custom_page_sections` groups the
  same pages: one section per heading (alphabetical), the ungrouped pages
  last with no heading. Every registered page must have its
  `templates/pages/<slug>.hbs`; a missing one fails startup.

- **`data/crap.lock` is the instance lock.** `serve`, `work`, stdio `mcp` and
  every other CLI command that opens the database take it shared before opening
  it and hold it until they exit; `restore` and `migrate fresh` take it
  exclusively for the whole command. The
  data directory must be on a filesystem with file locks: a process that can't
  take the lock doesn't start. The shared lock opens an existing lock file read-only, so a
  read-only data directory works once the file exists; `restore` and
  `migrate fresh` open it for writing, as an exclusive lock needs on NFS.

## MCP (Model Context Protocol)

- **Tool-name grammar** `{op}_{slug}` for collections, `global_{op}_{slug}` for
  globals, plus the static tool names. Because `{op}` includes the compound
  forms `create_many_` / `update_many_` / `delete_many_` / `find_by_id_`, a
  collection/global slug **may not begin with `many_` or `by_id_`** — enforced at
  load (`reject_reserved_tool_prefix`).
- **Tool input schemas are strict on data keys.** Write tools reject a data key
  that is neither a reserved meta-key (`id`/`locale`/`draft`/`events`, and
  `password` on auth collections) nor a declared top-level field (layout wrappers
  are transparent). The `where` filter rejects unknown operators and malformed
  operator values loudly (never silently drops a clause).
- **JSON-RPC 2.0 conformance:** the `jsonrpc` member must be `"2.0"`; a request
  with no `id` is a notification and receives no response; error responses carry
  `id: null` when it can't be determined. Error codes are the standard set
  (`-32700` … `-32603`) plus MCP's `-32002` for an unknown resource URI. A tool
  that ran and failed is reported in-band as a successful result with
  `isError: true`; a tool name the server does not expose never ran, so it is a
  `-32602` protocol error carrying `Unknown tool: {name}`.
- **A hidden collection is indistinguishable from a missing one**, in message
  and in latency. A collection filtered out by the `[mcp]` include/exclude
  lists or hidden by its `access.mcp` rule answers a direct tool call with
  exactly the `Unknown tool` error a never-generated name gets, and
  `describe_collection` answers `Unknown collection or global: {slug}` for
  both. The `access.mcp` rules are evaluated for the whole gated set before
  the tool name is parsed, so the two cases cost the same; a per-slug
  evaluation would time-stamp which names are real. Unresolvable rules hide
  every gated slug (fail closed) on listing and on execution alike. MCP is not
  a collection enumeration oracle.
- **`read_config_file` redacts `crap.toml` secrets** (`auth.secret`,
  `email.smtp_pass`, `mcp.api_key`, S3 `secret_key`), matching the redacted
  `crap://config` resource. Secrets never leave the server through MCP.
- **MCP is a machine surface**: transport-authenticated (API key over HTTP,
  process-gated over stdio) and gated per-collection by the `access.mcp` key; it
  runs with `override_access`, so per-row/field access rules do **not** further
  restrict an authorized MCP caller.
- **MCP collection schemas describe the stored document.** Block variants are
  discriminated by `_block_type`; array and blocks rows accept an optional
  `id` that keeps the row on update; has-many text and number fields are
  arrays; a timezone date has a `<name>_tz` string property; a polymorphic
  reference is a `collection/id` string constrained by `pattern`.
  `describe_collection` reports `timestamps` and `has_drafts` for collections
  and globals.

## Auth — TOTP (RFC 6238)

- **Storage columns** `_totp_secret` / `_totp_confirmed` /
  `_totp_last_step` on an auth collection with `mfa = "totp"`, added by
  the ungated per-column migration.
- **At-rest secret format** (versioned `v1`): AES-256-GCM over
  `base64(12-byte nonce ‖ ciphertext)`, key =
  `SHA-256("crap-cms:totp-secret:v1" ‖ "\n" ‖ [auth] secret)`. Rotating
  `[auth] secret` permanently invalidates enrolled secrets — enrollment
  restarts (fail-closed, `error!`-logged).
- **Parameters**: SHA-1, 30-second step, 6 digits, ±1 step verification
  window; replay guarded by a monotonic `_totp_last_step` CAS.

## MCP HTTP sessions (`Mcp-Session-Id`)

- Identity-for-audit **only** — the API key still authenticates every
  request; a missing/unknown/expired session id is **never an error**
  (fail-soft). Caps: `IDLE_TTL = 30 min`, `MAX_SESSIONS = 1024`
  (oldest-evicted).

## Job trigger options (`delay` / `unique` / `priority`)

- `unique`: a colliding key returns the **existing** run's id (not an
  error); collision scope is **pending+running only** (partial unique
  index). `delay`: integer seconds or a duration string (`"5m"`);
  negative rejected. `_crap_jobs` carries `unique_key`, `priority`,
  `retry_after` columns beyond `data`/`result`/`error`/`scheduled_by`.

## Ranked FTS search (`order_by = "_rank"`)

- Requires a `search` term; rejected with cursor pagination; `-_rank` is
  a hard error; `_rank` is carved out of the `select`-valid and
  sortable-column sets. (Supersedes the earlier "ranked search removed"
  note.)

## Hooks

- **A hook rejects a write by raising, and there are exactly two kinds.** A
  plain `error("…")` is an opaque hook failure; `crap.validation_error({field
  = "message"})` is a per-field validation failure that every surface renders
  where a built-in validator's would go. The second travels as a string,
  because that is all Lua can raise: the field errors are JSON after the
  `crap:validation-error:` prefix, written by `ValidationError::
  to_hook_message` and read by `from_hook_message` — one encoder, one decoder,
  both in `core::validate`, so the two ends of the channel cannot drift.
  A hook's message carries no translation key: the hook author wrote it, and
  an unknown key would render as the key. An empty error table is refused, so
  a mistake in the hook can never let the write through.
- **The Lua sandbox capability contract.** Hook code can never execute
  processes (`os.execute` and `io.popen` both removed), load code
  dynamically (`load`/`loadstring`/`loadfile`/`dofile` removed), or load
  native modules (`package.cpath` emptied, `package.loadlib`,
  `package.searchpath` and `string.dump` removed); `os` is reduced to
  `clock`/`date`/`difftime`/`time`. Every config-dir file is loaded as
  text — bytecode is refused on every load path (`load_source_file`).
  `require` resolves only `{config_dir}/?.lua` and
  `{config_dir}/?/init.lua`, fixed at VM build. The `io` file API stays
  available but **jailed** (`io_jail`): `io.open`/`lines`/`input`/`output`
  reach only paths that resolve under the config directory or a
  `[hooks] io_roots` entry, and never `crap.toml`, `data/`, `backups/`,
  the log directory, the database file, or `/proc`/`/sys`/`/dev`.
  Widening the jail is a reviewed decision; letting a hook read the
  process's secrets again is a breaking security change. The complete
  surviving global set is pinned by
  `sandbox_globals_match_reviewed_allowlist`; extending it is a reviewed
  decision, re-adding a removed capability is a breaking security
  change.
- **Lua chunk names are config-relative on every load path.** Files loaded
  for `collections/`, `globals/`, `jobs/`, init.lua, and — via the installed
  `require` searcher — `hooks/` are named `<dir>/<file>.lua`, never the
  absolute `{config_dir}/…` path. Lua stamps the chunk name into `error()`
  text, which travels verbatim to API clients as a `HookError`, so an
  absolute name would disclose the deployment's filesystem layout. Pinned by
  `chunk_names_are_relative_not_absolute`,
  `lua_error_text_carries_relative_chunk_name`, and
  `required_module_error_carries_relative_chunk_name`.
- **The 9 `HookEvent`s** and their per-operation firing order:
  field `before_validate` → richtext-attr `before_validate` → collection
  `before_validate` → validate → field `before_change` → collection
  `before_change` → persist → field `after_change` → collection `after_change` →
  registered `after_change`; reads run `before_read` → strip → `after_read`;
  deletes `before_delete` → `after_delete`. `before_render` is global-only.
- **Which events get CRUD access** (before/after-change, before/after-delete,
  field before-validate/before-change/after-change, richtext-attr
  before-validate — the field hook context, failing closed like a field hook)
  vs which do not (`before_read`,
  `after_read`, `before_broadcast`, validators, conditions). `before_render` and
  the `password_login` method's `mfa_when` gate are the **read-only** tier — see
  below.
- **A present-null field survives the hook context round-trip.** Lua collapses
  JSON null to `nil` and drops the key, so a field explicitly set to null (a
  clear-to-null request) that a hook does not replace is re-inserted as null when
  `ctx.data` is rebuilt — the clear is never silently downgraded to "no change",
  matching the field-hook `was_present` rule.
- **Null reaches Lua as `nil` on every surface.** Every context, argument and
  result table the runtime builds for Lua maps JSON null, unit and an absent
  optional value to `nil` (one serializer, `lua_api::to_lua_value`), never to
  a truthy sentinel value — `ctx.data.x == nil` holds for a null field in hook,
  field-hook, access, validation, live-filter, route, job, strategy and MFA
  contexts alike. The exception is a null **array element**, which is the
  `crap.null` sentinel (mlua's null light-userdata) so arrays never have
  holes; `lua_api::to_lua_value` and `json_to_lua` share this rule. mlua's own
  serializer (`LuaSerdeExt::to_value` / `to_value_with`) is banned by
  `clippy.toml` (`disallowed-methods`), so no context can bypass it.
- **`crap.null` is JSON null on every Lua→Rust path** (`lua_to_json`, and
  the serde deserializer behind `lua.from_value`), so it is the one way Lua
  writes an explicit null (clear a field, keep a present-null key).
- **An access constraint from a rule that read a NULL `ctx.user` field is
  denied.** The access evaluator records reads of NULL-valued user fields
  (an `__index` metamethod; `rawget` is untracked) and fails a returned
  filter table closed; boolean verdicts are unaffected.
- **Hook-return semantics.** Only `data` and `context` are read back; `data`
  **replaces** `ctx.data` wholesale. A normal hook returning `false` is ignored
  (only `error()` aborts); `before_broadcast`/live-filter returning `false`/`nil`
  suppresses, a table is a hook error and any other type suppresses with a
  warning (fail-closed, like every boolean gate). These asymmetric meanings
  are locked.
- **`ctx.operation` value set**: `create` / `update` / `undelete` / `delete` /
  `find` / `find_by_id` / `get` / `init`, plus `unpublish` / `restore` on an
  `after_read` or `before_broadcast` shaping a live event (hook context; the
  one list is `hooks::lifecycle::operation`, which the typed contexts are
  generated from); access functions also see
  `trash` / `undelete` / `unpublish` / `restore` / `count` / `search` / `read` /
  `subscribe` / `trigger`. Hook/field-hook context key names are frozen.

### `before_render`

- **The signature is `fn(ctx, info)`.** `ctx` is the page context; `info` is the
  page identity (`page`, `template`, `collection?`, `global?`). `info.page` is
  the same discriminant as `ctx.page.type`, and both come from the generated
  [template-context reference](../admin-ui/reference/template-context.md) — a
  page's `page.type` value and template name are part of that frozen table.
- **One shared table.** Every registered hook is handed the same Lua table; the
  context is converted from and back to JSON exactly once per render regardless
  of hook count. A hook that returns a table replaces the context for the hooks
  after it; `nil` keeps the current one.
- **The access level is shared with `crap.template_data`.** Both render-time
  extension points on a page run under one [`RenderCrud`] — same identity,
  same database access — so neither can drift into being the privileged one.
- **Read-only on authenticated pages; no database at all on unauthenticated
  and error pages.** Reads run as the signed-in admin with normal access
  control. Writes and `crap.transaction(fn)` are refused — a page render must
  not take the write path (it would serialize admin page loads against writes)
  and must not be able to mutate. The auth/error carve-out is a security
  boundary: with no viewer there is no identity to scope a read by, and error
  pages have to render when the database is what failed.
- **Failure is always fail-soft.** A hook error, a non-table return, or a
  conversion failure logs a warning and renders the page with the context as it
  stood. A render is never failed by a hook.

[`RenderCrud`]: https://docs.rs/crap-cms/latest/crap_cms/hooks/lifecycle/enum.RenderCrud.html

- **`after_read` has no CRUD, enforced.** A `crap.*` CRUD call from an
  `after_read` hook raises on every surface (Lua-driven reads included) —
  the hook runs after the read is final and fails open, so a write from it
  could half-apply. `before_read` + `ctx.context` is the sanctioned path.
- **A locale-locked (shared) field in a non-default-locale write is
  rejected at persist time too**, for collections and globals alike — so a
  `before_change` hook that injects one fails the write loudly instead of
  the field being silently skipped at the DB edge. The admin form strips
  its read-only shared fields before the service sees them (collections and
  globals), so a translation save is never rejected for them.
- **Bare pool-mode CRUD has the full transaction scope.** A single
  `crap.collections.x.create(...)` in a job, route, or effect runs in its
  own transaction with the same `crap.tx` queue, event gating, file
  cleanup, and cache invalidation as `crap.transaction(fn)` and the service
  envelope — one implementation (`run_scoped_tx`) behind all three.
- **Hook errors carry no Lua traceback to clients.** The message before
  `stack traceback:` is what a client, admin toast, or MCP response sees;
  the traceback goes to the server log.

- **A version snapshot records every locale's column.** Snapshots carry the
  decorated `field__xx` columns for every configured locale, not just the
  value the writing locale resolved; restore prefers those columns and falls
  back to the bare key only for snapshots written before this was true. A
  draft is read back for the locale being read.
- **`_status` is written only where it exists.** Restore and unpublish stamp
  it only for a drafts-enabled collection — an audit-trail collection
  (`versions = { drafts = false }`) has no such column, and unpublish is
  refused there. The status write bumps `updated_at` only on a table that has
  timestamps.
- **A write reports the `_status` its row ends with.** The one status write of
  a create/publish/unpublish stamps the reported document too, so the caller,
  `after_change` and the live event (whose view is chosen from `_status`) never
  see the pre-write status.
- **A write reports the document in the shape a read returns it.** Every write
  op — create, update, bulk update, restore-version, undelete, unpublish,
  global update — passes the document it reports through the same three steps
  before it reaches the caller, the `after_change` context and the emitted
  event: `hydrate_reported` (read the join-table rows back, for the flat
  re-read a write query returns — skipped where the document already carries
  them, so the join tables are never read twice), `shape_reported` (fold an
  upload's per-size values into `sizes`), and `strip_reported` (shape, then
  strip read-denied and hidden fields, judged in the write's locale, falling
  back to the default locale). A surface must never report a raw write result:
  a write's response and a read of the same document have to agree, field for
  field, or a client's types split in two. A live event is never built
  from that report: it carries the stored row, shaped by `shape_reported`
  alone, and stripping is left to per-subscriber event delivery.
  - **A user document on an auth response is a normal `Document`.** `Login`,
    `VerifyMfa` and `Me` return it hydrated, field-read-stripped and
    API-hidden-stripped, like every other document on the wire.
  - **A draft save reports the draft.** The return value, the `after_change`
    context and the emitted event all carry the stored draft snapshot stamped
    `_status = "draft"`; the published row is untouched. A published-only
    subscriber therefore sees nothing for a draft save.
- **The read strip runs read-access first, hidden second.** `strip_unreadable`
  (and its batch and event-payload twins) applies the data-aware field-read
  access strip *before* removing `api_hidden` fields, so an access rule that
  branches on the document judges the **whole** document — the order is the
  contract, not an implementation detail. Stripping hidden fields first would
  hand the rule a document with holes in it, and the same rule would then
  decide differently on a read than on a write report.
- **Changing an email address clears the verified flag** on a collection that
  requires verification.
- **A version read and the stored version row are separate lookups.**
  `find_stored_version` returns the snapshot **as stored** — every access gate
  of a version read already enforced (`access.versions`, then the
  read/draft composite), but no read shaping — and is what a restore writes
  back. `read_version_snapshot` rewrites that snapshot into the document a read
  returns: shaped for the caller's locale context (so per-locale `field__xx`
  keys never reach a caller, and `locale = "all"` yields the per-locale map),
  then stripped like any read document, with the snapshot itself as the
  `ctx.document` an access rule judges. A caller that needs both reads the row
  once and shapes it, rather than selecting it twice; a caller that needs only
  the read shape must never hand out the stored form.
- **The draft overlay obeys the lifecycle.** A soft-deleted document's draft
  snapshot is never returned by a live read.
- **The newest published version survives pruning**, whatever `max_versions`
  says — it is what an unpublished document serves.
- **A password change clears any pending reset token**, in the same statement
  that writes the hash.

## Read-surface invariants

- **Pagination limit and populate depth are clamped at every read surface**
  (Lua / gRPC / MCP / admin) via `apply_pagination_limits` (cap `max_limit`) and
  `min(max_depth)`. Any new read surface **must** apply the same clamps — an
  untrusted limit/depth must never reach the query layer unclamped. The main
  find path and the offset-only gRPC `ListJobRuns` both floor the limit at **1**
  (never `LIMIT 0`, never an unbounded `LIMIT -1`): the find path via
  `PaginationCtx::validate`, `ListJobRuns` via `PaginationCtx::resolve_limit`.
  **Version listing** floors its limit *and* offset inside the shared
  `service::versions::list_versions` (via `floor_optional_limit`, which lives in
  `db::query` so every surface and the service share one helper), so the Lua,
  gRPC, and MCP version listings all inherit the floor at one point.
- **Keyset pagination returns every row, NULL sort values included.** A page
  over a nullable sort column includes the NULL-valued rows (they sort to the
  tail under the explicit `NULLS LAST` / head under `NULLS FIRST`); the keyset
  predicate adds `col IS NULL` on the directions that advance toward them, so no
  row is ever silently skipped across pages (three-valued `col < ?` would drop
  them). `count` and the paged set agree.
- **Cache invalidation happens after commit on every surface.** A write clears
  the populate cache only once its transaction is durable — never before (a
  pre-commit clear lets a concurrent read repopulate a stale entry). Pool-mode
  clears post-commit in `run_pool_write`; conn-mode (Lua job / custom route /
  `crap.transaction`) defers via a transaction-scoped `cache_dirty` flag that
  the commit-owning envelope flushes, the same deferral events and upload-file
  cleanup use.
- **An unknown locale string errors on every surface — it is never silently
  dropped.** `LocaleContext::from_locale_string` rejects a locale outside the
  configured set, and every intake (Lua / gRPC / MCP / admin forms + validate
  endpoints, via the admin `parse_request_locale` helper) surfaces that as a
  request error. Swallowing it into a `None` context is forbidden: a `None`
  context on a localized collection reads/writes the bare columns (`title`
  instead of `title__en`) — the classic wrong-column footgun.
- **An all-locales read strips a localized field's read access per locale.**
  When `locale = "all"` returns a localized leaf as a `{ locale: value }` map,
  the field-read strip evaluates that field's `access.read` once per locale key
  (with `context.locale` set to the key) and removes only the denied locales'
  entries, dropping the field only when none survive. A single default-locale
  decision must never keep or discard the whole map — a rule that exposes a
  field in one locale and hides it in another is honored per value.
- **Live-mutation streams resolve access through one shared path.** The gRPC
  `Subscribe` and admin SSE streams build their per-subscriber view/mode maps via
  `EventAccessMap::resolve` and enforce them per event via `EventGate::evaluate`
  — one construction point and one enforcement point. Both are fail-closed (an
  access hook that errors or a global returning a row-filter drops the view) and
  a new stream surface must reuse both, never re-derive the access mapping.
- **Live row constraints are judged against the event's gating snapshot, which
  is never delivered.** Every published mutation event carries the stored row
  it concerns (for a delete, the row as removed; for a soft delete, as trashed)
  in an opaque `gate` snapshot, and `EventGate::evaluate` judges a subscriber's
  row constraint against it — never against the delivered `data`, which
  `live_mode` may empty and `before_broadcast` may reshape. The snapshot exists
  only for that check: the SSE envelope and the gRPC `MutationEvent` are built
  from an exhaustive destructure of the event that discards it, the type has no
  accessor, and its `Debug` is redacted. An event without a snapshot is dropped
  for a constrained view (fail-closed). On the Redis event channel the snapshot
  travels as an additional `gate` key of the JSON event (absent when there is
  none; a node that predates it ignores it, and its own events — lacking it —
  reach only unconstrained subscribers). The transport bounds `data` and the
  snapshot independently, each on its own JSON size (512 KiB each): an
  over-cap `data` is dropped (the event then reads as a metadata event), and an
  over-cap snapshot is dropped (the event then reaches only unconstrained
  subscribers). Neither part's size may cost the other its place on the wire.
- **The in-memory constraint evaluator judges a document's whole row.** A
  `Document` keeps `id` and the timestamps outside its field map; every caller
  that judges one — populated relationship targets, draft snapshots, event
  gating snapshots — builds the evaluator's input through `matches_document` /
  `constraint_row`, so a constraint naming `id` or a timestamp is judged as
  the SQL `WHERE` judges it. Passing `doc.fields` alone is forbidden.
- **A `full`-mode payload is stripped per subscriber from the stored row,
  never for the writer.** A write publishes the row it stored, read-shaped and
  stripped for no one, as the event's `data`: the `live` filter and
  `before_broadcast` hooks see it in both modes, the transport carries it only
  in `full` mode (a `metadata` event drops it after those hooks), and
  `EventGate::evaluate` strips each subscriber's copy by that subscriber's own
  field-read access, then of hidden fields, then runs `after_read` — the
  subscriber's read pipeline. On the Redis channel `data` therefore holds
  hidden and read-denied values (server-to-server, like the snapshot); every
  delivery encoder must go through `EventGate::evaluate`, never forward
  `data`. Stripping for the writer before publishing is forbidden: it would
  make what one subscriber receives depend on who made the change.
- **A delete event is gated by the view its row was last in, and every
  permanent delete publishes one.** The view and snapshot come from the
  removed row, read on the delete's connection (`read_delete_event`): a
  trashed row — soft-deleted now, or purged from the trash, or force-deleted
  while trashed — is gated by `trash`, any other by its status view. The
  retention purge, the CLI trash purge and "Empty trash" publish through the
  same event, after their transaction commits.
- **A live stream fails closed on a lost revocation signal.** Both the gRPC
  `Subscribe` and admin SSE pumps drop the subscriber when the fixed-capacity
  user-invalidation broadcast reports `Lagged` or `Closed` — an overflow may have
  dropped this session's own revocation, so the stream cannot be proven still
  valid and is torn down to force a re-authenticating reconnect. Staying
  connected on a lost signal is forbidden. The Redis invalidation transport
  upholds this at the pump too: if a subscriber's local queue is full and even
  the `Lagged` sentinel cannot be enqueued, the pump terminates so the receiver
  observes `Closed` (fail-closed) rather than silently dropping the message; the
  event transport, whose loss is non-contractual, stays best-effort.
- **Server-derived upload columns are never user-writable.** `url`, every
  `{size}[_fmt]_url`, `filename`, `mime_type`, `filesize`, `width`, and `height`
  (`CollectionUpload::derived_field_names`) are computed by the upload pipeline
  from the processed file. The write chokepoint (`create_/update_document_in_conn`)
  strips them from untrusted input on every surface; only the file-processing
  upload handlers, which set `trusted_upload_metadata`, may write them. The serve
  access gate and `delete_upload_files` trust these columns as truthful
  back-pointers, so a user-forged value would read or delete another document's
  file. `focal_x`/`focal_y` stay user-editable (a setting, not file-derived).
  The nest-and-strip is one `canonicalize_write_input` step that EVERY persisting
  write path — single create, single update, and bulk (`update_many`) — runs up
  front; the `write_paths_canonicalize_before_persist` guard test fails the build
  if a `persist_*` caller skips it, so a new write path can't reintroduce the
  forgery gap. It runs inside the write's admission prefix
  (`service::write::admission`: canonicalize, adopt the pending draft a publish
  makes live, locale lock), which the `validate` dry-run shares with the write it
  previews — the dry-run strips the same columns (the admin's multipart preview
  is the one trusted caller).
- **Version restore validates at the write path's strictness.** Restore builds
  its `ValidationCtx` draft-aware, locale-scoped, and `required_locales`-aware
  exactly like create/update, and refuses a soft-deleted (trashed) target with
  `NotFound`. A published restore must satisfy the localized-completeness gate; a
  draft restore is exempt; a trashed row is never silently rewritten.
- **An update preserves every stored column the caller did not supply.** This
  holds for scalars (the `UPDATE` names only present columns) and now for
  array/blocks rows too: the writer diffs incoming rows against the stored set by
  round-tripped junction-row `id`, and a matched row's UPDATE touches only the
  sub-fields the row supplies (arrays) or merges its top-level fields over the
  stored `data` (blocks). A sub-field the write-access strip removed, or the
  caller simply omitted, keeps its stored value; a present field (including an
  explicit null) overwrites. A row with no `id`, or an id that is not an existing
  row of that parent(+locale), is a new row with a server-minted id — a client
  cannot choose a primary key or address another parent's/locale's row. Rows
  absent from the incoming set are deleted; `_order` follows the incoming
  position. A surface that does not round-trip the id degrades to a full replace,
  never worse.

- **A field the caller cannot read is never a query oracle.** `hidden`
  fields are never filterable, sortable, or searchable; a field with an
  `access.read` rule is filterable/sortable only when the rule allows the
  caller without row data — judged for every field on the path (group,
  array-row, block and nested-row sub-fields and their containers; a block
  path in every block type holding the field) — and is out of the default
  search index (listing it in `list_searchable_fields` is an explicit
  opt-in). Enforced at the service find/count/search chokepoint, and for
  the `where` filter of `update_many` / `delete_many` at the bulk scope
  chokepoint (their counts answer the same question); `override_access`
  is exempt. The same rule decides which referring fields the
  back-reference report may list.
- **A join lists only children whose `on` value the reader may read.** A
  child whose `on` field the read strip removed is left out of a populated
  join and of the admin join items and count — judged per child, on its
  own data.
- **The full-text index is row-backed and write-path complete.** The
  per-document sync reads the indexed columns from the row itself (both
  backends index exactly `get_fts_columns`), so it cannot depend on the
  shape of an in-memory document; every write path (create, update, bulk
  update, undelete, restore, import, CLI) keeps it current. Soft-deleted
  rows keep their index entry (trash view search); only a hard delete
  drops it.
- **A filter value that does not fit the column type is a validation
  error** (400 naming the field) on every surface — never a silent text
  comparison — and a keyset cursor whose sort value cannot bind to the
  sort column's type is rejected the same way.
- **A relationship / upload value is an id (or a list of ids).** A number,
  boolean, populated document object, or list with non-string items is a
  validation error on write; a polymorphic target must be `collection/id`.
  A reference to an id that does not exist is a caller error (400 naming
  the target), not an internal fault.
- **A self-reference never counts.** A document referencing itself adds
  nothing to its own `_ref_count` (on create, update, hard delete, and the
  backfill alike), so it stays deletable — matching the back-reference
  list, which already omits the owner. The startup ref-count backfill
  skips (and logs) a stored reference whose target no longer exists
  instead of refusing to start.

- **The auth secret is resolved once, at config load.** An empty `[auth]
  secret` is replaced by the generated, persisted `data/.jwt_secret` before
  any consumer sees it, so JWT signing, `crap.crypto`, TOTP sealing and
  signed upload URLs all key off the same value. Deriving a key from an empty
  secret is refused, never silently done. The secret is generated under an
  exclusive lock on `data/.jwt_secret.lock` and written through a staged file
  renamed into place; an existing secret file that can't be read is an error,
  and only an empty one is ever replaced.
- **A slug names either a collection or a global, never both.** Re-defining
  the same kind stays legal (the plugin extension pattern).
- **`null` means "clear" on every write surface**, MCP included; non-finite
  numbers are rejected rather than coerced to null.

## Server-config posture (frozen defaults)

- **gRPC per-IP rate limiting is off by default** (`grpc_rate_limit_requests =
  0`) and keys on the raw TCP peer (no `X-Forwarded-For`). Live deployments set
  it explicitly; behind an L7 proxy set it at the proxy. Changing the default to
  non-zero would collapse all clients behind a proxy into one bucket.
- **Schema introspection is public by default** (`public_schema_introspection =
  true`): `ListCollections` / `DescribeCollection` need no auth. Operators set it
  `false` to require authentication. It never gates document data.
- **Static protective headers apply to every response.** `X-Frame-Options`,
  `X-Content-Type-Options: nosniff`, `Referrer-Policy`, `Permissions-Policy`, and
  (outside dev mode) HSTS are stamped on the full router — built-in admin routes
  **and** merged custom routes. Only the nonce-bound admin CSP is admin-only
  (custom routes render their own bodies and carry no nonce).

## Custom routes & admin responses

- **CSRF is enforced only on mutating methods** (POST/PUT/PATCH/DELETE).
  Declaring `csrf = true` on a custom route that answers only safe methods
  (GET/HEAD/OPTIONS) is rejected at load — a safe-method handler must not mutate
  state, and a route that mutates must declare a mutating method.
- **The external auth callbacks are the only built-in routes exempt from the
  double-submit CSRF check** (`/admin/auth/callback/{name}` and
  `/admin/auth/callback/{collection}/{name}`, matched by route template): an
  identity provider's `form_post` answer is a cross-site POST without the
  `SameSite=Strict` token cookie. Their login-CSRF defense is the OAuth `state`
  the hook verifies. The hook's `ctx.headers` carries `_query_{param}`,
  `_form_{field}` (urlencoded body) and `_method`; a request header spelled
  like one of those reserved keys is dropped.
- **Admin list sort eligibility requires a real column.** A field is sortable
  only if it has a parent column (`has_parent_column()`); a has-many
  relationship/upload (no column) is rejected at the 400 param gate, never passed
  to the query layer.
- **Admin JSON/lazy-load endpoints return real HTTP status codes through shared
  helpers.** Version restore returns 403 on denial (not a silent redirect);
  back-references, the delete dialog, and empty-trash go through
  `json_not_found` / `json_forbidden` / `json_conflict` / `json_bad_request` /
  `json_server_error` (and `require_collection_json`), so a given error is one
  status code and one `{"error": …}` envelope across sibling endpoints — 404
  (unknown), 403 (denied), 409 (a referenced document blocking delete), 400
  (bad input), 500 (failure). Two deliberate exceptions: the relationship-search
  autocomplete always answers `200` with a JSON array (a missing collection is
  an empty list; a real DB failure is logged, not surfaced), and
  evaluate-conditions returns its `field → bool` map — `{}` on the error path,
  because its JS consumer iterates the body as that map, so an `{"error": …}`
  envelope would inject a bogus field name.
- **Response status is mapped from the typed `ServiceError`, never re-derived by
  matching the error's `Display` string.** The gRPC surface owns the canonical
  `ServiceError → tonic::Status` mapping; the HTTP upload-delete surface mirrors
  it with an explicit variant match. A backend-string probe survives only where a
  condition has no typed variant (e.g. MCP `read_global` detecting an
  uninitialized global's missing table), and there it is scoped to
  `ServiceError::Internal` so typed errors always propagate.
- **Invalid-locale policy is intentionally surface-dependent.** Machine APIs
  (gRPC / MCP / Lua) propagate an unparseable `locale` as an error. The admin
  *rendering* helper (`editor_read_ctx`) logs a warning and falls back to the
  **default-locale** read context, so a hand-edited `?locale=` query param
  degrades the edit page to the default view rather than 500-ing it — and never
  leaves a localized read with no context, which would select columns that
  don't exist. The admin picker only ever emits configured locales, so this
  fallback is reachable only off the happy path.
- **One locale-picker shape in the admin template context.** A page exposes the
  editor locale as exactly `has_editor_locales` / `editor_locale` /
  `editor_locales`, written by `BasePageContext::with_editor_locale` from the
  same locale `editor_read_ctx` resolves the page's read context from. There is
  no second, parallel key set, so an override template cannot bind to a copy
  that drifts from the locale the page reads and writes in.

## Access model

- **The access-key set** (`read`, `create`, `update`, `delete`, `trash`,
  `draft`, `versions`, `unlock`, `admin`, `mcp`) and the **fallback chains**
  (`draft ?? update`, `trash ?? update`, `versions ?? update`). Reads are a
  union of allowed views that downgrades rather than erroring. Changing a
  fallback target silently re-permissions every config that omits that key.
- **Constraint tables use the canonical `where` grammar.** A `Constrained`
  access result decodes through the same `decode_where_map` as every CRUD
  filter — scalar shorthand, operator tables, and `["or"]` groups included —
  with the leaf allowlist (equality/membership on flat own columns) applied
  recursively. Every failure is a fail-closed deny: an empty constraint
  table, a decode error, an empty `or` group, a disallowed operator.
- **`access.admin` gates admin-UI visibility uniformly.** Both the sidebar nav
  and the dashboard cards hide a collection/global the user can't `admin`, and
  the rule is evaluated under operation `"admin"` — the same value the route
  middleware passes — so a hook branching on `ctx.operation` behaves identically
  in the UI filter and the real gate.

- **Field `access.update` rules judge the stored document.** `ctx.document` is
  the stored row on update (the incoming document on create); `ctx.data` is
  always the incoming level. The value under judgment is never the evidence.
  A draft save judges the *published* row — a pending draft's values never
  grant field write rights before publish — and a version restore judges the
  live row, not the snapshot.
- **`ctx.user` is the stored user document, whatever the auth method.** A
  bearer token, a session cookie, a custom strategy and a claims reload all
  read the user through one reader (the default-locale stored row: every
  field present, NULL as `null`, hidden fields included); a strategy's
  returned table only names the user by `id`, and an id naming no stored,
  non-trashed user is refused.

## Auth tokens

- **Password policy is enforced at the service write chokepoint.** A `password`
  supplied to a `create` or `update` on an auth collection is validated against
  `[auth.password_policy]` inside the service create/update path, so every
  surface (Lua / gRPC / MCP / admin) and both single and bulk `create` are
  covered by one check that no surface can bypass. A write context that does not
  thread the configured policy falls back to `PasswordPolicy::default()` — never
  to no enforcement. `create_many` accepts a per-item policed password (distinct
  per document); `update_many` **rejects** a password, because it applies one
  value to many rows and must not broadcast a single credential. A violation
  surfaces as a structured `password` field validation error, rendered uniformly
  by every surface. An **absent** password is always legal (an external auth
  method may own the credential); a **present but empty** one means "leave the
  stored password alone" on update and is an error on create, where there is
  nothing to leave alone. No surface applies the policy before its access
  check, so a rejection can never be read as a policy oracle by a caller who
  isn't allowed to write.
- **Email is matched case-insensitively everywhere it is an identity.** Account
  lookup (`find_by_email` = `LOWER(email) = LOWER(?)`), the per-account login and
  forgot-password rate-limit keys, and uniqueness on an `Email`-typed field all
  compare case-insensitively — one address is one account with one lockout bucket
  regardless of casing. Non-`Email` unique fields (slugs, codes) stay
  case-sensitive. Two guarantees keep this closed: an auth collection's `email`
  field **must** be `type = "email"` and `unique = true` (load error otherwise,
  so the type-scoped case-insensitive check always applies to the identity
  field), and every auth collection carries a `UNIQUE INDEX ON (LOWER(email))`
  backstop (partial — active rows only — under soft delete) so even a race past
  validation can't create case-variant duplicate accounts.
- **The `token_use` claim** partitions signed tokens into `session` (accepted by
  every authenticated surface — admin cookie/bearer, gRPC, upload serve) and
  `mfa_pending` (accepted only by the MFA-completion endpoint). Session
  validation rejects a non-`session` token, so an MFA-pending token can never
  authenticate a request. A token minted before the claim existed decodes as
  `session`. Never accept `mfa_pending` as a session, or MFA becomes bypassable.
  The in-memory claims of a custom-strategy-authenticated request carry a
  third value, `strategy`: the token provider refuses to sign them, and any
  handler minting a session from a request's claims (session refresh) accepts
  only a request the session cookie authenticated. A strategy credential is
  never exchangeable for a signed token.
- **Every session-minting authentication passes the MFA gate**
  (`service::auth::mfa_gate`): the password login, a custom-strategy login and
  the admin auth callbacks alike. On a collection with an `mfa` mode the
  surface issues the MFA-pending step, never the session; the only skip is the
  `mfa_when` verdict or, for a callback, its name in the `password_login`
  method's `mfa_exempt_callbacks`. A new way to mint a session must go through
  the same gate.
- **A session records its surface and its second factor.** Every session token
  is minted by `service::auth::mint_session` and carries `surface` (the minting
  surface) and `mfa` (the second factor was passed — the MFA step, or an
  MFA-exempt callback). A session without `mfa` never authenticates a request
  whose MFA gate (the collection's `mfa` mode and `mfa_when`, judged for that
  request's surface and headers) requires the second factor; the evaluator
  answers `mfa_required`. A token minted before the claims existed decodes as
  `mfa = false` — fail closed. Every MFA challenge is issued by
  `service::auth::issue_mfa_challenge`, and its pending token is consumed only
  by the surface that minted it (an unstamped pending token by none).
- **A session's lifetime is the collection's `token_expiry`, else the global
  `[auth] token_expiry`.** The two are resolved in one place
  (`Auth::token_lifetime`, called by `mint_session`); a collection that sets no
  `token_expiry` has none of its own — it is never filled in with a built-in
  number. Both must be positive.
- **Custom strategies judged per request run on the request's connection;
  the password and strategy login run on the login's write connection; an
  auth-callback hook and an `mfa_deliver` hook take a write connection only
  at their first CRUD call.** A per-request strategy may write
  (commit-on-success, like every strategy), but resolution never takes a
  write-pool connection — it runs for every request the strategy
  authenticates, and a write-pool checkout there would queue reads behind
  writes. A callback (commit when it returns a user) and `mfa_deliver`
  (commit when it returns, rollback when it raises) hold one lazily opened
  write transaction, so their outbound HTTP before any CRUD pins no
  connection; the MFA code is stored before `mfa_deliver` runs.
- **`mfa_when` is read-only.** Its reads work; every `crap.*` write
  (including `crap.transaction(fn)`) raises an error naming the gate, which
  fails closed — at login and on each request an MFA-unstamped session
  authenticates alike.
- **A stored MFA code gets exactly one verdict.** Every attempt consumes the
  code (right or wrong), atomically — a conditional update on the value the
  attempt read — so of concurrent attempts exactly one is judged.
- **Trashing an auth user bumps its session version**, like a lock: a restore
  never brings back a token issued before the trash. Any logout bumps it too.
- **Single-use security tokens are minted at one chokepoint.** Password-reset
  and email-verification tokens both come from `generate_security_token()` — a
  32-character nanoid. Any new single-use-token flow uses the same helper so the
  entropy length can't drift between flows. (Tokens are opaque; the length may
  only grow, never shrink.)
- **Nothing spendable is stored in the clear.** Reset and verification tokens
  are written as their SHA-256 digest (lowercase hex). An MFA code is written
  as an HMAC-SHA256 keyed with `[auth] secret` — six digits is a 10^6 preimage
  space, so a bare digest of one would still be the credential. Every lookup
  hashes what the caller presented and compares in constant time. The rendered
  mail carrying the value is dropped from `_crap_jobs` once the send completes,
  so the link does not outlive delivery there either. A new single-use-secret
  flow hashes at the same DB edge, keyed if its value is guessable.
- **A verification link is single-live.** Issuing one — at sign-up or on a
  resend — overwrites any outstanding token for that account, so an older link
  in an older inbox is dead. Lifetime is 24 hours for both paths.
- **Token-issuing endpoints never confirm an account.** Forgot-password and
  resend-verification answer identically for an address that exists, one that
  is already verified, one that is locked, and one that was never registered
  — on every surface, and on a rate-limit block too. Each keeps its OWN
  rate-limit keyspace, keyed on the normalized (trimmed, lowercased) address:
  budgets are sized alike but never shared, so a burst on one endpoint cannot
  lock a caller out of the other. Same rule as verify-email and
  reset-password.

- **No expiry leeway.** A JWT is invalid at `now >= exp` on every surface, like
  every other expiring credential; a session refresh never issues an `exp` past
  `auth.session_absolute_max_age`.

## Scheduler & jobs

- **Per-slug / per-queue concurrency caps are exact per tick, cluster-wide.**
  When any cap is configured, the job claim serializes its count+claim decision
  with a transaction-scoped advisory lock so two Postgres nodes can't each claim
  past the cap on a `READ COMMITTED` snapshot that misses the other's in-flight
  claims. SQLite (IMMEDIATE) and single-node need no lock; an unconstrained
  deployment skips it and claims in parallel.

- **`JobStatus` value set** `{pending, running, completed, failed, stale}`
  (lowercase) — stored in `_crap_jobs.status`, matched in SQL, and surfaced to
  clients. Adding a value is a forward-compat break for older cluster nodes
  (which parse an unknown status back to `pending`); renaming breaks stored rows.
- **`_crap_jobs` columns** are append-only-text: `data`/`result`/`error` are
  free TEXT, read positionally. `scheduled_by` is a closed provenance set
  (`core::ScheduledBy`): `grpc`, `cron`, `hook`, `mcp`, `cli`, `system` — every
  insert names a variant, never a free string, and each maps to its own
  `JobScheduledBy` value. Clients may match on these names; adding one is a new
  enum value on every surface. Reads tolerate the legacy `api` (every queued
  bulk run before bulk runs recorded their real surface), which reads back as
  `grpc`; any other unknown stored value reads back unchanged on Lua/MCP/CLI and
  as `UNSPECIFIED` on gRPC.
- **`_crap_cron_fired` dedup key** = bare `slug`, window encoded in the `fired_at`
  value; system pseudo-crons use `__`-prefixed slugs (`__retention_purge`), which
  is why user job slugs cannot start with `_`.
- **Delivery guarantee = at-least-once.** A job that times out, or whose worker
  crashes (heartbeat expires past `heartbeat_interval × 3`), is **requeued** and
  re-runs; an exhausted one goes terminal `stale`. **Handlers must be
  idempotent.** `max_attempts = retries + 1`.
- **A job's `timeout` is enforced, and a run never overlaps its own retry.** A
  Lua handler stops at its deadline (VM hook + every database / HTTP / email
  entry point), the operation in flight rolls back (writes committed before the
  deadline stay), and the run is requeued only once it has returned — until
  then its row stays `running` with a fresh heartbeat. `timeout >= 1`. The
  `crap.tx` effects of an already-resolved transaction still run past the
  deadline.
- **`skip_if_running` counts `pending` and `running` runs** of the slug.
- **A heartbeat is written for (id, attempt)** — compare-and-set like every
  other job-row write.
- **Retry backoff curve** `min(2^(attempt-1) × 5, 300)` seconds — 5,10,20,…,300 —
  hardcoded, no config knob.
- **Cron** is UTC-only and catches up at most once after downtime — a schedule that came due while the process was down fires once on the next check, anchored on its stored last fire, never once per missed slot (missed runs beyond that are
  dropped), and coalesces multiple missed occurrences to one fire. Accepts 5-field
  (seconds prepended) or 6/7-field (leading field = seconds) expressions; weekdays
  are crontab-numbered (`0`/`7` Sunday … `6` Saturday) and translated to the
  scheduler library's numbering in one place; every schedule is parsed at startup.
- **`[jobs]` / `[jobs.queues.<name>]` config keys** and the `Option<T>` tri-state
  (`None` = inherit default, `Some(0)` = operator-chosen unlimited/none) are
  frozen; `deny_unknown_fields` rejects typos. `auto_purge` defaults to 30 days;
  `auto_purge = false` disables it (an empty string is rejected, not a disable
  sentinel).

- **`heartbeat_interval` must match across scheduler nodes.** A running job is
  reclaimed once its heartbeat is older than 3× the *reclaiming* node's
  interval.
- **Schema sync is serialized and owns its indexes.** On Postgres it holds the
  advisory lock `"crapsync"` for the whole sync, and it drops any
  `idx_<collection>_*` index the starting node does not declare — a rollout that
  changes indexes must finish before an older node restarts.

## Project layout / CLI

- **Discovery directories** `collections/` `globals/` `jobs/` `hooks/` and the
  `init.lua` entrypoint; the `crap` Lua global.
- **CLI subcommand + flag names, positional-arg order, and exit codes.**
  Every error exits **1** — that universal mapping is the contract; the
  differentiated codes are `status --check` → **2** when the audit found
  warnings, `jobs healthcheck` → **2** on warning (recent failures, long-pending
  or never-run scheduled jobs) and **1** on unhealthy (a stale running job), and
  `update check` → **1** when an update exists. `2` therefore always means
  "warnings, not a failure", and `1` keeps its "action needed / failed"
  meaning across all three. (No other exit codes are reserved; scripts may
  treat any non-zero as failure.) `serve --only` accepts `admin`/`grpc`, with
  `api` kept as a backward-compatible alias of `grpc`. Machine output: `export`
  JSON envelope and `serve --json` (and `--json` is forwarded to the detached
  `serve`/`work` child).
- **Behavioral flag defaults** users' scripts/CI depend on: `logs --lines`
  defaults to 100, `fmt` with no path scans `templates/`, `jobs status --limit`
  / `images --limit` default 20, `jobs purge --older-than` 7d, `bench`
  iterations 10/5. Changing any silently changes what a script does.
- **Stdout/stderr split (machine contract).** Diagnostics — `cli::error` and
  `cli::warning` — go to **stderr**; all normal output (`success`/`info`/`hint`/
  `header`/`step`/`kv`/`kv_status`, tables, spinners) goes to **stdout**. A
  pipeline can `2>/dev/null` to drop diagnostics and parse stdout. `export`
  emits only JSON to stdout when piped.
- **Glyph vocabulary.** The status/output glyphs are frozen as Unicode/ASCII
  pairs: `✓`/`+` (success), `⚠`/`!` (warning), `✗`/`x` (error), `→`/`>` (info),
  `───`/`---` (bar), `?` (prompt). The ASCII fallback is a real contract — a
  script running under `CRAP_NO_UNICODE=1` sees the ASCII half.
- **Environment-variable accepted-value contract.** `CRAP_NO_UNICODE` and
  `CRAP_FORCE_UNICODE` enable on any **truthy** value — `1`/`true`/`yes`/`on`
  (case-insensitive, trimmed) — and are disabled otherwise. `CRAP_LOG_FORMAT`
  activates JSON on the exact value `json`. `_CRAP_DETACHED` is a **reserved**
  internal parent→child marker (presence-only; do not set it). `CRAP_CONFIG_DIR`
  mirrors `--config`.
- **Backup/export formats** are gated by a numeric `format_version` — the layout
  (`manifest.json` + `crap.db` + optional `uploads.tar.gz` + `jwt_secret` when
  the auth secret was generated, owner-only, recorded by the manifest's
  `includes_secret`; the export envelope) is frozen for a given version.
- **`import` round-trips a document without loss.** A re-import preserves the
  target's incoming-reference count (the upsert is column-preserving via
  `ON CONFLICT … DO UPDATE` on both backends — never a delete-and-reinsert that
  would zero unlisted system columns), carries `_status` for draft-enabled
  collections, writes a present-but-null field as an explicit clear versus an
  absent field left untouched, and indexes the written row for full-text search
  exactly as the service write path does. A field the export omits is preserved;
  a field it includes as `null` is cleared. An export carries every document,
  trashed ones included (`_deleted_at`), **every companion a field stores** — a
  timezone date's `<field>_tz` and a code field's `<field>_lang`, read from the
  one companion table rather than a per-suffix list, so a new companion is
  carried without touching export or import — and array/blocks row `id`s, and
  import writes them back under the same ids. A
  localized array, blocks or has-many field exports every locale's rows as
  `{ "<locale>": rows }`, the shape of a localized column. An account exported
  with `--include-credentials` carries a `_credentials` object keyed by stored
  credential column (`_password_hash`, `_locked`, `_session_version`,
  `_settings`, `_verified`, `_totp_secret`, `_totp_confirmed`,
  `_totp_last_step`); one-time tokens are never exported and import rejects any
  other key.
- **`import` is one transaction.** Every document is written before reference
  counts are settled, so a document may reference one later in the file.
  Credentials imported over an existing account move its `_session_version`
  past both the stored and the exported one. An account whose `_totp_secret`
  doesn't open with the target's auth secret is refused before anything is
  written.
- **`restore --include-uploads` fails the command when uploads do not restore.**
  A backup with no uploads archive is a successful skip; a `tar` extraction that
  fails (non-zero exit, or `tar` missing) fails the whole `restore` rather than
  printing success — the error notes the database was already restored.

## Template formatter (`crap-cms fmt`)

CI gates on `crap-cms fmt --check`, so the formatter's exact output is a frozen
contract: changing a rule reformats every committed `templates/**.hbs` and every
downstream config that vendored templates.

- **Idempotent and content-preserving.** `format(format(x)) == format(x)`, and
  formatting never adds, drops, or reorders content — text, mustache
  expressions, raw bodies (`<script>`/`<style>`/`<pre>`/`<textarea>` and
  `{{{{raw}}}}`), and comments come through unchanged (whitespace aside). Both
  are property-test invariants; the content one is the guard idempotency alone
  can't provide.
- **Verbatim regions.** Raw-content element bodies, `{{{{raw}}}}` raw blocks, and
  comments (`<!-- -->` / `{{!-- --}}`) are emitted byte-for-byte — the final
  blank-line-collapse and trailing-whitespace-strip passes explicitly skip them,
  because whitespace there is significant.
- **Inline whitespace is rendering-preserving.** A whitespace run inside inline
  content collapses to a single space (never widened, never dropped between
  tokens), and directly-adjacent inline elements (`<a>x</a><a>y</a>`) stay on one
  line so no line break is introduced that would render as a space. *Residual:*
  an adjacent inline pair whose combined length exceeds the 100-char line limit
  is still split; keep whitespace explicit where it must render.
- **Best-effort on unbalanced nesting, never an error.** Handlebars legitimately
  opens/closes HTML tags across `{{#if}}`/`{{else}}` branches, so the linear
  token stream is expected to be unbalanced; the printer clamps depth
  (`saturating_sub`) rather than validating balance. It never rejects a template
  for HTML nesting.
- **Frozen rule tables.** The `BOOLEAN_ATTRS`, `VOID_TAGS`, and
  `RAW_CONTENT_TAGS` sets, the 2-space indent, the 100-char line limit (measured
  in **characters**), attribute order preserved (never sorted), single-quote
  fallback only when the value has a literal `"` or a triple-stash `{{{ }}}`, and
  the single-final-newline / one-blank-line-max policy — all load-bearing once
  frozen. An empty input formats to a single newline.

## Pre-alpha.10 design freezes (2026-09-03)

Decided under the "cleanest solution, break now" rule; each is frozen from
alpha.10 on:

- **Locale-locked writes error.** A non-default-locale write containing a
  non-localized field is a validation error naming the field — never a
  silent skip.
- **`has_many` lives inside `relationship`.** The top-level flag next to a
  `relationship` table is a load error (legacy `relation_to` keeps its flat
  flag).
- **Event vocabulary is six operations.** `create`, `update`, `delete`,
  `undelete`, `unpublish`, `restore` — on the proto enum, SSE payloads, and
  the Lua live/broadcast contexts. Lifecycle mutations never masquerade as
  `update` — with one per-subscriber exception: a write that moves a
  document between content views (published / draft / trash — a publish,
  unpublish, status-changing restore, soft delete or undelete) reaches a
  subscriber that could see it in the view it left, but not in the one it
  moved into, as the removal that subscriber's view saw — `delete` for a
  collection document (no data); for a global, which only a move out of the
  published view removes, `update` carrying the empty global. Subscribers
  that can see the view it moved into get the event's own operation; one that
  could not see the view it left gets nothing. Coalescing never hides such a
  move: the surviving event carries the view the document was in before the
  burst. On the multi-node wire the event's view metadata carries the prior
  view as `prior` and keeps the older `left_published` flag (a move out of
  the published view) for nodes that predate `prior`; both are additive and
  omitted when the document did not move.
- **Auth strategies are transactional.** Commit on authenticate, rollback
  otherwise; failed attempts can never persist writes.
- **`select` is strict.** Unknown names error; valid = top-level field names
  + `id`/`created_at`/`updated_at`/`_status`.
- **`surfaces` is strict and `"all"` is the every-surface sentinel** (future
  surfaces included). Unknown names error.
- **No direct/public storage URLs.** Everything serves through `/uploads/…`;
  a bypass returns only as an explicit signed-URL design.
- **Search is a prefix filter.** The ranked FTS mode was removed as dead
  code; ranked search would return as an additive feature.

## Event timing & transaction-outcome effects (frozen 2026-09-03)

- **Mutation events are published only for committed writes.** Every write
  path queues events during its transaction and flushes them strictly after
  a successful commit — the pool-write envelope, job handlers (per-op
  transactions, flushed post-handler), and `crap.transaction(fn)`. A
  rolled-back write never emits an event.
- **`crap.tx.on_commit` / `crap.tx.on_rollback` contract.** Effects are hook
  refs plus JSON payloads. Registration is fail-closed (an unresolvable ref
  or unserializable payload fails the registering hook, and with it the
  transaction); execution is fail-open (an effect error is logged and
  skipped — the outcome is final). `on_commit` runs only after commit,
  `on_rollback` only after rollback; effects run *outside* the transaction
  in pool-mode with `ctx = { data, outcome }`. Registrations from hooks
  fired by nested CRUD attach to the outermost transaction.

## Queued bulk operations (frozen 2026-09-04)

- **`queue = true` response shape.** The count fields (`created`,
  `modified`, `deleted`, `soft_deleted`, `skipped`) are `0` and the document
  list is empty; `job_id` is present. Without `queue`, `job_id` is absent.
  The `result_json` summaries are frozen: `{"created":N}`,
  `{"modified":N}`, `{"deleted":N,"soft_deleted":N,"skipped":N}`.
- **Queued runs are queuer-scoped, override-wide.** A `_system_bulk` run
  is readable through `GetJobRun` by the identity that queued it, and by
  any override caller (which is how the MCP job tools read them — those
  return status/result/error only, never the payload). It never appears in
  `ListJobRuns`. Unparseable run data fails closed.
- **Identity is a reference, re-checked at execution.** Only the user id,
  auth collection, and session version are stored — never a user document —
  and the user is re-loaded when the run executes: a locked, deleted or
  trashed account, or a session-version bump (any logout — not only a
  forced one — password change or reset, lock, unverify, trash), abandons
  the run. Every authentication method can queue — a
  custom strategy's user is always a stored row of its collection, and its
  in-memory claims carry that row's session version. Anonymous callers
  cannot queue (`UNAUTHENTICATED`), and `CreateMany` with `queue` plus any
  per-item password is `INVALID_ARGUMENT`.
- **Exactly one attempt.** `_system_bulk` runs are pinned to
  `max_attempts = 1` at insert, independent of `[jobs.queues.bulk]
  retries` — a retry could re-apply an already-committed batch.
- **The budget is enforced, not just reported.** A run that exceeds its
  queue `timeout` commits nothing: the batch aborts itself and the atomic
  transaction rolls back, so the recorded failure is truthful.
- **`_system_bulk` is a reserved system slug** (`SYSTEM_JOB_SLUGS`). User
  job slugs cannot collide — `validate_slug` rejects a leading underscore.
- **Queue-time capture.** `bulk_max_documents`, `hooks`, `draft`, `events`,
  `locale`, and `force_hard_delete` are snapshotted when the run is queued;
  later config changes do not affect a pending run.
- **Refused before it is stored.** The collection access gate and the
  document cap run at queue time; a denial or an over-limit batch is a
  synchronous error, not a queued run that fails later.
- **`CancelJobRun` cancels a not-yet-claimed run**, authorized by the same
  rule that governs reading it. A claimed run cannot be cancelled.
- **A finished run does not retain its request body.** The stored payload
  is reduced to the queueing identity once the run reaches a terminal
  status.

## MCP job tools (frozen 2026-09-04)

- **`[mcp] job_tools` is three-state** — `false` | `"read"` | `"all"`;
  `true` is rejected as ambiguous. Tier membership is frozen: `"read"` =
  `list_jobs` + `get_job_run` + `list_job_runs`; `"all"` adds
  `trigger_job`. Every tier is enforced at execution, not only in
  `tools/list`, and the `queue` argument on the bulk tools is advertised
  and accepted only from `"read"` up.
- **System-job runs stay hidden.** `_system_email` and
  `_system_image_convert` runs are not readable through the job tools
  (their payloads carry delivery tokens); `_system_bulk` is the sole
  exception, under the queuer-scoped rule above.

## Explicitly NOT frozen (carve-outs recorded before the alpha.10 tag)

Named here so later fixes are improvements, not breaking changes:

- **Queued-bulk failure classification and check granularity.** Whether a
  crashed run surfaces as `failed` or `stale`, the grace the scheduler's
  outer timer allows a self-limiting job, and how often a batch checks its
  deadline (currently between documents and once before commit) are
  implementation details. The contract is only: an over-budget run commits
  nothing and is recorded as a failure.
- **Live-event delivery granularity under load.** Sequence numbers already
  make delivery best-effort (a lagging subscriber drops events and detects
  the gap). Since alpha.10 the stream pumps implement burst coalescing —
  each sweep collapses queued events latest-wins per document — and that
  granularity remains explicitly non-contractual: a subscriber is guaranteed
  an event carrying each changed document's *latest* state, never every
  intermediate event. Further batching (subscriber grouping, windowing)
  stays within this contract.
- **Upload URL storage IS frozen — which made signed URLs additive.** The
  value stored in a document's `url` / `{size}_url` columns is the
  `/uploads/…` proxy path, permanently. The signed-URL scheme (shipped in
  alpha.10) signs at read/serve time and never changes stored values. Its
  wire contract is frozen: `?exp=<unix-seconds>&sig=<hex>` query parameters
  on the serve path; `sig` = HMAC-SHA256 over
  `"crap-cms:upload-url:v1\n{path}\n{exp}"` keyed by `[auth] secret`
  (the `v1` context is the versioning seam for any future scheme); a valid
  pair is a mint-time capability that serves without the per-document gate;
  anything less than valid falls through to normal cookie/Bearer resolution
  and never removes access. Minting: `crap.uploads.sign_url(url,
  expires_in?)`; `expires_in` is capped at 30 days (relaxing the cap later
  is additive; the cap itself is not a frozen minimum).
