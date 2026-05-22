# Changelog

All notable changes to this project will be documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/).

## [0.1.0-alpha.9] — Unreleased

### Security

- **Strategy-authenticated users now pass the same
  `is_locked` / `verify_email` checks as bearer / cookie
  authentication.** Previously, a locked or unverified user could
  re-enter via a strategy hook because the evaluator's strategy
  branch skipped both checks. Closed.
- **Logout now bumps `_session_version`.** Cookie clearing alone
  left the issued JWT valid until exp — a captured token survived
  logout. The bump invalidates every JWT for that user across all
  surfaces. Also added to `lock_user` so a locked-out admin's
  active sessions die on the next request, not at JWT exp.
- **`Resolution::Invalid(Unaccepted)` for credentials that decode
  cleanly but aren't accepted anywhere.** Previously, a user with
  a cookie whose `session_cookie` method had been removed from
  the collection's `methods` list would get `Anonymous` →
  redirect to `/admin/login` → browser sends the same cookie →
  infinite redirect loop. Now the cookie is cleared on the
  redirect; gRPC returns 401 instead of silently treating the
  request as unauthenticated.

### Changed

- **Unified per-request auth evaluator at
  `service::auth::evaluate`.** Replaces the legacy split between
  the admin middleware's cookie-only fast path, the standalone
  strategy-fallback walker, and the gRPC handler's bearer-only
  `resolve_auth_user`. The evaluator walks every auth collection's
  `methods` in declaration order, honors the per-method `surfaces`
  filter (a Bearer JWT scoped to `admin` is not accepted on gRPC),
  and honors the `activates_on` discriminator on `strategy`
  methods (a strategy with `activates_on = { header = "x-api-key" }`
  fires only when that header is present on the request). The
  previous "model expressed scoping in types but runtime ignored
  it" gap is closed — these guarantees are now enforced end-to-end
  and pinned by e2e tests at `e2e/tests/grpc_methods_evaluator.rs`.
- `service/auth/` split into one file per concern: `local` (the
  password-login flow), `tokens` (reset + verification), `account`
  (lock / verified / session-version), `mfa`, `evaluator`.
  `service/auth/mod.rs` is now pure module declarations and
  re-exports (CLAUDE.md compliance).
- Tweaking a `password_login` method goes through a typed
  `PasswordLoginBuilder` (`AuthMethod::password_login_builder()
  .mfa(...).verify_email(...).build()`). The previous
  panic-on-missing / silent-no-op variants on `Auth` are still
  available as convenience methods (no-op when no password_login)
  but the builder is the recommended path for new code — misuse
  is a compile error rather than a silent miss.
- `Resolution::Invalid` carries a typed `AuthFailure`
  (`BadToken`, `Locked`, `StaleSession`, `UserMissing`,
  `UnknownCollection`, `Lookup`, `Unaccepted`) instead of a
  free-form string. Callers map each failure to a precise
  response — gRPC returns `PermissionDenied` for `Locked` and
  `Unauthenticated` with a specific message for the others; the
  admin middleware clears the stale session cookie before
  redirecting to `/admin/login` (except on `Lookup`, which is
  transient).
- Strategy hook contract: `ctx.headers` preserves the casing the
  transport delivered. Activation matching itself is
  case-insensitive — `activates_on = { header = "x-api-key" }`
  matches `X-API-Key`, `x-api-key`, etc. Existing Lua hooks that
  index headers by specific case continue to work.
- **Upload paths route through `state.token_provider`** instead
  of calling `auth::validate_token(token, state.jwt_secret)`
  directly. Removes the silent-mismatch risk if the JWT backend
  is ever swapped (the test fixture once carried exactly this
  bug — caught only when a refactor exposed it).
- **`admin/auth_middleware/` is now a folder split** into
  `middleware.rs`, `gate.rs`, `pages.rs`, `load_user.rs` — every
  business-logic file under the 300-line CLAUDE.md soft limit.
- **`crap-cms status --check` adds two new warnings:**
  - Auth collection with `password_login` but no `bearer` method
    (Login issues a JWT no future request can authenticate).
  - Multiple strategies bound to the same `(header, surface)` pair
    across collections — `HashMap` iteration order picks the
    winner non-deterministically.
- `Activation::Always` now wraps a private `AlwaysMarker` (the
  inner `always: bool` field is private and constrained to the
  literal `true`). `Activation::Always { always: false }` is no
  longer constructible in Rust code; the deserialize-time check
  was already in place. Wire format unchanged
  (`{ always = true }`).
- `is_locked` / `is_verified` on user documents now accept i64,
  bool, or string-coercible-to-bool ("0"/"1"/"true"/"false"/"yes")
  values. A strategy hook returning `_locked = "1"` (string) used
  to silently bypass the lock check.

### Breaking Changes

- **`crap-cms typegen` restructured into three subcommands.** The
  single `typegen --lang X [--proto M]` shape conflated server-side
  Lua extensibility types with consumer-side gRPC client types and
  with the Rust proto-conversion glue — three artifacts for three
  audiences sharing one verb. Split into:
  - `crap-cms typegen lua [-o DIR]` — server-side Lua types for
    hook authors. Writes `types/crap.lua` (API surface) +
    `types/hooks.lua` (per-collection narrowings). Replaces the
    implicit `crap.lua` side-effect that fired on every old
    `typegen` invocation.
  - `crap-cms typegen client -l <LANG[,LANG...]> [-o DIR]` —
    per-collection types for external API consumers. `--lang`
    accepts a comma list of `ts`, `go`, `py`, `rs`. Writes
    `types/client.<ext>` per language.
  - `crap-cms typegen proto -m <MODULE_PATH> [-o DIR]` —
    `From<proto::Document>` impls for Rust gRPC servers. Replaces
    the old `--proto` flag. Writes `types/proto.rs`.

  Output filenames renamed (the breaking part):
  - `types/generated.lua` → `types/hooks.lua`
  - `types/generated.{ts,go,py,rs}` → `types/client.{ts,go,py,rs}`
  - `types/generated_proto.rs` → `types/proto.rs`
  - `types/crap.lua` unchanged.

  Migration: replace `crap-cms typegen` (no args) with
  `crap-cms typegen lua`; replace `crap-cms typegen -l ts` with
  `crap-cms typegen client -l ts`; replace
  `crap-cms typegen -l rs --proto crate::proto` with
  `crap-cms typegen proto -m crate::proto`. Drop any stale
  `.luarc.json` paths pointing at `generated.lua` — the
  `Lua.workspace.library = ["./types"]` glob picks up
  `hooks.lua` automatically. The `--lang all` aggregator is
  gone; chain explicit invocations instead.

  Auto-regeneration on `crap-cms serve` is now gated on
  `admin.dev_mode = true`. Production startups no longer write
  the Lua type files. Run `crap-cms typegen lua` explicitly after
  a binary upgrade if you haven't enabled `dev_mode`.

- **`crap.schema.get_collection` / `get_global` field shape:** admin
  hints and join config are now nested objects instead of flat
  top-level fields, mirroring the write-side `crap.FieldAdmin` /
  `crap.FieldDefinition.join` shapes. Affects callers reading field
  metadata from the schema-introspection API.
  - **Was:** `field.language`, `field.features`, `field.picker`,
    `field.collection` (for `Join` fields), `field.on`.
  - **Now:** `field.admin.language`, `field.admin.features`,
    `field.admin.picker`, `field.join.collection`, `field.join.on`.
  - Also new: `field.picker_appearance` for `Date` fields (was
    silently dropped from the schema output before).
  - Migration: `field.picker` → `field.admin.picker` etc. The
    nested objects are skipped from the emitted table when empty
    (no `admin` block on a field with no admin-UI hints, no `join`
    block on non-`Join` fields).

- **`crap.util.json_encode` / `crap.util.json_decode` removed.** Use
  `crap.json.encode` / `crap.json.decode` instead. The dual
  registration was leftover technical debt from an incomplete
  migration to a dedicated `crap.json` namespace; the old aliases
  shipped as a back-compat measure that was never actually wound
  down. Cleaned up while restructuring the Lua API typegen
  surface — the new derive-driven generation expects a single
  canonical path per function, and `crap.json.*` is the cleaner
  location (matches Lua convention like `string.format`,
  `math.pi`). Migration: search-replace `crap.util.json_encode` →
  `crap.json.encode` and `crap.util.json_decode` →
  `crap.json.decode` in your collection definitions and hooks.

- **`auth.strategies`, `auth.disable_local`, `auth.verify_email`,
  `auth.forgot_password`, `auth.mfa` removed from
  `CollectionDefinition.auth`.** Replaced by a single ordered
  `auth.methods` list of typed entries (`password_login`,
  `bearer`, `session_cookie`, `strategy`). Password-only knobs
  (`mfa`, `verify_email`, `forgot_password`) move inside the
  `password_login` method variant. Each non-`password_login`
  method takes an explicit `surfaces` list (`{"admin"}`,
  `{"grpc"}`, or both) so per-surface auth is no longer
  implicit. `strategy` methods declare an `activates_on`
  discriminator (`{ header = "x-..." }` or `{ always = true }`)
  so each strategy is bound to its own activation signal — cross-
  collection accidental authentication is structurally
  impossible. New Lua helpers `crap.auth.default_methods()` and
  `crap.auth.with_defaults({...})` make the common cases
  one-liners. Full model documented in
  `docs/src/authentication/auth-methods.md`. Migration: rewrite
  each `auth = { … }` block per the new shape; the shorthand
  `auth = { enabled = true }` continues to work and populates
  the default methods.

### Internal

- **Lua typegen: every `--- @class crap.X` and `--- @alias crap.X`
  block now derives from a real Rust type.** The
  `src/typegen/lua/doc_structs/` directory is gone — what was a
  pile of phantom unit-structs and `()`-typed fields used only
  for documentation has been collapsed into real `Serialize`-
  or `Deserialize`-able Rust types. Drift between the runtime
  and the Lua-facing doc is now impossible by construction.
  Migrations in this pass:
  - `Document`, `HookContext`, `AccessContext`, `AuthStrategyContext`,
    `FieldHookContext`, `ValidateContext`, `JobHandlerContext` +
    `JobInfo`, `DeleteManyResult`, `SchemaCollection` +
    `SchemaField`, `HttpResponse`, `ValidateResult`,
    `VersionSummary`, `ListVersionsResult`, `FindResult` — derived
    on the real struct that the handler constructs and emits via
    `LuaSerdeExt::to_value(&struct)`.
  - `Activation` (refactored from `Always(AlwaysMarker)` to a
    struct variant) + `AuthMethod` — derived via the new
    `LuaTaggedClass` macro, which emits one
    `crap.X<VariantName>` class per variant (with the
    discriminator field as a literal) plus a `--- @alias crap.X`
    union. Better LuaLS narrowing than the old flat
    discriminated-union class.
  - `RichtextNodeSpec` — real Rust struct with hand-written
    `FromLua` that parses `attrs` into `Vec<FieldDefinition>`
    eagerly and keeps `render` as `mlua::Function`. The
    `register_node` handler now takes the typed struct directly.
  - `FieldWidth` — enum with `Full | Half | Third | Custom(String)`
    plus `From<String>` / `From<&str>` and serde
    `Serialize`/`Deserialize` round-trip. Replaces the old
    `width: Option<String>` field on `FieldAdmin`. `LuaAlias`
    derive extended for mixed unit + newtype variants — emits
    `crap.FieldWidth = "full" | "half" | "third" | string`.
  - `PickerAppearance` — strict 4-variant enum with `FromStr`,
    serde camelCase. Replaces `picker_appearance: Option<String>`
    on `FieldDefinition`. Parser warns + drops invalid values
    (the templates already treated anything not in the four
    canonical strings as `dayOnly`).
  - `ValidateFunction` / `FieldHookFn` — relocated to
    `core::field::definition` as `LuaTypeAlias` decls next to
    `FieldDefinition.validate` / `FieldHooks`.
  - `FindQuery`, `CountQuery`, `UpdateManyQuery`,
    `DeleteManyQuery`, `FilterOperators`, `FilterValue`,
    `FilterScalar`, `OrCondition` — new typed input structs in
    `hooks/lua_api/crud/query_input.rs`. The 4 query handlers
    (`find`, `count`, `update_many`, `delete_many`) accept these
    via `FromLua` (via `LuaSerdeExt::from_value`); each
    `into_find_query()` produces the runtime `db::FindQuery`.
    The old free-form `lua_table_to_find_query` walker is gone.
  Net result: `cargo xtask gen-lua-types` produces 130 typed
  blocks, every one of them sourced from a Rust definition that
  the compiler verifies.
- **New derives + macro extensions.**
  - `#[derive(LuaTaggedClass)]` — Rust tagged enum (or
    `#[serde(untagged)]`) → per-variant `--- @class` + union
    `--- @alias`. Honors `#[lua(tag = "...")]` and
    `#[lua(rename_all = "...")]` for the discriminator field +
    variant naming.
  - `#[derive(LuaAnnotation)]` supports generic lifetimes —
    context structs like `AccessContext<'a>` /
    `AuthStrategyContext<'a>` borrow request-scoped data instead
    of cloning into owned strings just to satisfy the derive.
    Stackable with `#[derive(LuaFieldTypeViews)]` on the same
    struct.
  - `#[derive(LuaAlias)]` mixed-mode — unit + newtype variants
    on one enum render as a single-line `"name" | type` union
    (was a hard error before).
- **Macro crate restructured.** `crap-cms-macros` now has one
  file per derive (`lua_annotation.rs`, `lua_alias.rs`,
  `lua_type_alias.rs`, `lua_field_type_views.rs`,
  `lua_tagged_class.rs`, `lua_fn.rs`, `lua_table.rs`) plus a
  `shared.rs` for the type-mapping + field-emission helpers
  cross-derives use. `lib.rs` collapses to the proc-macro
  registration shims.

### Fixed

- **Access functions that raise a Lua error now deny access (and
  log a warning) instead of returning `Status::internal`.**
  `check_access_with_lua` previously propagated `func.call(...)`'s
  `Err` as `anyhow::Error`, which `From<anyhow::Error> for
  ServiceError` wrapped as `Internal` → over gRPC, clients saw
  `Status::internal("Internal error")` and retried. The function
  now mirrors its own unexpected-return-type handling: catch the
  Lua error, log a `warn!` with the function ref + error, return
  `AccessResult::Denied`. Production clients now see
  `PERMISSION_DENIED` (correct) and stop retrying.
  Surfaced by `grpc_hook_errors::access_fn_error_maps_to_permission_denied`.

- **VerifyAccount / UnverifyAccount on a collection without
  `verify_email = true` now returns `FAILED_PRECONDITION` instead
  of `INTERNAL`.** The handlers wrap a SQL UPDATE that touches
  `_verified`, `_verification_token`, and `_verification_token_exp` —
  columns only provisioned when `auth.verify_email` is enabled.
  Calling them on a non-verify-email collection failed with "no
  such column", which `From<anyhow::Error> for ServiceError`
  mapped to `ServiceError::Internal` → `Status::internal`. The
  handlers now preflight-check the collection's `auth.verify_email`
  flag in `validate_verify_email_enabled` and return
  `FailedPrecondition` — correct per gRPC status-code semantics
  (server healthy, request well-formed, system state doesn't
  allow the operation).
  - `src/api/handlers/auth/account.rs`: new
    `validate_verify_email_enabled` helper, called from both
    `verify_account_impl` and `unverify_account_impl` before
    spawning the blocking task.
  - Regression test:
    `grpc_account_admin::verify_account_returns_failed_precondition_without_verify_email`.

- **Update / UpdateMany on a non-existent document id now returns
  `NOT_FOUND` instead of `INTERNAL`.** `query::update` raised an
  untyped `anyhow!("Document not found after update")` when the SQL
  UPDATE matched zero rows; `From<anyhow::Error> for ServiceError`
  had no way to distinguish it from a real internal error, so it
  surfaced as `ServiceError::Internal` → `Status::internal` over
  gRPC. Production clients treat `Internal` as transient and retry
  on it, so calling Update with a stale id triggered a retry loop
  instead of failing fast.
  - `src/db/query/write/update.rs` raises a new typed
    `DocumentNotFound` error (`pub` from `query::write`) when
    `conn.execute(UPDATE …)` reports 0 affected rows.
  - `From<anyhow::Error> for ServiceError` downcasts it and maps
    to `ServiceError::NotFound` → `Status::not_found`.
  - Discovered by the new `grpc_errors` test suite; the test
    `grpc_errors::update_unknown_id_returns_not_found` is the
    e2e regression net, `query::write::update::tests::update_on_missing_id_returns_typed_document_not_found`
    is the unit-level pin so future refactors of the query layer
    can't reintroduce the untyped path.

- **MFA-enabled auth collections now provision `_mfa_code` columns at
  creation time.** Previously the columns were only added during the
  ALTER path (which runs on existing tables on schema sync). On a
  freshly-created auth collection with `mfa = Email`, the migration
  omitted `_mfa_code` and `_mfa_code_exp`, so `set_mfa_code` failed
  silently when a user tried to log in and the verification email
  never got queued. The MFA challenge page would render but no code
  would ever arrive. Found via the new `html_mfa::mfa_email_full_flow`
  e2e test added as part of the alpha.9 regression net.
  (`src/db/migrate/collection/create.rs::collect_system_columns`)


### Security

- **`lettre` 0.11.21 → 0.11.22** (RUSTSEC-2026-0141). The advisory
  flags TLS hostname verification being disabled when using the Boring
  TLS backend. crap-cms ships with the `tokio1-rustls-tls` feature
  (rustls backend), so the vulnerable code path is not active in this
  project — but the upgrade clears the advisory and unblocks
  `cargo audit` in CI. Cargo.lock-only change; the `lettre = "0.11"`
  spec already permitted the patch.

### Added

- **e2e regression-net expansion (6 new tests across 3 files).** New
  test infrastructure: `e2e/src/email.rs` reads queued emails directly
  from `_crap_jobs` (the scheduler isn't running in tests, so emails
  sent via `email::queue_email` sit pending). Exposes `CapturedEmail`,
  `wait_for_queued_email`, `extract_token`, `extract_mfa_code`,
  `clear_queued_emails`. New tests:
  - `html_logout::logout_clears_session_cookies` and
    `logout_redirects_protected_request_to_login` — verifies POST
    /admin/logout clears `crap_session` and that subsequent
    unauthenticated requests redirect to login.
  - `html_password_reset::password_reset_full_flow` and
    `password_reset_rejects_mismatched_confirmation` — full round-trip
    through POST /admin/forgot-password → email capture → token
    extraction → POST /admin/reset-password → login with new
    password.
  - `html_mfa::mfa_email_full_flow` and `mfa_wrong_code_rejected` —
    login → MFA challenge redirect + cookie → email-code capture →
    POST /admin/mfa with code → session cookie set. The full-flow
    test caught the `_mfa_code` column migration bug fixed above.
  - `html_email_verify::verify_email_valid_token_marks_verified` and
    `verify_email_invalid_token_redirects_to_login` — admin-side
    consume path: token planted via `query::set_verification_token`,
    GET /admin/verify-email → user marked verified, login works.
    (Send-side test moves to a future CLI e2e workstream since
    email triggers come from `service::create_document`.)
  - `html_trash::soft_delete_moves_doc_to_trash`,
    `undelete_restores_doc_to_active_list`, and
    `empty_trash_purges_all_soft_deleted` — full trash lifecycle on
    a `soft_delete = true` collection: DELETE → ?trash=1 list →
    POST /undelete → POST /empty-trash.
  - `html_delete_refcount::hard_delete_blocked_when_referenced` and
    `back_references_shows_referring_documents` — verifies a
    referenced doc cannot be hard-deleted (400 + "Cannot delete:
    referenced by N document(s)" via the dialog path) and that the
    back-references endpoint returns metadata identifying the
    referring collection + field + count.
  - `html_version_restore::version_restore_reverts_doc_to_snapshot`
    — create, update (creates a snapshot), POST
    /admin/collections/{slug}/{id}/versions/{vid}/restore, verify
    the doc reverts to the snapshotted state.
  - `html_access_enforcement` (6 tests) — counterpart to the
    existing `html_access_gating.rs` (which tests UI button
    visibility). This file verifies the server **actually** rejects
    forbidden requests when a user crafts them directly (bypassing
    the hidden UI): `viewer_create_post_returns_403`,
    `viewer_update_post_returns_403`,
    `editor_delete_post_returns_403` (admin_only check),
    `admin_delete_post_succeeds` (positive control),
    `no_read_access_blocks_item_get` (read-fn returning false hides
    document data), `unauthenticated_post_returns_unauthorized`
    (no session → blocked or redirected).
  - `html_csrf` (3 tests) — POST without `crap_csrf` cookie → 403;
    POST with mismatched `X-CSRF-Token` → 403; POST with matching
    cookie+header redirects normally (positive control).
  - `html_404` (3 tests) — unknown collection list, unknown item id,
    and unknown global all return 404 (no 5xx, no content leak).
  - `html_dashboard` (2 tests) — GET /admin renders collection and
    global cards; unauthenticated GET /admin redirects to login.
  - `html_sort` (2 tests) — `?sort=title` orders rows asc, `?sort=-title`
    orders desc; verified by substring positions in the rendered HTML.
  - `html_custom_page` (2 tests) — Lua-registered custom page via
    `crap.pages.register` + `templates/pages/{slug}.hbs` renders at
    `/admin/p/{slug}`; unknown slug returns 404.
  - `html_search` (3 tests) — GET /admin/api/search/{slug} returns a
    JSON array; `?limit=N` caps results; unknown slug returns `[]`;
    `?q=...` filters by FTS match (test populates FTS via `fts_upsert`
    since `query::create` skips the service-layer side effect).
  - `html_optimistic_lock` (1 test) — pins the *current*
    last-write-wins semantics: two sequential POSTs to the same doc
    both succeed and B's data wins. When optimistic locking ships, the
    test will fail and force an explicit semantic update.
  - `html_conditional` (3 tests) — server-side display-condition
    evaluation. Registers `hooks/conditions/show_when_online.lua`,
    sets `condition = "hooks.conditions.show_when_online"` on a field,
    POSTs to `/admin/collections/{slug}/evaluate-conditions` with
    form state, verifies visible/hidden response. Includes the
    security gate that unknown condition refs fail open (visible).

- **gRPC e2e regression-net (8 new tests across 5 files, plus
  `spawn_grpc_server` harness).** The existing `tests/grpc_*.rs`
  files in the main crate construct `ContentService` directly and
  call trait methods with `tonic::Request` objects — they cover
  business-logic correctness per RPC but never cross the network,
  so the actual `tonic::Server` layer stack (health, reflection,
  HTTP/2 framing, real `tonic::Channel`) was untested. This bundle
  adds the missing transport-level coverage.
  - `e2e/src/grpc.rs::spawn_grpc_server` mirrors
    `crap_cms::api::server::start` but binds via a caller-owned
    `TcpListener` on `127.0.0.1:0` so each test gets an ephemeral
    port. Returns `GrpcTestCtx { pool, registry, config, addr,
    channel, shutdown, server_handle, … }` — pool + registry for
    DB seeding, channel for RPCs over real TCP, shutdown for
    clean teardown.
  - `grpc_smoke` — proves the full stack works end-to-end
    over real TCP via a single `Find` on an empty collection.
  - `grpc_health` (2 tests) — `grpc.health.v1.Health/Check`
    reports `SERVING` both for the empty-service overall query
    and the specific `crap.ContentAPI` service. Would catch a
    regression where `add_service(health_service)` falls out of
    the layer chain.
  - `grpc_reflection` — `grpc.reflection.v1.ServerReflection`
    lists `crap.ContentAPI`. This is what `grpcurl -plaintext
    HOST:PORT list` consumes; if it regresses, ad-hoc gRPC
    debugging without local proto files stops working.
  - `grpc_auth` (3 tests) — Login → use the returned token in
    a follow-up `Me` request; invalid token returns
    `UNAUTHENTICATED`; wrong password returns a non-`Internal`
    error code over the wire (the exact code depends on the
    handler's mapping; this test pins "not a server bug").
  - `grpc_subscribe` — opens a server-streaming `Subscribe`
    on a real `tonic::Channel`, issues a `Create` from a second
    client multiplexed over the same HTTP/2 connection, and
    verifies the create event arrives on the streaming connection
    within 3 seconds. Pins the streaming framing + multi-client
    HTTP/2 multiplexing that the in-process trait tests can't
    exercise.
  - `grpc_metadata_auth` (3 tests) — `ListJobs` requires the
    Bearer token in `authorization` gRPC metadata (separate path
    from the `Me`-style token-in-body). Verifies: missing metadata
    → `UNAUTHENTICATED`; valid Bearer → succeeds; invalid Bearer
    → `UNAUTHENTICATED` (not `INTERNAL`). Covers the HTTP/2
    header → `MetadataMap` → `extract_token` chain end-to-end.
  - `grpc_rate_limit` (2 tests) — `spawn_grpc_server_with_rate_limit`
    installs `GrpcRateLimitLayer` with a tight budget; bursting
    past the limit returns `RESOURCE_EXHAUSTED` over the wire,
    and a high limit doesn't throttle. The layer sits at the
    `tower::Service` level and is unreachable from the in-process
    `ContentService` trait tests.
  - `grpc_crud` (3 tests) — full CRUD round-trip
    (`Create` → `FindByID` → `Update` → `Find` → `Delete` →
    `Undelete`) over a real channel, plus `Count` happy paths and
    `force_hard_delete` semantics on soft-delete collections.
  - `grpc_bulk` (3 tests) — `CreateMany` / `UpdateMany` /
    `DeleteMany` over the wire. Pins the `repeated
    google.protobuf.Struct` framing the in-process tests can't
    exercise.
  - `grpc_globals` (2 tests) — `GetGlobal` auto-creates the
    document on first access; `UpdateGlobal` round-trips through
    `GetGlobal` and preserves unmodified fields under partial
    update. Closes a previously-empty surface in the e2e crate.
  - `grpc_schema` (3 tests) — `ListCollections` returns
    registered collections + globals with correct flag values
    (`auth`, `timestamps`); `DescribeCollection` returns field
    definitions in declaration order; `DescribeCollection` with
    `is_global = true` resolves globals correctly. Real clients
    (JS SDK, etc.) depend on this introspection surface.
  - `grpc_errors` (5 tests) — gRPC `Status` code mapping
    over the wire: `NOT_FOUND` on unknown collection slug,
    unknown document id, unknown global slug;
    `INVALID_ARGUMENT` on missing required field (with field
    name in message). **Surfaced one real bug**:
    `update_unknown_id_returns_user_recoverable_error` documents
    that Update on a missing id currently returns `INTERNAL`
    (worst-possible mapping — production clients retry on it)
    instead of `NOT_FOUND`. Test pins the negative invariant for
    now; tighten to `assert_eq!(status.code(), Code::NotFound)`
    when the handler is fixed.

  Main-crate surface change: `src/api/rate_limit::GrpcRateLimitLayer`
  is now `pub` (was `pub(crate)`) and gained `#[must_use]` on
  `::new`. Required so the e2e harness can install the layer
  without going through `api::server::start`, which only accepts
  `addr: &str` and doesn't return the bound port — needed for
  ephemeral-port-per-test isolation.

  - `grpc_validate` (2 tests) — `Validate` RPC round-trip.
    Valid data returns `valid=true` and an empty errors map;
    missing-required-field returns `valid=false` and the offending
    field name as a key in the `map<string, string> errors`.
  - `grpc_versions` (2 tests) — `ListVersions` returns one
    snapshot per create + per update with `latest=true` on the
    newest; `RestoreVersion` reverts the live document to the
    chosen snapshot's field values (verified via `FindByID` after
    restore).
  - `grpc_password_reset` (3 tests) — full forgot →
    `wait_for_queued_email_in_pool` → `extract_token` → reset
    flow over the wire, login with new password succeeds + old
    password rejected. ForgotPassword always returns success
    (no email-existence leak). Invalid token → non-Internal
    error. The pool-based email helpers
    (`read_queued_emails_from_pool`, `wait_for_queued_email_in_pool`,
    `find_queued_email_in_pool`) are siblings of the
    `TestApp`-based originals so harnesses without a `TestApp`
    (the gRPC ctx, future MCP ctx) can read the queue too.
  - `grpc_account_admin` (4 tests) — `LockAccount` /
    `UnlockAccount` round-trip verified via login behavior;
    `VerifyAccount` / `UnverifyAccount` round-trip on a
    `verify_email = true` collection; missing-Bearer call returns
    `UNAUTHENTICATED`. Regression test for the second bug fixed
    in this changelog
    (`verify_account_returns_failed_precondition_without_verify_email`).
  - `grpc_jobs` (3 tests) — `spawn_grpc_server_with_jobs` registers
    a `JobDefinition` in the registry; `TriggerJob` queues a run
    (`status = "pending"` since the scheduler isn't running in
    tests), `GetJobRun` fetches it by id with the input `data_json`
    round-tripped intact, `ListJobRuns` filtered by slug includes
    the new run. Unknown job slug → `NOT_FOUND`; unknown run id
    → `NOT_FOUND`.
  - `grpc_verify_email` (2 tests) — consume-side flow: plant a
    verification token via `query::set_verification_token` (what
    the send-side `service::email::send_verification_email` would
    do, minus the email rendering), call `VerifyEmail` over the
    wire, verify `_verified = 1` by attempting login (which a
    `verify_email = true` collection rejects for unverified users).
    Invalid token returns a non-`Internal` error.

  Final RPC coverage: all 31 RPCs in `proto/content.proto` now
  have at least one wire-level test. `spawn_grpc_server` /
  `spawn_grpc_server_with_jobs` / `spawn_grpc_server_with_rate_limit` /
  `spawn_grpc_server_with_lua` cover the four setup variants
  tests need.

  **Cross-cutting concerns** (Lua-driven access, hooks, custom auth)
  added on top of the RPC coverage. The harness's
  `spawn_grpc_server_with_lua(collections, globals, &[(path, src)])`
  writes inline Lua fixtures under `config_dir` before the
  `HookRunner` builds — same shape as
  `helpers::setup_app_with_access_files` but accepts any subdir
  (`access/`, `hooks/`, etc.).
  - `grpc_access` (5 tests) — `access.read` allow/deny based on
    `ctx.user`; `access.create`/`update`/`delete` map to
    `PERMISSION_DENIED` over the wire; `access.never` blocks
    everyone; constrained-access fn that returns a where-filter
    `{author_id = ctx.user.id}` for non-admins makes `Find`
    return only the caller's own rows.
  - `grpc_field_access` (3 tests) — field with
    `access.read = "access.admin_only"` is stripped from the
    response for non-admins (admin still sees it); field with
    write-denied `access.create`/`access.update` is silently
    dropped from incoming `data` (Create+Update both).
  - `grpc_hooks_lifecycle` (5 tests) — field-level
    `before_change` derives a slug from `name`; collection-level
    `before_change` stamps a field that survives a `FindByID`
    round-trip; `before_validate` raising `error(…)` maps to
    `INVALID_ARGUMENT`; `after_read` adds a computed field
    visible over the wire; `before_read` runs without breaking
    `Find`.
  - `grpc_hook_errors` (3 tests) — Lua `error(…)` from a
    lifecycle hook → `INVALID_ARGUMENT` (matches
    `ServiceError::classify` "hook error:" pattern); access fn
    raising an error → `PERMISSION_DENIED` (regression for the
    third bug fixed in this changelog); structured
    `crap.validation_error({field = "msg"})` → `INVALID_ARGUMENT`
    with the field name in the error message.
  - `grpc_auth_strategy` (3 tests) — Lua `authenticate` fn that
    falls back to "first user in collection" rescues a wrong-
    password Login; nil-returning strategy doesn't rescue; correct
    password still works alongside the strategy. (gRPC Login
    passes an empty headers map to strategies — a known
    limitation; strategies can only authenticate based on
    collection + DB lookup, not request metadata.)

- **Browser e2e regression-net expansion (6 new tests across 3 files).**
  Plugs gaps in the alpha.9 P0/P1 browser coverage; each test mounts
  the relevant web component and exercises its public API.
  - `browser_session_expiry` (2 tests) — verifies the
    `<crap-session-dialog>` singleton (`templates/layout/base.hbs`)
    mounts its shadow `<dialog>`, and that `dialog.show(message,
    { onStay, onLogout })` opens the dialog with the message text
    plus both action buttons. Bypasses the 5-minute `crap_session_exp`
    timer so the test runs in seconds.
  - `browser_locale_nav` (2 tests) — `<crap-ui-locale-picker>` only
    renders when `available_locales` is non-empty; verifies it appears
    with the configured locales (en + de), has one
    `[data-ui-locale-value=…]` per locale, and the dropdown gains
    `locale-picker__dropdown--open` after clicking the toggle.
  - `browser_filter_advanced` (2 tests) — extends existing
    `browser_list_settings::filter_builder_adds_condition` coverage:
    clicking "Add" three times produces three condition rows; clicking
    the per-row `.filter-builder__remove` drops one row back to one.

### Changed

- **Lua typegen template-context emits now mirror every typed Rust
  `admin::context::*` block.** Previously the generator emitted a small
  set of flat `crap.template_data_*` stubs that captured a handful of
  fields per block; the example's `generated.lua` had been hand-edited
  to add a fuller namespaced hierarchy that got lost on the next
  regenerate. `src/typegen/lua/render.rs::render_template_data_types`
  now emits the full hierarchy with every field the Rust context
  serializes — `crap.template.crap_meta` (including `site_name`),
  `crap.template.user`, `crap.template.breadcrumb`, `crap.template.page`
  (with the `type` union built from `PageType::as_str` so new page
  types appear in autocomplete automatically),
  `crap.template.{nav_collection, nav_global, custom_page}` plus the
  parent `crap.template.nav`, `crap.template.{admin_meta, upload_meta,
  versions_meta, auth_meta, field_admin_meta, field_meta}` for
  collection sub-shapes, the full `crap.template.collection` and
  `crap.template.global` with `versions` / `fields_meta` /
  `can_permanently_delete` / `soft_delete` etc.,
  `crap.template.document`, and `crap.template.editor_locale_option`.
  The aggregate `crap.template_ctx` carries every field
  `BasePageContext` serializes, including `nav` (non-optional),
  `breadcrumbs`, and `editor_locales`. The `crap.template_data_fn`
  alias points at `crap.template_ctx` (what `example/init.lua` and
  customer hook annotations expect). When a Rust typed-context grows a
  field, update the matching block in `render_template_data_types`
  rather than hand-editing `generated.lua`.

- **Admin UI now hides action buttons the user isn't allowed to use.**
  Previously the Create / Trash / Empty-Trash / Delete / per-row
  delete + restore buttons all rendered regardless of the user's
  per-collection access; clicking them just hit a 403 (often silently
  — see next entry). Now each surface checks the user's permissions
  for that collection / global up front:
  - `crap-cms` exposes `CollectionPermissions` / `GlobalPermissions`
    typed structs on collection-list / edit / create / form-error and
    global-edit page contexts (template field: `{{perms.*}}`). Each
    set is computed in one shared transaction per page render
    (collection: `read` / `create` / `update` / `delete` / `trash`;
    global: `read` / `update`).
  - `collections/items.hbs`, `items_empty.hbs`, `items_row.hbs`,
    `edit_sidebar.hbs`, and `globals/edit_sidebar.hbs` wrap their
    action UI in `{{#if perms.X}}`. The Save / Publish / Save-Draft /
    Unpublish row on the global edit sidebar gates on `perms.update`;
    the Delete panel on the collection edit sidebar disappears when
    the user has neither `trash` nor `delete`; the per-row Trash /
    Delete / Permanently-Delete / Restore buttons each check the
    matching flag. Cancel / read-only links stay visible.
  - The misleading `collection.can_permanently_delete` flag (which
    was *definition*-level — "is `access.delete` configured at all"
    — not per-user) is no longer used to gate UI, only to thread the
    soft-vs-hard mode into the JS confirm dialog.
- **403 responses now emit `X-Crap-Toast`.** `shared::response::forbidden`
  carries the access-denied message both in the rendered HTML body
  (for direct browser navigations) and in the `X-Crap-Toast` header
  (for htmx submits). htmx doesn't swap 4xx by default, so the
  client-side toast handler in `static/components/toast.js` picks the
  message up on `htmx:afterRequest` and surfaces it inline. Without
  this header htmx form submits to access-denied paths looked
  silently broken — the server enforced, but the user saw nothing.

- **`crap-cms update` (no subcommand) now surfaces a PATH-vs-store
  mismatch before the remote check.** Previously the "Already on the
  latest release" message was computed from the running binary's
  compile-time version, which silently misled when the user's shell
  was resolving `crap-cms` to something outside the store (e.g. a
  `cargo install --path .` dev build at `~/.local/bin/crap-cms`
  shadowing the store-managed `current` symlink). Now resolves the
  running binary against the store first: if it's outside the store
  entirely, suggests `update use --force <version>` to repoint PATH;
  if it's inside the store but not the active version, suggests
  `update use <version>`. The remote "already on latest" line still
  renders, but as a secondary `Remote:` info instead of a success.

- **`crap-cms update use --force` now actually relinks the `$PATH`
  binary** to point at the store's `current` symlink, instead of
  re-printing the misalignment warning. Stale symlinks (e.g. an old
  shim pointing elsewhere) are replaced silently. Regular files
  (e.g. a `cargo install` build sitting at `~/.local/bin/crap-cms`)
  prompt for confirmation before replacement, unless `--yes` is also
  passed. Distro-managed paths (`/usr/bin`, `/opt`, `/nix/store`, …)
  refuse to relink even with `--force` — those belong to the system
  package manager. Fixes the "lying flag" papercut where `--force`'s
  output was identical to the non-force run.

- **`FormData` type unifies admin form input.** New
  `crate::admin::FormData` (`src/admin/handlers/forms/form_data.rs`)
  carries the raw `HashMap<String, String>` form bag plus the typed
  join data (arrays, blocks, has-many relationships) extracted from
  it. Construction (`FormData::from_raw`) runs
  `transform_select_has_many` and `extract_join_data_from_form`
  internally — these were called in a fixed order at every site
  before; the type now encodes that invariant. Accessors:
  `raw()` / `raw_mut()` (for in-place upload-metadata injection),
  `join()`, `take(key)` / `get(key)` (generic meta-key extraction),
  `take_action()` / `take_locale()` (universal admin meta keys).
  `From<FormData> for DocumentFields` produces the merged typed
  payload for `service::WriteInput::builder`. Replaces the duplicated
  `let mut data = values_from_strings(form_data); data.extend(join_data);`
  dance that lived in `service::upload`, `admin::handlers::validate`,
  and three admin write handlers, plus a parallel error-render
  iterator chain in `globals::update_action::render_validation_error`.
  The `_blocking` input structs, validation params, and form-error
  renderers all take a single `FormData` instead of separate
  `form_data + join_data` pairs.
- **Spawn-blocking body names harmonized.** Every admin
  `task::spawn_blocking` body now follows the
  `<verb>_<noun>_blocking` convention (CLAUDE.md). Renamed
  `globals::update_action::execute_update` →
  `update_global_document_blocking`,
  `globals::edit_form::read_global_document` →
  `read_global_document_blocking`,
  `collections::item::edit_form::read_document` →
  `read_document_blocking`.
- **`unsafe` surface reduced.** `hooks::lua_api::crud::get_tx_conn`
  now returns `LuaResult<&dyn DbConnection>` instead of a raw fat
  pointer; the dereference and its safety argument move into the
  helper, eliminating 22 `unsafe { &*conn_ptr }` blocks across the
  Lua CRUD modules. `db::migrate::helpers::column_specs` no longer
  needs a lifetime-laundering transmute — `db::query::helpers::
  walk_leaf_fields` is now `for<'a>` over its callback's
  `&'a FieldDefinition` parameter, so the closure receives the right
  lifetime without a pointer round-trip. `commands::helpers::
  send_signal` is now the single `libc::kill` wrapper used by
  `commands/serve/process.rs`, `commands/work.rs::stop`, and
  `commands::helpers::is_process_running`.

- **Runtime `crap.<x>.define` error message harmonized.** The
  previous "for a NEW collection" / "Re-defining an already-registered
  collection is allowed" branching collapses to a single message:
  `must be called from a definition file or init.lua. To change a
  registered <x>, edit the file and restart the process.` Applies to
  `crap.collections.define`, `crap.globals.define`, `crap.jobs.define`.
  `crap.richtext.register_node`'s message stays as-is (it was
  already strict).

- `EmailRenderer::render` is now generic over `T: Serialize`. Built-in
  templates have typed contexts in `crate::core::email`:
  `PasswordResetEmailContext`, `VerifyEmailContext`,
  `MfaCodeEmailContext`. Lua/custom callers using `serde_json::Value`
  continue to work (`Value` implements `Serialize`).
- Webhook email provider builds its outgoing payload through typed
  `WebhookEmailPayload` / `WebhookFrom` structs in
  `core/email/webhook.rs` instead of an ad-hoc `json!()`.
- `commands/db/backup.rs` writes and `commands/db/restore.rs` reads
  the backup `manifest.json` through a shared `BackupManifest` struct
  instead of ad-hoc `serde_json::Value` lookups.
- Upload HTTP API JSON responses use typed bodies:
  `api/upload/helpers.rs` exposes `DocumentBody` and `SuccessBody`,
  the local `ErrorBody` is constructed by `json_error`, and `json_ok`
  is now generic over `T: Serialize`.
- `core/upload/metadata.rs::assemble_sizes_object` builds the nested
  `sizes` payload through typed `ImageSizeEntry` / `FormatVariant`
  structs and serializes once at the boundary, replacing layered
  `Map::new() + insert(Value::String(...))` plumbing.
- MCP collection tool responses (`find`, `list_versions`, `count`,
  `create_many`) use small typed wrapper structs that embed
  `PaginationResult` directly.
- `mcp::tools::schema::introspection::exec_list_field_types` returns a
  typed `&[FieldTypeInfo]` constant; the table lives in code instead
  of a `json!([...])` literal.
- New `crate::core::ReqContext` newtype around the request-scoped
  hook-context bag (the per-request scratchpad Lua hooks read/write
  across the lifecycle). Replaces `HashMap<String, Value>` in
  `WriteResult`, `AfterChangeInput.req_context`, `DeleteResult.context`,
  `Upload{Create,Update}Result.req_context`, `HookContext.context`,
  and the corresponding builders. The newtype derefs to
  `HashMap<String, Value>` and has `From<HashMap>` / `Into<HashMap>`
  for transparent boundary conversion. Adds `get_str` / `get_bool` /
  `get_i64` typed accessors. Builders accept `impl Into<ReqContext>`
  so existing call sites that already had a `HashMap` keep compiling.
  No outside-API change — proto/Lua/admin-template surfaces serialize
  transparently via `#[serde(transparent)]`.
- New `crate::core::ConditionExpr` typed enum representing the
  client-evaluable display condition shape emitted by Lua hooks.
  `DisplayConditionResult::Table` now carries a typed `ConditionExpr`
  rather than a free-form `serde_json::Value`. The grammar is now an
  explicit Rust contract: `ConditionExpr = Single(ConditionRow) |
  All(Vec<ConditionRow>)` with `ConditionOp` operators
  (`equals`, `not_equals`, `in`, `not_in`, `is_truthy`, `is_falsy`).
  Wire format unchanged — `untagged` + `flatten` + externally-tagged
  serde representation matches what `static/components/conditions.js`
  expects byte-for-byte. The legacy `evaluate_condition_table` free
  function is gone; condition evaluation is now a method on the typed
  enum. **Behavior change**: malformed condition tables (missing
  `field`, unknown operator, non-object/non-array shapes) used to
  default-to-show silently; they now fail to deserialize and the seam
  hides the field (fail-closed) with a `warn!` log identifying the
  offending hook ref.
- New `crate::db::query::SortValue` enum (`Null` / `Bool` / `Integer` /
  `Real` / `Text`). Replaces `CursorData.sort_val: serde_json::Value`
  with a typed payload that mirrors the sortable subset of `DbValue`.
  `From<&Value>` and `From<&SortValue> for DbValue` keep the existing
  doc-field → cursor → SQL parameter pipeline intact. Wire-format
  compatible — `#[serde(untagged)]` reproduces the raw JSON scalar in
  `sort_val` slot of the cursor token, so existing cursor URLs decode
  unchanged.
- MCP `list_collections` and `describe_collection` tool responses are
  typed: `ListEntry` (untagged collection/global) and
  `DescribeResponse` (internally-tagged on `type` discriminator) in
  `mcp/tools/schema/introspection.rs`. Wire format preserved exactly.
- MCP `cli_reference` command-detail data table is typed: ~500 lines
  of `json!({...})` replaced with 24 `static CliCommandDetail`
  constants and supporting structs (`CliOverview`,
  `CliCommandSummary`, `CliFlag`, `CliArg`, `CliSubcommand`,
  `CliReferenceError`). Output bytes unchanged — verified with a
  before/after dump comparison across all 24 commands plus their
  alias forms.
- `db::query::populate::helpers::document_to_json` now builds its
  output through a typed `PopulatedRef` struct with
  `#[serde(flatten)]` over the document's fields. Replaces the manual
  `Map::new() + insert(Value::String(...))` chain. Wire format
  identical (existing tests pass unchanged).
- `db::query::populate::batch::dispatch::join_key_from_value` replaces
  an `other.to_string().trim_matches('"')` hack with a typed match
  over `Value::{String, Number, Bool}`. **Behavior change**: arrays
  and objects, which previously produced garbage keys like `"[1,2]"`
  that never matched a parent ID, are now skipped explicitly. Pinned
  by 6 unit tests in a new pure `join_key_tests` mod.
- `db::query::join::blocks::split_block_row` extracts the duplicated
  `_block_type` + `data_json` build pattern from the locale and
  non-locale `set_block_rows` branches into a single helper.
- `db::query::fts::prosemirror` adds a typed `ProseMirrorNode<'a>`
  borrow view over the `{type, text, attrs, content}` shape used by
  the FTS extractor. Replaces inline `.get(...).and_then(Value::as_*)`
  chains; pure clarity refactor with no behavior change.
- `core::validate::FieldError::with_key` is no longer 4-arity. Drop
  the trailing `params: HashMap<String, String>` argument; chain
  `.with_param(name, value)` instead. ~30 call sites across
  `hooks::lifecycle::validation::checks/*`, `recursive.rs`,
  `richtext_attrs.rs`, `sub_fields/validate.rs` simplified; the
  `use std::collections::HashMap;` import drops out of 10 leaf check
  files.
- `hooks::lifecycle::crud::helpers::ExtractedData` exposes
  `join_data: HashMap<String, Value>` directly instead of a merged
  `hook` map that callers re-filtered. Drops the `flat-as-strings +
  join_data merge → re-filter for non-strings` round-trip from four
  call sites (`create.rs`, `update.rs`).
- All five `scaffold/*/generator.rs` Handlebars contexts use typed
  `#[derive(Serialize)]` structs (`CollectionTemplateContext`,
  `CrapTomlContext`, `GlobalTemplateContext`, `JobTemplateContext`,
  plus `CollectionHookContext` / `FieldHookContext` / `AccessHookContext`
  / `ConditionBooleanContext` / `ConditionTableContext`) instead of
  ad-hoc `json!({...})`.
- MCP write tool responses use small typed Serialize structs:
  `DeletedResponse`, `RestoredResponse`, `DeleteManyResponse`,
  `UpdateManyResponse`, `WrittenResponse`, `ConfigFileEntry`
  (`#[serde(rename_all = "snake_case")]` enum `ConfigFileKind`),
  `NotFoundResponse`. All MCP tool serializations standardized on
  `to_string_pretty` for LLM-consumer readability.
- `mcp::tools::schema::introspection::exec_cli_reference` now takes
  `command: Option<&str>` instead of `&Value`. The three
  `mcp::tools::schema::config_files::exec_*` signatures take
  `path: &str` / `content: &str` / `subdir: Option<&str>` instead
  of `&Value`. The MCP dispatcher extracts those fields once at the
  call site and surfaces "Missing X argument" errors there.
- `mcp::resources::collections_schema` and `globals_schema` return
  typed `BTreeMap<String, CollectionSchemaEntry>` /
  `BTreeMap<String, GlobalSchemaEntry>` instead of `Map<String, Value>`
  with hand-rolled `json!({...})` per entry. Inner `schema: Value`
  stays as JSON Schema (KEEP-PROTO).
- New shared `crate::commands::export::file::ExportFile` struct
  (`crap_version`, `exported_at`, `collections: Map<String, Value>`)
  used by `commands::export::export_cmd` (writer) and
  `import_cmd` (reader) instead of constructing/destructuring an
  ad-hoc `Value`. Inner per-document payloads stay `Value` (doc
  fields).

### Fixed

- Verification email now includes the configured `from_name` in the
  footer. Previously the only call site
  (`service::email::send_verification_email`) forgot to pass it and
  the template silently rendered `Sent by` with a blank trailer
  because Handlebars strict mode is off. Regression test in
  `core::email::renderer::tests::verify_email_renders_from_name`.

### Internal

- **Cargo workspace migration.** The repo is now a Cargo workspace
  with three members at the root: `crap-cms` (main crate, unchanged
  binary), `crap-cms-e2e` (`e2e/` — end-to-end and HTML integration
  tests, formerly `tests/e2e/`, now in the canonical Rust layout
  with shared fixtures in `e2e/src/{browser,helpers,html}.rs` and
  one integration-test binary per `e2e/tests/<name>.rs`), and
  `crap-cms-macros` (`macros/` — proc-macro crate, currently an
  empty stub). New `setup_html_test` / `setup_html_test_with_config`
  / `setup_html_test_with_access_files` helpers in
  `e2e/src/helpers.rs` and `setup_browser_test` /
  `setup_browser_test_with_config` in `e2e/src/browser.rs` collapse
  the 3-line HTML setup ritual (`setup_app` + `create_test_user` +
  `make_auth_cookie`) and 7-line browser setup ritual (`spawn_server`
  + user + cookie + `launch_browser` + `new_page` + `browser_login`)
  into a single call per test. 117 of 153 e2e tests adopt the new
  helpers; remaining 36 have setup variations (data injection
  between auth and browser launch, custom page configuration,
  role-based users, auth-flow tests) and stay on the underlying
  primitives. Shared dependency,
  metadata, and lint configuration moved to `[workspace.package]`,
  `[workspace.dependencies]`, and `[workspace.lints]` so all current
  and future members inherit them via `*.workspace = true`. The
  `browser-tests` feature went away entirely — the e2e crate's own
  membership boundary is the gate, so an internal feature flag is
  redundant. `chromiumoxide` is a regular dep of the e2e crate.
  CI's `check` job now runs
  `cargo test --workspace --exclude crap-cms-e2e` (the dedicated
  `e2e` job runs `cargo test -p crap-cms-e2e -- --test-threads=1`).
  Release/nightly cross-builds
  pin `-p crap-cms` to skip the test-only e2e crate. The
  `default-members = [".", "macros"]` setting keeps plain
  `cargo build` / `cargo test` from root focused on the main crate
  + macros stub, avoiding accidental chromiumoxide compiles during
  routine development. New CLAUDE.md `Workspace layout` section
  documents the structure and how to add future members.

- **Clippy pedantic sweep — `cargo clippy --all-targets` is now clean.**
  Production code (`--lib --bin`) is held to the strict pedantic set;
  the only workspace-level allows remain `implicit_hasher` and
  `struct_excessive_bools` (both pre-existing, both have documented
  rationale in `Cargo.toml`). Test code is held to a narrower set —
  pedantic lints that surface as noise without catching real issues in
  tests (`cast_*`, `match_wildcard_for_single_variants`,
  `needless_pass_by_value`, `similar_names`, `too_many_lines`,
  `unreadable_literal`, `used_underscore_binding`,
  `missing_panics_doc`, `items_after_statements`,
  `case_sensitive_file_extension_comparisons`) are allowed
  per-integration-test-file (`#![allow(…)]` on each `tests/*.rs`) and
  per-`mod tests` block on the lib unit-test modules that have them.
  Substantive findings (architectural fixes, real bugs, real docs
  gaps) were applied as code changes rather than allows — e.g. handler
  splits into thin orchestrators (`list_items`, `edit_form`,
  `build_router`, `run`), bootstrap + per-server-task helpers in
  `serve::startup`, a `JoinTarget` bundle in batch populate dispatch,
  `Default::default()` → typed-constructor calls, and a few targeted
  patterns (`writeln!` over `push_str(&format!(...))`, `let-else` over
  `match` + `panic!`, `matches!` over identical-arm match).

- **Stutter-rename pass.** Files whose names repeated their parent
  directory got their prefixes dropped — the prefix is informative
  inside the type name (`FieldDefinition`, `AuthConfig`) but
  redundant inside the path.
  Renames: `core/auth/auth_user.rs` → `user.rs`,
  `core/collection/collection_definition.rs` → `definition.rs`,
  `core/field/{field_admin,field_definition}.rs` →
  `{admin,definition}.rs`,
  `core/richtext/richtext_node_def.rs` → `node_def.rs`,
  `config/auth/auth_config.rs` → `config.rs`,
  `config/server/server_config.rs` → `config.rs`,
  `scaffold/collection/collection_options.rs` → `options.rs`,
  `admin/context/field/field_context.rs` → `context.rs`,
  `admin/handlers/field_context/enrich/{enrich_ctx,enrich_options,enrich_types}.rs`
  → `{ctx,options,types}.rs`,
  `admin/handlers/collections/items/validate/{validate_create,validate_update}.rs`
  → `{create,update}.rs`,
  `service/hooks/{read_hooks,write_hooks}.rs` → `{read,write}.rs`.
  No type names changed. `commands/export/{export_cmd,import_cmd}.rs`
  were left as-is — `module_inception` clippy fires on
  `commands/export/export.rs`; renaming those two needs the parent
  dir restructured first.

- **Keyword-name and unclear-name fixes.**
  `core/document/type.rs` → `kind.rs` drops the `r#type` keyword
  workaround in `core/document/mod.rs`. `db/query/fts/prosemirror.rs`
  → `extract.rs`; the `prosemirror` prefix already lives in the
  exported function names (`extract_prosemirror_text`, etc.) and the
  parent `fts/` module supplies the search context.
  `commands/update/{use_action,where_action}.rs` left as-is — the
  `_action` suffix there is also a keyword workaround (`use` and
  `where` are reserved).

- **Registry definition APIs are now strictly init-only at runtime.**
  `crap.collections.define`, `crap.globals.define`, `crap.jobs.define`,
  and `crap.richtext.register_node` all reject calls outside the
  init phase, for both new and existing slugs. Previously
  `collections`/`globals`/`jobs` allowed existing-slug redefinition at
  runtime — the test-only artefact that justified that branch
  (`tests/lua_api_filters.rs` redefine tests evaluating against the
  runtime VM) has been rewritten to set `InitPhase` before the
  redefine call. New helper `HookRunner::eval_lua_init_with_conn` +
  `tests/lua_api_filters.rs::eval_lua_init` mirror the init-time
  evaluation path. The "documented round-trip pattern" comment in
  `lua_api/collections.rs` is dropped — real plugin loops over
  `crap.collections.config.list()` run from `init.lua` (or files it
  requires) where `InitPhase` is set throughout, so the strict guard
  never fires for legitimate plugin code. Mirrors the policy
  `crap.richtext.register_node` has had since inception.

- **`hooks::init::load_lua_dir` now caches into `package.loaded`.**
  Each `<config_dir>/{collections,globals,jobs}/foo.lua` is evaluated
  once at boot and its return value (or `true` for files without
  `return`) is stored at `package.loaded["<dir>.<stem>"]`. The job
  dispatcher's later `require("jobs.foo")` hits the cache instead of
  re-evaluating the file's top-level — which is what made the strict
  guard tractable for `crap.jobs.define`, since handler files
  conventionally mix `crap.jobs.define(...)` at the top with the
  handler function in the returned module table.

- **`RegistryRead` trait + crud reads use `Arc<Registry>`.** New
  `core::RegistryRead` trait abstracts over `SharedRegistry`
  (locks per call) and `Arc<Registry>` (no lock). `crap.access.*` and
  `crap.schema.*` registration functions are generic over the trait
  so init and runtime VMs share one body. The runtime CRUD layer
  (`hooks::lua_api::crud::*`) was migrated to take `Arc<Registry>`
  directly — `HookRunnerBuilder` snapshots the populated registry
  once at construction and hands the snapshot to
  `register_crud_functions`. CRUD reads from Lua hooks (find,
  find_by_id, count, etc.) are now lock-free.

- **`commands/cli_types.rs` and `config_resolve.rs` renamed.**
  `cli_types.rs` → `types.rs` — the `cli_` prefix was dead weight
  (already inside `commands/`, and the separate top-level `cli/`
  module owns CLI presentation, not action enums). `config_resolve.rs`
  → `resolve_config.rs` — verb-first reads as the action it performs
  (`commands::resolve_config::resolve_config_dir`). Both files have
  no external callers; only `commands/mod.rs` needed updating
  (declarations, `pub use` lines, doc comment).

- **`commands/mod.rs` flat-vs-folder rule made explicit.** The
  doc-comment now codifies what the layout already followed
  implicitly: single-action subcommand → flat file (`fmt.rs`,
  `init.rs`, `work.rs`, …); multi-action subcommand → folder
  (`db/`, `user/`, `make/`, …). Default to a flat file; promote to
  a folder the first time a second action lands. No file moves.

- **`service/collection/` renamed to `service/collections/`.** All
  other noun-feature dirs in `service/` (`globals/`, `jobs/`,
  `versions/`, `hooks/`, `types/`) are plural — `collection/` was
  the lone singular outlier. "Collections" is the framework's named
  feature surface, same category as the others. Verbs (`read/`,
  `write/`, `persist/`) stay singular. Zero blast radius for callers:
  the file already re-exported every public item at
  `crate::service::*`, so only `service/mod.rs` saw the change. Two
  other apparent singular/plural inconsistencies (`admin/handlers/
  collections/` lone-plural; `db/query/{jobs,versions}/` plural-
  while-siblings-singular) turned out to be correctly applying the
  three-way rule (singular for operations + subsystems, plural for
  named CMS features) — left as-is.

- **`ServiceContext` promoted out of `service/types/`.** Moved
  `service/types/service_context.rs` (591 lines) to
  `service/context.rs`. `ServiceContext` is the central runtime
  context bundle threaded through 129 call sites, not a request /
  result data class — sitting in `types/` next to 18-line
  `*_input.rs` files mixed two concerns. The remaining `types/` is
  now coherent (request shapes, results, queue infra, two domain
  contexts). Zero blast radius for callers: `crate::service::
  ServiceContext` was already the canonical import path via
  `service/mod.rs` re-export, so only `service/mod.rs` and
  `service/types/mod.rs` saw the change. `Def` enum (the variant
  selector that lives alongside `ServiceContext`) moved with it.
  Filename follows the established stutter-rename pattern
  (`core/auth/auth_user.rs` → `user.rs`): the `service_` prefix is
  redundant once inside `service/`.

- **`hooks/validate.rs` renamed to `hooks/startup_checks.rs`.**
  The old name overlapped with `hooks/lifecycle/validation/` (per-write
  field validation) — different time and scope. The renamed file holds
  the post-init correctness passes (`validate_hook_references`,
  `validate_locale_field_collisions`) that walk the registry once at
  boot. Two call sites in `hooks/init.rs` updated; `hooks/mod.rs` doc
  comment now contrasts the two modules.

- **`hooks/api/` renamed to `hooks/lua_api/`.** The original `api`
  name collided in conversation with the top-level `api/` module
  (gRPC). `lua_api` is what the directory actually is — the surface
  registered onto every Lua VM as `crap.*`. Touched the directory
  itself, `hooks/mod.rs` declarations + doc comment, `hooks/init.rs`,
  and ~20 `hooks::api::*` import sites across `hooks/` and inside
  `hooks/lua_api/` (siblings used absolute paths). No external
  callers outside `hooks/`.

- **`hooks/lifecycle/crud/` relocated to `hooks/lua_api/crud/`.**
  The runtime CRUD surface (`crap.collections.{find,create,update,…}`,
  `crap.globals.{get,update}`, `crap.jobs.queue`) was historically in
  `lifecycle/` because it depends on the `TxContext` machinery, but
  every other `crap.*` registration lived in `lua_api/`. The split
  was leaky — `lua_api/email.rs` reached into
  `lifecycle::crud::get_tx_conn` to grab the active transaction. With
  the move, every `crap.*` registration site lives under one tree;
  the `email.rs` leak collapses to a sibling import. Internal CRUD
  files keep their `crate::hooks::lifecycle::{…}` imports for the
  TxContext / converter / runner types they still need from
  `lifecycle/`. `register_crud_functions` is now reached at
  `hooks::lua_api::crud::register_crud_functions`; the one external
  caller (`HookRunnerBuilder`) was updated. `lua_api/mod.rs` gained
  an architecture sketch documenting the new layout.

- **`db/{postgres,sqlite}.rs` moved into `db/backend/`.** The `db/`
  module root used to mix abstractions (`connection.rs`, `pool.rs`,
  `types.rs`, `ops.rs`, `document.rs`) with the two engine impls.
  Backends now collect under `db/backend/{postgres,sqlite}.rs` behind
  a thin `db/backend/mod.rs` that carries the existing
  `#[cfg(feature = "...")]` gates. `db/mod.rs` declares `pub mod
  backend` and updates the test-only `pub use sqlite::InMemoryConn`
  re-export to `pub use backend::sqlite::InMemoryConn`. Internal
  imports inside the moved files swap `super::{connection,types,pool}`
  for their re-exported short paths (`crate::db::{DbConnection,
  DbRow, DbValue, BoxedConnection, DbPool}`); only the non-re-exported
  `ConnectionInner`, `TransactionInner`, and `PoolBackend` keep their
  full `crate::db::{connection,pool}::*` paths. Three external call
  sites updated (`db/pool.rs` ×2, `db/query/filter/operators.rs` ×1).
  Both `--features sqlite` (default) and `--features postgres` build
  clean.

- **`helpers.rs` over `shared.rs`.**
  `commands/templates/shared.rs` → `helpers.rs`; updated three
  importers and the `mod.rs` doc comment. `hooks/api/parse/shared.rs`
  was left as-is because it sits next to a separate `helpers.rs`
  (primitive table getters) and itself holds higher-level definition
  parsers — merging would push past 800 lines and erase a meaningful
  split.

- **Inline `use` cleanup, codebase-wide.** CLAUDE.md's "tree-style
  imports at the top of the file/module. Never use inline `use`
  statements inside function bodies" rule had drifted in the
  early-audited modules. Swept 33 violations across 25 files; each
  inline `use crate::core::Foo;` style import inside a `#[test] fn`
  body was lifted to the top of the surrounding `mod tests` block
  (or to the file top for files like the `hooks/lifecycle/validation/
  sub_fields/tests/*` test modules that are themselves the test
  scope). Also flattened ~30 nested `core::{... field::{X, Y} ...}`
  grouped-import patterns that the earlier deep-path sweep missed
  (the pattern only matched at one level), so any leaf-module
  re-exported type now lives directly inside the outer `core::{...}`
  list. 3851 lib tests pass; clippy + fmt clean.

- **Retroactive pass: applied late-playbook axes to the early-audited
  modules.** The original `core/`, `db/`, `hooks/`, `service/`, `api/`
  audits ran before axes 25 (`mod.rs` architecture sketch) and 26
  (workspace-split prep — kill external `crate::module::sub::Foo`
  imports) crystallised. This pass closes the gap:
  - **`core/`: 183 external deep-path imports → ~30 namespace-only.**
    Promoted `Access`, `Hooks`, `IndexDefinition`, `Labels`, `LiveMode`,
    `LiveSetting`, `VersionsConfig` (from `core::collection`) and
    `JoinConfig`, `to_title_case` (from `core::field`) and
    `SharedStorage` (from `core::upload`) to top-level `crate::core::*`
    re-exports. A Python-script-driven sweep then flattened 129
    callers' `use crate::core::<sub>::Type` imports to
    `use crate::core::Type`. Remaining ~30 deep paths are intentional
    (cache/rate_limit/event/email namespace prefixes carry semantic
    meaning per CLAUDE.md's exception, plus the few builder
    direct-construction sites that need a separate caller refactor).
  - **`db/`: 12 external deep-path imports → 0.** `LocaleContext`,
    `LocaleMode`, and `Singleflight` were already top-level
    re-exported; callers were just using the deep `db::query::*` form.
    Same script flattened them.
  - **`mod.rs` architecture sketches** added to `core/` (1 → 50
    lines), `db/` (1 → 45), `hooks/` (1 → 35), `api/` (1 → 30),
    `service/` (5 → 50). Matches the layout/conventions pattern the
    later admin/, mcp/, scaffold/, scheduler/, config/, fmt/ audits
    established.
  - **`api/`: `start` and `GrpcStartParams` promoted** to
    `crate::api::*`. The two callers in `commands/serve/startup.rs`
    were going through `api::server::start` /
    `api::server::GrpcStartParams::builder()`; now use the flat
    forms.

- `src/fmt/` code-quality cleanup pass. Inventory was structurally
  already clean (4 files, 1594 LOC, all under the 1000 soft limit, 0
  `#[allow]`, 0 `super::super`, 0 manual `Default`, 0 external deep
  paths). Concrete changes:
  - **Visibility tightening:** `pub mod printer` and `pub mod
    tokenizer` demoted to `pub(crate) mod`. The single external
    consumer (`commands/fmt.rs`) only imports
    `crate::fmt::format` — neither submodule needs a public
    surface.
  - **`emit_start_tag` (6 positional args) refactored to a typed
    `EmitStartTag<'_>` input struct.** Only >4-arg fn in the
    module.
  - **`mod.rs` architecture sketch** expanded from a 5-line `//!`
    to a 25-line layout/conventions map covering the `tokenize ->
    print` pipeline and the idempotency invariant.
  42 fmt unit tests pass; clippy clean.

- `src/config/` code-quality cleanup pass. Inventory was already in
  good shape on the playbook structural axes (0 `#[allow]`, 0
  `super::super`, 0 external deep-path imports, 0 wide-arg fns) but
  three files were "shared.rs"-style multi-type homes. Concrete
  changes:
  - **`features.rs` (901 LOC, 17 unrelated types) decomposed into
    `features/`** with one file per `[<section>]` table in
    `crap.toml`: `email.rs`, `depth.rs`, `cache.rs`, `pagination.rs`,
    `mcp.rs`, `upload.rs`, `locale.rs`, `jobs.rs`, `live.rs`,
    `hooks.rs`, `access.rs`, `logging.rs`, `update.rs`. Genuinely
    paired enums stay together (`SmtpTls` with `EmailConfig`,
    `PaginationMode` with `PaginationConfig`, `LogRotation` with
    `LoggingConfig`). Each file is 21-170 LOC with its own `Default`
    and colocated tests. `features/mod.rs` re-exports keep the flat
    `config::*` API unchanged.
  - **`types.rs` (1162 LOC, over the 1000 soft limit) split**: the
    `CrapConfig` struct + `load` / `test_default` / `validate`
    orchestrator + version check + path helpers + permission warnings
    stay in a slimmed `types.rs` (512 LOC). The 14 per-section
    `validate_*` methods (with their ~30 colocated tests) move to a
    new `validate.rs` (648 LOC) as additional `impl CrapConfig`
    blocks. The orchestrator still calls them by name; visibility is
    `pub(super)` since they're never invoked outside the
    orchestrator.
  - **`server.rs` (707 LOC, 5 types) decomposed into `server/`**:
    `server_config.rs` (`ServerConfig` + paired `CompressionMode`),
    `database.rs` (`DatabaseConfig`), `admin.rs` (`AdminConfig`),
    `csp.rs` (`CspConfig` + the header-builder logic + 7 dedicated
    tests). `CspConfig` is reachable via `AdminConfig::csp` but no
    external caller imports it by name -- kept private to the
    `server::admin` module.
  - **`auth.rs` (423 LOC, 3 types) decomposed into `auth/`**:
    `auth_config.rs` (`AuthConfig` + paired
    `SessionCookieSameSite`), `password_policy.rs` (`PasswordPolicy`
    with its standalone `validate()` and 12 strength tests).
  - **`mod.rs` architecture sketch** expanded from a one-line `//!`
    to a 30-line layout/conventions map covering submodule layout,
    secret-newtype wrappers, default-impl conventions, and the
    `#[serde(deny_unknown_fields)]` policy on every section.
  217 config tests pass; clippy clean. Net file count: 11 -> 30.

- `src/scheduler/` code-quality cleanup pass. Inventory was already in
  good shape (4 files, 1688 LOC, 0 `#[allow]`, 0 `super::super`,
  0 manual `Default` impls, 0 external deep-path imports). Concrete
  changes:
  - **Pure-ceremony `SchedulerParamsBuilder` deleted** in favour of
    plain struct-literal construction. The builder existed to wrap a
    7-arg `new()` plus three optional setters, but both call sites
    (`crap-cms work` and `serve`'s startup) supplied every field
    anyway -- the "optional" defaults were never used. `SchedulerParams`
    fields are now `pub`; both call sites construct it via
    `SchedulerParams { pool, hook_runner, registry, … }`. The builder
    type and its top-level re-export are gone.
  - **Wide-arg helpers refactored to typed `*Input<'_>` structs.**
    Three helpers crossed the >4-arg threshold:
      - `run_periodic_purges` (7 args -> `PurgeTickInput<'_>`)
      - `purge_collection` (6 args -> `PurgeCollectionInput<'_>`)
      - `spawn_job_execution` (6 args -> `SpawnJobInput<'_>`)
    Call sites now read at a glance instead of counting positional
    arguments.
  - **Visibility tightened.** `RETENTION_PURGE_SLUG` and
    `claim_retention_purge_tick` were `pub` but only used inside
    `scheduler/` (loop_runner + runner's own tests). Demoted to
    `pub(super)` and dropped from the `scheduler::*` re-export
    block. The remaining re-exports (`start`, `execute_job`,
    `check_cron_schedules`, `purge_soft_deleted`,
    `recover_stale_jobs`, `SchedulerParams`) all have ≥1 external
    consumer (call sites in `commands/work.rs`, `commands/serve/
    startup.rs`, `tests/scheduler.rs`, `tests/db_soft_delete.rs`).
  - **`mod.rs` architecture sketch** expanded from a one-line `//!`
    to a 25-line layout/conventions map matching the admin/, mcp/,
    commands/, scaffold/ pattern.
  39 scheduler tests pass (32 unit + 7 integration); clippy clean.

- `src/scaffold/` code-quality cleanup pass. Concrete changes:
  - **Seven inline templates moved to template files.** The module
    already had a Handlebars registry in `render.rs` and per-submodule
    `templates/` folders for `collection/`, `global/`, `hook/`, `init/`,
    `job/`, `migration/` — but six generators still inlined their
    templates as Rust raw-string `format!()` calls. Moved to dedicated
    files and registered:
      - `component/templates/component.js.hbs` (Web Component skeleton)
      - `theme/templates/theme.css.hbs` (CSS theme catalogue)
      - `node/templates/node.lua.hbs` (richtext node registration)
      - `field/templates/field.hbs.hbs` (per-field admin template)
      - `field/templates/plugin.lua.hbs` (Lua plugin wrapper)
      - `page/templates/page.hbs.hbs` (custom admin page)
      - `slot/templates/slot.hbs.hbs` (slot widget)
    Each generator now serializes a small context struct and calls
    `render::render("<name>", &ctx)?` instead of carrying ~50 lines of
    inline `format!(r#"..."#)` literal. The three `.hbs`-output
    templates (page, slot, field's hbs) use Handlebars's `\{{...}}`
    backslash escape to emit literal `{{...}}` sequences in the
    produced file. **Net:** −267 LOC of inline template Rust code,
    +7 hbs files where syntax highlighters work and the template can
    be edited without Rust recompilation.
  - **`super::super::` deep path resolved.** `blueprint/apply.rs`
    reached `super::super::init::LUA_API_TYPES`. Replaced with a
    top-of-file `use crate::scaffold::init::LUA_API_TYPES;`.
  - **Manual `Default` collapsed to `#[derive(Default)]`.**
    `CollectionOptions` had a hand-rolled `new()` (five `false` bools)
    plus a `Default` impl forwarding to `new()`. All callers already
    used `default()` or struct literals. `InitOptions::default` kept
    its manual impl — its defaults are non-trivial (`admin_port:
    3000`, `grpc_port: 50051`, …).
  - **5-arg `templates_extract` → named-field params struct.** New
    `TemplatesExtractParams<'_>`, re-exported at `scaffold::*` for the
    one external caller. Test block grew a small `extract_one(tmp,
    path, force)` helper that compresses six nearly-identical test
    calls into one-liners.
  - **`collection/types.rs` (5 unrelated types in one file) split**
    into per-concept files: `field_types.rs` (`VALID_FIELD_TYPES` +
    `CONTAINER_TYPES` consts), `collection_options.rs`
    (`CollectionOptions`), `stubs.rs` (`FieldStub` + `FieldStubBuilder`
    + `BlockStub` + `TabStub` — kept together because the three stub
    types form a mutually-referential hierarchy via `FieldStub.fields`
    / `.blocks` / `.tabs`).
  - **Duplicated container-type list deduped.** `wizard.rs` carried a
    private `WIZARD_CONTAINER_TYPES = &["group", "array", "row",
    "collapsible"]` const identical to `collection::CONTAINER_TYPES`.
    Wizard now imports the canonical list.
  - **Submodule visibility tightened (12 of 17 modules).** `mod.rs`
    declared all 17 submodules `pub mod` even though the module's
    public API is the flat `scaffold::*` re-export block underneath.
    Demoted to `pub(crate) mod`. The lone external deep-path import
    (`commands::templates::shared` reaching
    `scaffold::templates::EMBEDDED_*`) was rewritten via a new
    `pub(crate) use self::templates::{EMBEDDED_STATIC,
    EMBEDDED_TEMPLATES};` re-export.
  - **`type_specific_stub` demoted from `pub` to private** (used only
    inside `writer.rs`); dropped from the `pub use writer::{...}`
    re-export. Same demotion for `KNOWN_SLOTS` (used only inside
    `slot/generator.rs`).
  - **Duplicated overwrite-guard pattern lifted** to a tiny
    `scaffold/guards.rs::refuse_file_overwrite(path, force)` helper.
    Eleven sites across the `make_*` generators that wrote
    `if file_path.exists() && !opts.force { bail!("File '{}' already
    exists -- use --force to overwrite", path.display()); }` now call
    the helper instead. A typo in the message at one site can no
    longer drift; future scaffold subcommands share the same
    behaviour by default.
  - **ASCII-only scaffolded output.** Swept all non-ASCII characters
    (`—`, `…`, `─`, `→`) out of every `.hbs` / `.tpl` / `.lua`
    template and the `.rs` files that scaffold them — `--`, `...`,
    `=`, `->` respectively. The generated files (`make page`,
    `make slot`, `make field`, `make theme`, `make component`,
    `make node`, `make collection`, …) and the operator-facing CLI
    error messages are now ASCII-only, predictable across terminals
    that don't render UTF-8 reliably.
  - **Magic-string path segments centralized** in a new
    `scaffold/paths.rs`. Eighteen `.join("collections")` /
    `.join("globals")` / `.join("templates").join("pages")` /
    `.join("static").join("components")` etc. site across 11
    generators replaced with named helpers (`paths::collections_dir`,
    `paths::templates_pages_dir`, `paths::static_components_dir`, …).
    The same module also owns the `INIT_SUBDIRS` list — single source
    of truth shared by `init` (which creates the directories) and
    every `make_*` generator (which writes into them). A typo at one
    site can no longer silently break the contract.
  - **`scaffold/mod.rs` architecture sketch** — top-of-module doc
    expanded from a one-liner to a 30-line layout map covering
    submodule conventions (template rendering via `render::*`, slug
    validation rules, why submodules are `pub(crate)`).

  All 207 scaffold tests pass; clippy clean.

- `src/cli/` code-quality cleanup pass. Tiny module (5 files,
  437 LOC), structurally already clean. The one real fix:
  `output.rs` privately resolved the
  `CRAP_NO_UNICODE=1` / `CRAP_FORCE_UNICODE=1` /
  `console::Term::wants_emoji()` cascade for its glyphs, but
  the other two rendering surfaces (`spinner.rs`, `theme.rs`)
  hard-coded the Unicode glyphs (`"✓"`, `"⚠"`, `"✗"`) without
  the fallback. Lifted the resolver to a new `cli/glyphs.rs`
  module with named accessors (`success()`, `warning()`,
  `error()`, `info()`, `prompt()`, `bar()`); each returns the
  Unicode form when the terminal supports it and the ASCII
  fallback otherwise. `output`, `spinner`, and `theme` now
  call through. **Net effect:** `CRAP_NO_UNICODE=1` now
  uniformly forces ASCII across every CLI surface (spinner
  finish messages, dialoguer prompt prefixes, banner glyphs);
  previously only plain-text output respected it. All 20 cli
  tests pass; clippy clean.

- `src/typegen/` code-quality cleanup pass. Module was
  structurally clean to begin with; two files crossed the
  1000-line soft limit (`mod.rs` at 505 LOC and `lua.rs` at
  1146 LOC) and the per-language generators carried duplicated
  boilerplate. Concrete work:
  - `mod.rs` split into four siblings:
    - `language.rs` — `Language` enum + `from_name`/
      `file_extension`/`all`/`label` accessors + 6 colocated
      tests.
    - `helpers.rs` — `to_pascal_case`, `is_optional`,
      `rel_has_many`, `sorted_*_slugs`, `SubTypeKind`,
      `SubTypeField`, `collect_sub_type_fields` + 17 colocated
      tests. `to_pascal_case` re-exported `pub(crate)` at the
      typegen root for cross-module callers
      (`scaffold::{job,hook}::generator`).
    - `dispatch.rs` — file-output entry points (`generate`,
      `generate_lang`, `generate_proto_conversion`) + the
      private `render` per-language match dispatch + the
      `LUA_API_TYPES` const.
    - Resulting `mod.rs`: 40 LOC of declarations + re-exports +
      30-line architecture doc.
  - `lua.rs` (1146 LOC, the only language file over the 1000
    soft limit; the other 5 are 742-914 LOC) split into a
    `lua/` folder:
    - `lua/mod.rs` — declarations + `pub(super) use render`
      + shared `#[cfg(test)] pub(super) mod test_helpers`
      (`text_field`, `select_field`, `checkbox_field`).
    - `lua/render.rs` (760 LOC) — top-level `render` entry +
      `render_template_data_types` + `render_collection` +
      `render_global` + `render_find_overloads` + 17 colocated
      render-level tests.
    - `lua/field.rs` (343 LOC) — `write_field` +
      `field_to_lua_type` + 22 colocated field-level tests.
    - 1 duplicate test (`to_pascal_case_basic`) dropped — the
      same coverage exists in `helpers.rs`.
  - Per-language sub-files (`typescript.rs`, `go.rs`,
    `python.rs`, `rust_types.rs`, `rust_proto.rs`) updated to
    `use super::helpers::{…}` (the helpers' new home) instead
    of going back through `crate::typegen::{…}`.
  - **Centralized 196 sites of duplicated boilerplate.** Every
    per-language generator had repeated calls of the shape
    `writeln!(out, …).expect("write to String")`. `rust_proto.rs`
    already had a private `w!` macro for this; the other five
    files did not. Lifted that macro to `helpers.rs` (two arms:
    `w!(out)` for blank lines, `w!(out, fmt, args…)` for
    formatted) and converted all 196 sites in `typescript.rs`,
    `go.rs`, `python.rs`, `rust_types.rs`, `lua/render.rs`,
    `lua/field.rs`. The macro brings `std::fmt::Write` into a
    local block scope so callers no longer need their own
    `use std::fmt::Write;` — deleted from 7 files. **Net:**
    −196 boilerplate lines, +1 shared macro.
  All 175 typegen tests pass; clippy + full lib suite clean.

- `src/commands/` code-quality cleanup pass. Module was in
  good shape to start with; one file crossed the 1000-line
  soft limit and visibility had drifted. Concrete work:
  - `templates.rs` (1079 LOC, 7 public actions + ~10 helpers
    + 11 colocated tests) split into a folder per the
    "one file per `crap-cms templates <action>` subcommand"
    rubric the user articulated: `templates/{list,extract,
    status,layout,diff,shared}.rs` plus `mod.rs` (re-exports).
    `status.rs` keeps the `customization_counts` /
    `CustomizationCounts` API that `commands::status::display`
    consumes, alongside the `Drift` enum and overlay walker.
    `layout.rs` carries the 23 `EXACT_LAYOUT_MOVES` entries
    plus `LayoutKind` + `LayoutEntry` + classifier helpers.
    `diff.rs` keeps `print_unified_diff` and its tests.
    `shared.rs` holds `split_kind`, `lookup_embedded`, and
    `CRATE_VERSION` — used by both `status.rs` and `diff.rs`.
    Each destination got the colocated tests that exercise
    its own functions: 4 to `status.rs`, 5 to `layout.rs`,
    2 to `diff.rs`. Largest resulting file is `layout.rs` at
    563 LOC (the layout move tables dominate).
  - Visibility tightening. Sweep across the eight subcommand
    subdirs found 54 `pub fn` / `pub struct` / `pub enum`
    items with no external callers outside their own subdir;
    demoted to `pub(super)`. Four `pub use` re-exports in
    `make/mod.rs`, `db/mod.rs`, `user/mod.rs` pointed at these
    newly-private items but were themselves never imported
    externally — deleted along with the demotion. Items
    affected: `try_load_registry`, `find_orphan_columns`,
    `user_verify`, `user_unverify`. Also deleted the unused
    `cache_path` fn in `update/mod.rs` (its doc comment said
    "Exposed for tests" but no test ever referenced it).
  - Closure-to-fn-pointer polish: 4 sites in `user/modify.rs`
    converted from `.map_err(|e| e.into_anyhow())` to
    `.map_err(ServiceError::into_anyhow)`, matching the
    api/ + mcp/ passes.
  - Five wide-arg fns refactored to named-field parameter
    structs (a named-field struct reads at a glance; counting
    to position 5 in a positional call does not):
      - `user_create` (7 args → `UserCreateParams`)
      - `user_change_password` (7 → `UserChangePasswordParams`)
      - `user_delete` (6 → `UserDeleteParams`)
      - `run_purge` (6 → `PurgeParams`, private to trash.rs)
      - `write_backup_manifest` (6 → `WriteManifestParams`,
        private to db/backup.rs)
    Both call sites in `init.rs` and the ~10 sites in
    `tests/cli_commands*.rs` updated to struct-literal
    construction.
  - Promoted user-management entry points (`user_create`,
    `user_change_password`, `user_delete`, `user_list`,
    `user_lock`, `user_unlock` and their `*Params` structs)
    to top-level `commands::*` re-exports. Test files that
    previously wrote `commands::user::user_create(commands::
    user::UserCreateParams { … })` now write the flat form.
  - Tightened import paths across `commands/`:
    `chrono::Local::now()` → `Local::now()` (db/backup.rs);
    `chrono::Utc::now()` → `Utc::now()` (serve/startup.rs);
    `crate::commands::update::cache::*` → `update::cache::*`
    via a top-of-file `commands::update` import
    (serve/startup.rs); `std::sync::OnceLock::new()` →
    `OnceLock::new()` (3 sites);
    `std::collections::BTreeMap` → `BTreeMap`. `serde_json::*`
    and `tokio::*` paths intentionally stay qualified — the
    prefix carries semantic meaning (`serde_json::to_string`
    reads as "JSON serialize", `tokio::spawn` as "async
    runtime spawn") and CLAUDE.md explicitly exempts these.
  Additionally: `commands/mod.rs` got a 30-line architecture
  sketch (layout, entry-point convention, cross-cutting
  helpers, visibility convention) matching the admin/ + mcp/
  module-doc pattern.

- `src/mcp/` code-quality cleanup pass.
  - **Structural cleanup.** All 7 `#[allow(clippy::too_many_arguments)]`
    escapes scattered across `tools/collection/{write/{create,
    update, delete, delete_many, update_many}, versions}.rs` and
    `tools/dispatch.rs::execute_tool` resolved at the root cause.
    New `tools/exec_ctx.rs` defines a single
    `ToolExecCtx<'a> { registry, pool, runner, config,
    event_transport, invalidation_transport, cache }` bundle that
    every CRUD-style `exec_*` fn now takes as its third argument
    (after the per-call `args` and `slug`). Each tool fn went
    from 6–9 positional params to a flat `(args, slug, ctx)`
    signature. The existing `UnpublishParams` ad-hoc struct
    collapsed into the same shape and was deleted along with its
    re-export. `dispatch::execute_tool` now constructs nothing
    inline — it pattern-matches `ToolOp` and dispatches each
    branch with `(args, slug, ctx)`. The two callers of
    `execute_tool` (`mcp::server` and the test sites in
    `tools/dispatch.rs`'s colocated `mod tests`) build the
    `ToolExecCtx` once via a shared `make_exec_ctx` helper.
    File splits to bring `tools/schema/introspection.rs` (1472
    LOC) under the 1000-LOC soft limit: split into four
    sibling files by purpose — `field_types.rs`,
    `list_collections.rs`, `describe_collection.rs`, and
    `cli_reference.rs`. The CLI-reference file then split
    further: types + dispatch fn stay in `cli_reference.rs`
    (~240 LOC), the 23 `CLI_DETAIL_*` static command
    descriptions live in a sibling `cli_details.rs` (~945 LOC)
    so each file individually clears the soft limit. Test
    colocation: the monolithic `tools/tests.rs` (922 LOC, 64
    tests) deleted; tests redistributed into per-tool
    `#[cfg(test)] mod tests` blocks beside the function they
    exercise — 24 tests to `dispatch.rs`, 18 to
    `collection/helpers.rs` (parse_where_filters + doc_to_json),
    10 to `schema/config_files.rs`, 5 to
    `schema/describe_collection.rs`, 3 each to
    `schema/list_collections.rs` and `schema/cli_reference.rs`,
    1 to `schema/field_types.rs`. Shared fixtures
    (`make_registry`, `make_exec_ctx`) extracted to a new
    `tools/test_helpers.rs` reachable by every colocated test
    block.
  - **Pattern parity with admin/ and api/.** Most of those
    patterns were already absent here — mcp tools are sync, so
    no `spawn_blocking` antipatterns; no `if let Some(...) {
    builder.x(Some(x)) }` redundant wrappers at builder call
    sites; no `match registry.get(slug)` fallthroughs to
    convert; no `.map_err(|e| { error!(...); X })`
    log/transform mixes. Four `.map_err(|e| e.into_anyhow())`
    closures (in `find.rs`, `find_by_id.rs`, `count.rs`,
    `globals/get.rs`) converted to function-pointer form
    `.map_err(ServiceError::into_anyhow)` for parity.
  - **Visibility + dead code.** Every `pub mod` under `mcp/`
    (protocol, resources, schema,
    server, stdio, tools) demoted to `pub(crate) mod` after
    confirming via grep that no external callers reach in.
    Top-level re-exports added for the actual external API
    surface: `McpServer` (already exported), `run_stdio` (now
    `mcp::run_stdio` — `commands/mcp.rs` updated from
    `mcp::stdio::run_stdio`), and the JSON-RPC types used by
    `admin::mcp_handler` (`JsonRpcRequest`, `JsonRpcResponse`,
    `JsonRpcError`, plus the `INTERNAL_ERROR` /
    `INVALID_REQUEST` / `PARSE_ERROR` constants —
    `admin::mcp_handler` updated from `mcp::protocol::*` to
    `mcp::*`). Crate-internal `tools` re-exports trimmed to
    just the three names actually consumed (`execute_tool`,
    `generate_tools`, `should_include`); `ParsedTool`, `ToolOp`,
    `parse_tool_name` were exposed but never imported outside
    of `mcp::tools` itself, so they're back to module-private.
    Dead code: `protocol::ToolResultContent` struct (defined
    but never constructed) deleted. `InitializeParams` /
    `ClientInfo` flagged with dead-field warnings —
    investigated against the MCP spec to decide the right
    treatment. The `initialize` request mandates
    `protocolVersion`, `capabilities`, `clientInfo.name` per
    spec, but the server is only obligated to *echo back* its
    own version+capabilities — not to act on the client's
    declared ones. To make the fields genuinely live (no
    `#[allow(dead_code)]` shortcuts), `handle_initialize` now
    emits a single diagnostic `info!` line on each handshake:
    `MCP initialize: client=<name>/<version>
    protocol=<version> capabilities=<json>`. This gives
    operators visibility into which integrations connect with
    which protocol/feature flags, and incidentally makes every
    spec-modeled field a production read.
  - **Per-call audit trail.** The 10 `info!("MCP <op> ...")`
    lines on write tools were already deliberate — MCP is the
    one transport whose caller is a model, so "what did Claude
    do to my data" is a real operational question that justifies
    layered success logging here (api/ has none, admin/ has one).
    The lines used to be unstructured and didn't say *which*
    client made the call. Plumbed the
    client identity through: new
    `McpServer::client_name: OnceLock<String>` populated by
    `handle_initialize` from `params.client_info.name`, and
    new `McpServer::transport_label: &'static str` set at
    construction by each transport runner — `(stdio)` for
    the long-lived stdio process, `(http)` for the
    per-request HTTP handler, `(test)` for unit tests. Parens
    on the fallback labels disambiguate them from a real
    client that happens to be named "stdio". `handle_tools_call`
    resolves an `audit_label()` (client name when known,
    transport label otherwise) and passes it through
    `ToolExecCtx::client_label`. Every audit `info!` now ends
    with `[client=<label>]`. Stdio sees the actual client name
    after `initialize`; HTTP shows `(http)` until session-id
    tracking lands (separate work — `Mcp-Session-Id` header
    +  `AdminState`-level session map). The
    `exec_write_config_file` static tool also takes
    `client_label` directly since its dispatch path doesn't go
    through `ToolExecCtx`.

- `src/admin/` code-quality cleanup, first pass: test
  colocation. The three monolithic sibling-file test
  modules (`context/field/tests.rs` 626 LOC,
  `handlers/field_context/builder/tests.rs` 1133 LOC,
  `handlers/field_context/enrich/tests.rs` 1854 LOC) are gone.
  Each test is now in a `#[cfg(test)] mod tests` block at the
  bottom of the source file that owns the function it exercises:
  - `context/field/tests.rs` → split by variant family across
    `base.rs` (3 base-data tests + the shared `make_base()`
    fixture in a new `test_helpers.rs`), `scalars.rs` (12 tests
    for text/textarea/number/code/richtext/date/select/checkbox),
    `refs.rs` (5 tests for relationship/upload/join),
    `composites.rs` (8 tests for group/collapsible/row/tabs/
    array/blocks), and `mod.rs` (the enum-tagging test).
  - `handlers/field_context/builder/tests.rs` → 36
    `build_field_contexts_*` tests moved into
    `builder/context.rs` (alongside the production fn);
    `safe_template_id_*`, `split_sidebar_fields_*`, and
    `count_errors_*` tests joined the existing
    `field_context/helpers.rs` test module. Shared fixtures
    (`make_field`, `fields_from_json`, the `Vec<Value>`-returning
    `build_value_contexts` wrapper) live in a new
    `field_context/test_helpers.rs`. The 4 `split_sidebar_fields`
    tests now exercise the production fn through
    `fields_from_json` instead of a parallel Value-based partition
    impl that the old test file kept as scaffolding (`#[allow(
    dead_code)] split_sidebar_field_contexts` deleted).
  - `handlers/field_context/enrich/tests.rs` → 30
    `enriched_sub_field_*` + `enrich_nested_fields_*` tests moved
    into `enrich/nested.rs`; 5 `enrich_field_contexts_*` /
    `*_transparent_names` tests into `enrich/enrichment.rs` (with
    the two tests that inlined `make_test_state` rewritten to
    call the shared helper, ~110 LOC each → 1 line); 7
    `enrich_richtext_*` tests + the `make_cta_registry` fixture
    into `enrich/enrich_types.rs`; the 3
    `collect_node_attr_errors_*` tests joined the
    `field_context/helpers.rs` test module (where the production
    fn lives). `enrich/test_helpers.rs` houses the
    sqlite-feature-gated wrappers (`build_enriched_sub_field_value`,
    `enrich_field_contexts_values`, `enrich_nested_fields_values`,
    `enrich_richtext_value`, `make_test_state`) — same gating the
    monolithic file had. The duplicated
    `max_depth_prevents_infinite_recursion` test (one copy in
    each of the two old test files) is now a single test in
    `builder/context.rs`.
- Test-file file-size soft limit deliberately broken on
  `enrich/nested.rs` (1518 LOC after split). Per CLAUDE.md the
  1000-line cap is a soft limit; respecting it would have meant
  either keeping a sibling `tests.rs` indirection or fragmenting
  `nested.rs` into smaller per-test-topic source files for no
  source-readability gain. Strict colocation — function visible
  alongside its tests — won the trade. Other files stay under
  1000 LOC even with their tests folded in.
- `src/admin/` cleanup, second pass: structural cleanup. All five
  `#[allow(...)]` escapes resolved at root cause:
  - `mod.rs::AdminState` `dead_code` allow was a stale legacy
    blanket; every field is read.
  - `templates/helpers/translation.rs::TranslationHelper`
    `dead_code` allow was stale — the struct is constructed at
    `helpers::register_helpers` and its field is read in
    `call_inner`.
  - `handlers/collections/items/empty_trash.rs::empty_trash`
    `clippy::too_many_arguments` (10 args) replaced with
    `EmptyTrashInput<'_>` typed input struct.
  - The remaining two were on test files that no longer exist
    (deleted in the test-colocation pass).
- `enrich/` builders colocated with their structs, and the
  three-struct `enrich/context.rs` decomposed: `enrich_options.rs`
  holds `EnrichOptions` + `EnrichOptionsBuilder`; `sub_field_opts.rs`
  holds `SubFieldOpts` + `SubFieldOptsBuilder`; `enrich_ctx.rs`
  holds the module-internal `EnrichCtx`. The orphaned
  `enrich_options_builder.rs` / `sub_field_opts_builder.rs` /
  `context.rs` files are gone. Builders dropped from the module's
  re-export surface (callers reach them via `Type::builder()`).
- `EnrichOptionsBuilder::doc_id` now takes `Option<&'a str>`
  to match the existing `Option<&...>` setters on the same
  builder. The one call site that did
  `if let Some(id) = p.doc_id { enrich_opts = enrich_opts.doc_id(id); }`
  collapses to `enrich_opts.doc_id(p.doc_id)`.
- A `super::super::MAX_FIELD_DEPTH` chain in
  `enrich/field_types.rs` rewritten to use the existing
  field_context-level import. Zero `super::super::` chains
  remain in `src/admin/`.
- Visibility tightening. At `admin/mod.rs`:
  `csp_nonce` demoted from `pub mod` to `mod` (the `pub use
  csp_nonce::{...}` re-exports cover the public surface);
  `context` and `server_builder` demoted to `pub(crate) mod`. At
  `admin/handlers/mod.rs`: ten of eleven submodules demoted to
  `pub(crate) mod` (`forms` stays `pub` because it has cross-crate
  consumers). Stale re-exports dropped: `AdminMeta`, `AuthMeta`,
  `UploadMeta`, `FieldAdminMeta`, `LocaleTemplateOption`,
  `NavCollection`, `NavGlobal` from `context/mod.rs` (only
  internal `schema_doc.rs` referenced the last three, and it now
  reaches them via deep path); `PaginationParams` /
  `SearchQuery` re-exports from `handlers/collections/mod.rs`.
- Genuine dead code deleted: `PageMeta::with_breadcrumbs`
  + its test (handlers use `BasePageContext::with_breadcrumbs`
  which writes both `self.breadcrumbs` and `self.page.breadcrumbs`
  — the `PageMeta`-level helper was redundant and never called
  from production code); `FieldContext::field_type_str` (zero
  callers); `MfaQuery` struct + `Query<MfaQuery>` extractor in
  `mfa_page` (collection slug travels through the
  `crap_mfa_pending` JWT cookie, not via the URL query string —
  full flow trace verified). `FieldContext::to_value` gated to
  `#[cfg(test)]` because only test code uses it.
- Workspace-split prep: the only two cross-module
  imports into `admin` (`api::upload` and `service::upload`
  pulling `parse_multipart_form` and `extract_join_data_from_form`
  from `admin::handlers::forms`) are now top-level `crate::admin::Foo`
  imports via a `pub(crate) use handlers::{...}` re-export at
  `admin/mod.rs`. Zero `crate::admin::<sub>::*` deep paths from
  outside `src/admin/`. Promotion stays `pub(crate)` since both
  callers live in this crate; a future workspace split flips it
  to `pub`.
- `src/admin/` cleanup, third pass: additional improvements:
  - **Cookie-name constants**: 16 raw `"crap_session"` /
    `"crap_session_exp"` / `"crap_mfa_pending"` / `"crap_csrf"` /
    `"crap_editor_locale"` literals across `auth/session.rs`,
    `auth/mfa.rs`, `auth_middleware.rs`, `server.rs`,
    `uploads/serve.rs`, and `shared/locale.rs` hoisted to
    `pub(in crate::admin) const`s in `auth/session.rs`. A typo at
    one site can no longer silently break auth — every cookie
    write and read goes through the same constant.
  - **`service_error_to_admin_response` + `task_join_error_response`**
    helpers added to `handlers::shared::response`. The
    `ServiceError` variant matching that 4 admin handlers
    (`globals/edit_form`, `collections/items/list`,
    `collections/item/edit_form`, plus more) duplicated inline
    now collapses to a one-liner. The 403/500 page rendering and
    the `error!` log of underlying details live in one place.
    Mirrors the JSON-returning `service_error_to_response` that
    `api/upload` already uses; the two are domain-shaped (HTML
    vs JSON) so they stay separate functions.
  - **`paths::*` migration completion**: the existing
    `handlers::shared::paths` helpers covered ~36% of admin URL
    construction; raw `format!("/admin/...")` and string literals
    handled the rest. New helpers (`paths::LOGIN`,
    `paths::COLLECTIONS_ROOT`, `paths::login_with_success(key)`,
    `paths::collection_item_versions_page(slug, id, page)`,
    `paths::collection_item_version_restore(slug, id, version_id)`)
    + ~12 call-site rewrites bring the migration to ~95%. The
    remaining literals are all axum route definitions in
    `server.rs` (route patterns, not URL builders) and test
    fixtures.
  - **`registry.get_collection` let-else conversion**: 14 sites
    used `match state.registry.get_collection(&slug) { Some(d) =>
    d.clone(), None => return X }`. Converted to
    `let Some(def) = state.registry.get_collection(&slug).cloned()
    else { return X };` (Rust 1.65+ let-else, idiomatic).
    4-line block → 3-line block, happy path no longer indented
    inside a match arm. CLAUDE.md "Prefer early returns over
    nesting" applied uniformly.
  - **`AdminState::mcp_server` helper**: the 8-field manual splat
    in `mcp_handler::mcp_http_handler` (`pool: state.pool.clone()`,
    `registry: state.registry.clone()`, …) collapsed to
    `state.mcp_server()`. Mirrors `AdminState::email_context()`
    from the service/ pass; a future workspace split won't have
    to re-derive the same plumbing.
  - **Spawn-blocking body extraction**: 8 `spawn_blocking(move ||
    { … })` closures with multi-statement bodies (build
    `ServiceContext`, call service fn, sometimes do follow-up
    work) extracted to named `*_blocking` functions per CLAUDE.md.
    Each gets a typed `*BlockingInput` struct bundling the owned
    captures: `RestoreVersionInput`, `RestoreGlobalVersionInput`,
    `UndeleteInput`, `UpdateBlockingInput`, `CreateBlockingInput`,
    `DeleteBlockingInput`, plus the simpler
    `check_admin_access_blocking`,
    `check_upload_access_blocking`,
    `verify_credentials_blocking`,
    `verify_mfa_blocking`,
    `run_auth_strategy_blocking`. Closure bodies are now
    single-fn-call shaped throughout `src/admin/`.
  - **`admin/mod.rs` architecture sketch**: top-of-module doc
    expanded from one line to a short architecture map covering
    the submodule layout, cross-module conventions (cookies,
    URLs, error response, spawn-blocking), and `AdminState`
    plumbing. Anchors newcomers without forcing them to
    reverse-engineer the layout.
- `src/api/` code-quality cleanup pass. Module started in good
  shape (zero `super::super`, zero deep-path imports from
  outside, zero manual `Default` impls, all files under 1000
  LOC). Concrete changes:
  - **Structural cleanup.** Sole `#[allow(dead_code)]` on
    `ContentService` removed by tracing the one truly-dead
    `jwt_secret` field through the data flow: it was set by both
    `ContentServiceDeps` and `GrpcStartParams` but never read.
    The actual JWT operations all flow through `token_provider`
    (a `SharedTokenProvider` constructed externally from the same
    secret), so the duplicate field was vestigial. Removed from
    `ContentService`, `ContentServiceDeps`,
    `ContentServiceDepsBuilder` (field + setter), `GrpcStartParams`,
    `GrpcStartParamsBuilder`, plus the 23 `.jwt_secret(...)`
    setter calls scattered across 14 integration tests in
    `tests/`. Builder colocation: `ContentServiceDeps` +
    `ContentServiceDepsBuilder` collapsed into a new
    `handlers/content_service_deps.rs` (the struct previously
    lived in `handlers/mod.rs`, violating CLAUDE.md's
    "mod.rs files should contain no business logic"); the
    builder's old `handlers/deps_builder.rs` deleted.
    `GrpcStartParams` + `GrpcStartParamsBuilder` collapsed into
    `server.rs`; `server_builder.rs` deleted. Top-level
    `pub use server_builder::GrpcStartParamsBuilder` re-export in
    `api/mod.rs` removed (builders are reached via
    `Type::builder()`, not separate import). `pub mod
    rate_limit` demoted to `pub(crate) mod` (no external
    consumers).
  - **Pattern parity with the `admin/` cleanup.** Four
    `match registry.get_collection(&slug) { Some(d) => d.clone(),
    None => return X }` sites converted to
    `let Some(def) = ....cloned() else { return X };` (Rust 1.65+
    let-else). Twenty-one `spawn_blocking(move || { … })`
    closures with multi-statement bodies extracted to named
    `*_blocking` functions taking typed `*BlockingInput` structs
    bundling the owned captures, per CLAUDE.md's "the closure
    should be a single function call" rule. Per-site structs:
    `TriggerJobBlockingInput`, `ListJobRunsBlockingInput`,
    `GetJobRunBlockingInput`, `CountBlockingInput`,
    `FindBlockingInput`, `FindByIdBlockingInput`,
    `ListVersionsBlockingInput`, `RestoreVersionBlockingInput`,
    `CreateBlockingInput`, `UpdateBlockingInput`,
    `DeleteBlockingInput`, `UndeleteBlockingInput`,
    `UnpublishBlockingInput`, `ValidateBlockingInput`,
    `CreateManyBlockingInput`, `UpdateManyBlockingInput`,
    `DeleteManyBlockingInput`, `GetGlobalBlockingInput`,
    `UpdateGlobalBlockingInput`, `MeBlockingInput`,
    `LoginBlockingInput`, `VerifyEmailBlockingInput`,
    `ResetPasswordBlockingInput`, `UploadCreateBlockingInput`,
    `UploadUpdateBlockingInput`, `UploadDeleteBlockingInput`,
    `ResolveSubscribeAccessBlockingInput`. The four
    `account.rs` action sites (`lock`/`unlock`/`verify`/
    `unverify`) DRY'd via a single `account_action_blocking`
    helper that takes a `fn(&ServiceContext, &str) ->
    Result<(), ServiceError>` action pointer + a shared
    `AccountActionBlockingInput`, plus an
    `account_action_input` constructor method that toggles
    `invalidation_transport` for the lock-only flow.
  - **gRPC error mapping** — nothing new to add. The existing
    `From<ServiceError> for Status` impl in
    `handlers/collection/error_mapping.rs` already covers every
    variant with the right gRPC status code (regression tests
    pin the `UniqueViolation`→`AlreadyExists` and
    `InvalidToken`→`Unauthenticated` mappings). All 23
    `Status::from(ServiceError::classify(...))` /
    `Status::from(e.reclassify(...))` call sites use this impl
    — no inline matching to consolidate.
  - **Builder Option-symmetry.** `account_action_blocking` had
    one residual
    `if let Some(transport) = input.invalidation_transport
    { builder = builder.invalidation_transport(Some(transport)); }`
    from the lock-only special case. Since
    `ServiceContextBuilder::invalidation_transport` already
    takes `Option<SharedInvalidationTransport>`, the wrapper was
    redundant — flattened to `.invalidation_transport(input
    .invalidation_transport)` in the build chain. The other 11
    `.invalidation_transport(Some(...))` / `.cache(Some(...))` /
    `.event_transport(Some(...))` call sites in the codebase
    were verified to source from non-Option values where the
    `Some(_)` wrap is intentional, not a violation.
  - **gRPC blocking-fn return types.** `reset_password.rs` /
    `verify_email.rs` /
    `me.rs` / `login.rs` blocking fns previously returned
    `Result<_, anyhow::Error>` and the call site did the work
    of converting to `Status` via a second `.map_err(|e| {
    error!(...); Status::internal(...) })` after the
    JoinError-to-Status `.map_err(...)?`. Each blocking fn now
    returns `Result<_, Status>` directly — `pool.get()` /
    `conn.transaction()` / `tx.commit()` failures map to
    `Status::internal` inline (with `inspect_err` for the
    log side-effect, `map_err` only for the type transform),
    and `ServiceError`-returning service calls map via
    `.map_err(Status::from)` so they pick up the proper variant
    from `error_mapping::From<ServiceError> for Status` instead
    of being collapsed to a generic 500 (incidental fix:
    `verify_email` and `update_global_document` used to surface
    every `ServiceError` as 500 internal — they now map to
    their semantic gRPC variant). Call sites use the standard
    `??` pattern matching the rest of the api/ tree.
  - **`inspect_err` / `map_err` separation, codebase-wide.**
    Logging is a side effect and shouldn't live inside the
    closure that transforms the error type. Twenty additional
    sites across api/ + admin/ + commands/
    were converted from `.map_err(|e| { error!("...", e);
    SomeReturnError })` to `.inspect_err(|e| error!("...", e))
    .map_err(|_| SomeReturnError)`. Files touched:
    `api/handlers/{auth/login, jobs/{trigger,get_run,list_runs,
    list}, globals/{get,update}, collection/versions/{list,
    restore}, content_service, subscribe}.rs`,
    `admin/handlers/shared/access.rs`, and
    `commands/serve/startup.rs` (where the closure was a no-op
    `|e| { error!(...); e }` — collapsed to `.inspect_err(...)?`
    with no map_err at all).

- Continued the alpha.8 admin-context typing work into the rest of
  the app: audited every non-admin `serde_json::Value` /
  `HashMap<String, Value>` usage and typed the cases with a
  compile-time shape — email template contexts, webhook payload,
  backup manifest, upload HTTP API responses, image-sizes nested
  structure, MCP collection tool responses, MCP field-type table.
  The remainder is genuinely dynamic — user document fields, the
  gRPC proto bridge, JSON-RPC envelopes, JSON Schema output,
  user-supplied filter/validation values, the Lua hook context bag —
  and stays as `Value`.
- `core/` module restructured for colocation. Every struct that has a
  builder now lives in a single file with its builder and tests next
  to it (claims, document, version_snapshot, field_admin,
  field_definition, collection_definition, global_definition,
  job/definition, richtext_node_def). The orphaned `*_builder.rs`
  pair-files are gone. `JobRunBuilder` moved from `job/definition.rs`
  into `job/run.rs` next to `JobRun`. No public-API change.
- `core/collection/shared.rs` (269 LOC, 9 types) decomposed into
  per-concept files: `access.rs`, `hooks.rs`, `admin_config.rs`,
  `labels.rs` (also home to the `resolve_label` helper, with new
  unit tests), `mcp_config.rs`, `versions_config.rs`, `live.rs`
  (`LiveSetting` + `LiveMode`), `index_definition.rs`. `shared.rs`
  is gone.
- `core::` top-level re-export surface tightened for consistency.
  `GlobalDefinition`, `RichtextNodeDef`, `JobDefinition`, `JobRun`,
  `JobStatus`, `JobLabels`, `FieldAccess`, `FieldHooks`,
  `McpFieldConfig`, `FieldError`, and `ValidationError` now reachable
  as `crate::core::*` without going through their submodule. Builders
  remain `pub` and continue to be reached via `Type::builder()`.
- Removed every `#[allow(...)]` escape hatch from `core/`. The
  remaining warnings the audit surfaced were addressed at the source
  (typed param structs replacing `unused_variables` markers in
  `core/rate_limit/factory.rs`, dead code that turned out to be live,
  fields that were actually used in 5+ places).
- Replaced four panic-on-missing-required builders with plain structs
  + struct-literal construction since the builders' only value was
  ceremony: `UploadedFile`, `ProcessedUpload`, `QueuedConversion`,
  `SizeResult`. Builders that aid DX (chained construction with
  defaults) are kept.
- `queue_email` reduced from 7 → 3 args via `EmailJobData` payload
  struct (`to`, `subject`, `html`, `text`) and `EmailConfig` for
  retry/queue-name policy. Per-call `retries` override flows through
  the captured `EmailConfig` clone in the Lua hook layer without
  changing the function signature.
- `save_resized_image` reduced from 7 → 2 args via
  `SaveResizedImageInput` builder.
- `core/event/mod.rs` (376 LOC) split into `types.rs`, `receiver.rs`,
  `transport.rs`, and `sequence.rs`. `mod.rs` is now 35 lines of
  declarations + re-exports.
- Qualified-path cleanup in `core/`: no more `super::super::*`
  chains, no `crate::core::field::FieldTab`-style re-export
  re-traversals, no inline `use` statements inside function bodies.
  All imports use the shortest available path per CLAUDE.md.
- Typed `serde_json::Value` write pipeline end-to-end. Previously,
  scalar write data was stringified at every entry point
  (`prost_struct_to_hashmap` for gRPC, form parsers for admin) and
  re-parsed back into `DbValue` at the DB layer — losing precision
  for typed numeric inputs (gRPC `int64` rounded through `f64`) and
  conflating `null` with empty string. Now typed values flow from
  every entry boundary (`prost_struct_to_json_map` for gRPC,
  `service::values_from_strings` adapter at the form/admin boundary,
  Lua's already-typed bridge, MCP's `extract_data_from_args`) all
  the way through `WriteInput`, `service::persist::*`,
  `query::create`/`update`/`update_global`, `set_array_rows`, and
  `coerce_json_value`. The dead `prost_struct_to_hashmap` shim is
  deleted.
- `core::db::query::coerce_json_value` rewritten to dispatch on
  `field_type` first (was: dispatch on `Value` variant first). The
  old shape produced `Integer(1)` for a `Bool(true)` reaching a
  `Text` field — the typed-pipeline rework caught this; covered by
  16 cross-type tests in `core::db::query::helpers::tests`. The
  function is now live (called from every typed write); its
  `#[allow(dead_code)]` is gone.
- `WriteInput.data` and `WriteInput.join_data` merged into a single
  `data: DocumentFields` field. The split was historical — `data` was
  stringified columns, `join_data` was typed arrays/blocks/has-many.
  Now both flow through one typed map; the internal dispatch by
  `field.field_type` (column vs join table) happens inside
  `query::create`/`save_join_table_data`.
  `service::persist::create`/`persist_update`/`persist_bulk_update`
  signatures simplified — single `data` arg replaces the
  `(final_data, hook_data)` pair. `build_hook_data` helper deleted
  (the merge it performed is now upstream). `strip_denied_fields`
  reduced from `(denied, &mut data, join_data) -> Cow<...>` to
  `(denied, &mut data)` — pure mutation, no return.
- New `crate::core::DocumentFields` newtype around the
  `HashMap<String, Value>` shape that user-defined document fields
  travel through. Distinct at the type level from the
  identically-shaped `ReqContext` (per-request hook scratchpad), so
  the two can no longer be mixed up at boundaries even though they
  serialize the same way. Replaces `HashMap<String, Value>` in
  `Document.fields`, `WriteInput.data`, `HookContext.data`,
  `MutationEvent.data` / `MutationEventInput.data` /
  `PendingEvent.data`, `DocumentRef.data`, the `query::create` /
  `query::update` / `update_global` write APIs, the persist layer
  (`persist_create` / `persist_update` / `persist_version`),
  collection bulk service ops (`CreateManyItem.data`, `update_many`,
  `delete` via empty payload), the read-write hook runner
  (`PublishEventInput.data`, `run_before_broadcast`,
  `fire_before_read`, `apply_after_read_for_event`,
  `check_access`/`check_live_setting`), the reference-count helpers
  (`ref_count::after_create_from_data`, `data_touches_refs`,
  `lock_ref_targets_from_data`, `compute_refs_from_data`), the
  version-snapshot extractor (`extract_snapshot_data`,
  `collect_join_data_from_snapshot`), the join-save writer
  (`save_join_table_data`), the upload helpers
  (`delete_upload_files`, `publish_upload_event`), the admin
  `validate` handler (`ValidateRequest.data`,
  `flatten_document_values`), `extract_join_data_from_form`, the
  `field_context::enrich/*` doc-field params, the API
  `extract_auth_password` helper, the bench helpers
  (`resolve_bench_data` return, `to_string_map`,
  `randomize_unique_fields`, `generate_synthetic_data`), the
  in-memory `FilterClause` evaluator
  (`filter::memory::matches_constraints` /
  `matches_filter`, used by SSE + gRPC Subscribe), the after-read
  field-hook execution path (`run_field_hooks_inner` /
  `run_field_hooks_recursive` / `run_single_field_hook` /
  `call_field_hook_ref`), and the populate-cache view
  (`PopulatedRef.fields`). Derives `Default`, `Serialize`,
  `Deserialize`, and `JsonSchema` with `#[serde(transparent)]` +
  `#[schemars(transparent)]` so wire format and OpenAPI/MCP schemas
  are byte-identical to the prior `HashMap`. Implements `Deref` /
  `DerefMut` to `HashMap<String, Value>`, `From<HashMap>` /
  `Into<HashMap>`, `IntoIterator` / `FromIterator` / `Extend`, plus
  `get_str` / `get_bool` / `get_i64` / `get_f64` typed accessors.
  Builders accept `impl Into<DocumentFields>` so existing call sites
  that already had a `HashMap` keep compiling. Sites that are
  semantically *not* document fields — richtext node attrs (Prosemirror
  `data-attrs`), array/blocks sub-rows in join tables, the generic
  `lua_table_to_json_map` Lua→JSON adapter, the generic
  `hashmap_to_lua` marshaller (called for both `DocumentFields` and
  `ReqContext` via Deref), the protobuf wire-format
  `prost_struct_to_json_map` boundary, and the 3-context
  `run_validate_function_inner` validator helper (richtext / sub-row
  / doc) — stay as `HashMap` to preserve their genuine shape
  ambiguity.
- `core::db::query::helpers::extract_snapshot_data` returns
  `DocumentFields` (was `String`). Version-restore path preserves
  typed precision now. `snapshot_val_to_string` helper deleted —
  was only used to flatten typed snapshot values into strings before
  reparse.
- `service::types::values_from_strings(map: HashMap<String, String>)
  -> HashMap<String, Value>` is the canonical boundary adapter for
  the form-input path (HTML forms genuinely produce strings; this
  wraps each value in `Value::String`). Lives in
  `service/types/write_input.rs` next to `WriteInput`.
- Removed every `#[allow(...)]` escape hatch from `db/`. The two
  `too_many_arguments` markers in `db/query/filter/resolve.rs` are
  gone: `resolve_array_filter`, `resolve_blocks_filter`, and
  `resolve_relationship_filter` now share a single typed
  `SubFilterCtx<'_>` input struct built once in `resolve_filter`.
  The 8-arg `entry()` test helper in `db/query/images.rs` is replaced
  with a `default_entry()` returning `NewImageEntry<'static>`, with
  per-test overrides via struct-update syntax. The
  `cfg_attr(not(feature = "postgres"), allow(dead_code))` on
  `DbPool::from_backend` is replaced with a real `cfg(feature =
  "postgres")` since the function only compiles for that backend.
  The stale `cfg_attr(not(test), allow(dead_code))` on
  `rebuild_junction_table_for_polymorphic` is gone — the function is
  reached from `sync_relationship_table` in production builds, the
  marker was a leftover.
- Qualified-path cleanup in `db/`: every `super::super::` chain (16
  occurrences across `migrate/collection/`, `query/populate/single/`,
  `query/populate/batch/`, `query/join/hydrate/`) replaced with the
  shortest available `crate::db::*` path per CLAUDE.md. Inline `use`
  statements inside fn bodies (test helpers in `migrate/global.rs`,
  `migrate/collection/alter.rs`, `query/auth/password.rs`,
  `query/fts/sync.rs`, `query/read/find.rs`, `query/read/count.rs`,
  `query/validation.rs`, `query/join/hydrate/{mod,save,locale,group}.rs`,
  `query/populate/batch/dispatch.rs`) lifted to the `mod tests`
  preamble. No public-API change.
- `db/query/ref_count.rs` (1939 LOC) split into a `ref_count/`
  module with one concept per file: `outgoing_ref.rs` (the
  `OutgoingRef` newtype + `push_ref` helper), `api.rs` (public
  orchestrators — `get_ref_count`, `after_create`,
  `after_create_from_data`, `after_update`, `before_hard_delete`,
  `snapshot_outgoing_refs`, `data_touches_refs`,
  `lock_ref_targets_from_data`), `read.rs` (DB read path —
  `read_outgoing_refs` + `collect_*` helpers), `compute.rs`
  (data-driven path — `compute_refs_from_data` + helpers),
  `delta.rs` (`to_delta_map` + `apply_deltas` + `find_missing_ids`).
  `mod.rs` is declarations + re-exports only — no business logic.
  Tests live next to the functions they exercise (api.rs holds the
  orchestrator integration tests + `after_create_from_data` tests;
  delta.rs holds the delta-map and apply-deltas tests; read.rs
  holds the lone direct read test). Shared test fixtures
  (`setup_db`, `no_locale`, `insert_doc`, etc.) live in
  `test_helpers.rs`. Largest split file is 928 LOC (api.rs incl.
  ~640 LOC of tests); all others well under the soft 1000-line
  limit.
- `db/query/read/find.rs` (1889 LOC) split into a `read/find/`
  module: `runner.rs` (the public `find` entrypoint plus the
  small SELECT/limit/map helpers — `build_select`, `apply_fts`,
  `apply_soft_delete`, `apply_limit_offset`, `map_rows`),
  `cursor.rs` (`SortInfo` + `apply_cursor_keyset` +
  `inner_keyset_clause`), `sort.rs` (`resolve_sort` +
  `apply_order_by` + `is_valid_sort_column`). `mod.rs` is
  declarations + `pub use runner::find;` only. Tests live next to
  the functions they exercise: cursor pagination tests in
  cursor.rs (14), sort-validation tests in sort.rs (7), basic
  find / soft-delete / drafts / edge-case tests in runner.rs
  (14). Shared `test_def` and `setup_db` fixtures in
  `test_helpers.rs`. Largest split file is 831 LOC (runner.rs).
- `db/migrate/helpers/join_tables.rs` (1360 LOC) split into a
  `join_tables/` module by table type: `orchestrator.rs`
  (`sync_join_tables` + the recursive walker that dispatches each
  field to the per-type sync helper), `relationship.rs` (junction
  tables for has-many relationships, including the polymorphic
  rebuild path with its 8 tests), `array.rs` (array join tables
  with create/alter helpers and 13 tests), `blocks.rs` (blocks
  tables with create / locale-column-add and 8 tests). `mod.rs`
  is declarations + the `pub(in crate::db::migrate)` re-export of
  `sync_join_tables` only. The previously `pub(super)`
  `rebuild_junction_table_for_polymorphic` is now private — its
  tests live in the same file. Largest split file is 513 LOC
  (relationship.rs).
- `db/query/jobs.rs` (1281 LOC) split into a `jobs/` module by
  operation type: `lifecycle.rs` (insert/complete/fail with retry
  backoff, heartbeat, mark-stale + 9 tests), `claim.rs`
  (`claim_pending_jobs` with sqlite + postgres backends +
  `parse_job_row` + 5 tests), `query.rs` (read-only queries:
  count_*, list_*, get_*, last_*, find_stale_jobs + the wide
  `row_to_job_run` parser + 9 tests), `bulk.rs`
  (`cancel_pending_jobs` + `purge_old_jobs` + 2 tests),
  `cron.rs` (`try_claim_cron_window`). `mod.rs` is declarations
  + `pub use` re-exports only. Shared `setup_db` test fixture in
  `test_helpers.rs`. Largest split file is 452 LOC (query.rs).
- `db/query/filter/resolve.rs` (1278 LOC) split into a
  `filter/resolve/` module by responsibility: `types.rs`
  (`ResolvedFilter` + `SubqueryCondition` + `BlockWalkResult`
  filter shapes), `lookup.rs` (`find_field_recursive` +
  `lookup_column_field_type` + group-path walker — shared
  field-tree traversal), `normalize.rs`
  (`normalize_filter_fields` rewriting Group dot-notation to
  flat `__`-joined column names + 8 tests), `path.rs`
  (`resolve_filter` + `SubFilterCtx` + per-type resolvers for
  Array/Blocks/Relationship + 14 tests), `blocks.rs`
  (`walk_block_fields` + `build_block_type_expr` +
  `build_json_each_source` for JSON-extract path building + 13
  tests). `mod.rs` is declarations + re-exports only. Shared
  test fixtures (`make_field`/`make_array_field`/etc.,
  `test_conn`) in `test_helpers.rs`. The previously
  filter-private types are now `pub(in crate::db::query::filter)`
  so the WHERE-clause builder still sees them. Largest split
  file is 471 LOC (path.rs).
- `db/query/read/back_references.rs` (1092 LOC) split into a
  `read/back_references/` module by scan target: `types.rs`
  (`BackReference` result + `BackRefScan` invariant context),
  `scan.rs` (the public `find_back_references` orchestrator +
  `scan_fields` recursive walker + `scan_relationship` +
  `query_has_one`/`query_has_many` + 12 integration tests),
  `sub_fields.rs` (`scan_array_sub_fields` + `scan_blocks` for
  the join-table scanners + 3 tests), `helpers.rs` (`query_ids`
  + `query_ids_simple`/`_simple_params` self-ref-filtering
  helpers + the cross-module `field_display_label` shared with
  `missing_relations`). `mod.rs` is declarations + re-exports
  only. Shared test fixtures in `test_helpers.rs`.
  `field_display_label` is now `pub(in crate::db::query::read)`
  so the missing-relations sibling continues to import it via
  `use super::back_references::field_display_label`. Largest
  split file is 636 LOC (scan.rs).
- `db/query/fts/sync.rs` (1007 LOC) split into a `fts/sync/`
  module by operation phase: `helpers.rs`
  (`get_fts_table_columns` introspection, shared between
  migration and runtime upsert), `migration.rs`
  (`sync_fts_table` + `bulk_populate_fast` /
  `bulk_populate_slow` for the migration-time
  drop-and-rebuild path + 8 tests), `upsert.rs` (per-document
  `fts_upsert` / `fts_upsert_with_registry` +
  `resolve_logical_columns` + `extract_field_texts` + Postgres
  vs SQLite upsert backends + 10 tests), `delete.rs`
  (`fts_delete` + 2 tests). `mod.rs` is declarations +
  `pub use` re-exports only. Shared test fixtures
  (`setup_db`, `simple_def`, `text_field`, `localized_text_field`,
  `insert_post`, `locale_config_en_de`) live in
  `test_helpers.rs`. Largest split file is 469 LOC
  (migration.rs).
- Removed pure-ceremony `AlterCtxBuilder` in
  `db/migrate/collection/alter.rs` (3 panic-on-missing
  required fields, single call site) — replaced with plain
  struct-literal construction of `AlterCtx`. Builders that
  exist solely to enforce required fields lose to plain
  struct literals when the struct is constructed in one
  place.
- `crate::db::PaginationResult` and `crate::db::Singleflight`
  promoted to top-level `db::*` re-exports (each used twice or
  more externally through the longer `db::query::` path). The
  five external call sites in admin/, service/types/, and test
  modules updated to the short path.
- `db/migrate/backfill_ref_counts.rs` argument-count cleanup:
  introduced `BackfillCtx { conn, locale_config }` invariant
  context threaded through every
  helper. `backfill_has_one` shrinks from 7 args to 4 (now
  takes `&FieldDefinition` and extracts `default_collection`/
  `is_polymorphic`/`is_localized` internally). `backfill_has_many`
  takes `&RelationshipConfig` instead of separate `default_collection
  + is_polymorphic`. `backfill_column_refs` likewise takes
  `&RelationshipConfig`. All private helpers now ≤ 4 args.
- `validate_find_pagination` privatized — it had a public
  `pub use` re-export but zero external callers; only
  `PaginationCtx::validate` invoked it. The shorter
  `PaginationCtx::validate(req_limit, req_page,
  req_after_cursor, req_before_cursor)` is the single public
  entrypoint.
- Final `super::super::*` chain pass: 10 chains in the
  `read/find/{cursor,sort,runner}.rs`,
  `read/back_references/{scan,sub_fields}.rs`,
  `filter/resolve/{path,blocks}.rs`, and
  `fts/sync/{migration,upsert,delete}.rs` test modules
  introduced by my own splits all converted to the
  `crate::db::query::*::test_helpers` form for consistency
  with the earlier ref_count split. Zero `super::super::`
  paths now anywhere in `src/db/`.
- `src/service/` code-quality cleanup pass.
  Already-clean axes (zero `#[allow(...)]`, zero
  `super::super::` chains, zero inline-use in fn bodies, all
  files < 1000 LOC, builders colocated with types, no `>4`-arg
  fns) verified untouched. Active changes:
  - `service/types/service_context.rs` (702 LOC, 7 types)
    split into `email_context.rs` (`EmailContext`),
    `pending_event.rs` (`PendingEvent` + `EventQueue` +
    `flush_queue`), `pending_verification.rs`
    (`PendingVerification` + `VerificationQueue` +
    `flush_verification_queue`), and a slimmed
    `service_context.rs` (614 LOC, holds `Def` +
    `ServiceContext` + `ServiceContextBuilder` +
    `ResolvedConn`).
  - Dead `crate::service::ReadOptions` /
    `ReadOptionsBuilder` (zero call sites, never constructed
    or passed as a parameter) and the entire `read/options.rs`
    file deleted.
  - Optional-setter symmetry fix:
    `PersistOptionsBuilder.locale_config` takes
    `Option<&'a LocaleConfig>` (was `&'a LocaleConfig`),
    matching the `Option<&...>` shape on the other
    optional-attachment methods on the same builder. The two
    callers in `service/write/{create,update}.rs` lost their
    `if let Some(lctx) = ...` wrappers in favor of inline
    `.locale_config(input.locale_ctx.map(|c| &c.config))`.
  - `*Builder` types (`WriteInputBuilder`,
    `ServiceContextBuilder`, `PersistOptionsBuilder`,
    `Find{ById,Documents}InputBuilder`,
    `CountDocumentsInputBuilder`, `LuaReadHooksBuilder`,
    `LuaWriteHooksBuilder`) dropped from
    `crate::service::*` re-exports. Builders are accessed via
    `Type::builder()`; no external caller imported them by
    name.
  - Visibility tightening across `service/`.
    Modules `document_info`, `helpers`, `hooks`,
    `user_settings`, `write`, `read` demoted from `pub mod`
    to `pub(crate) mod` (zero deep-path external users —
    one `service::read::{validate_*}` call site in
    `api/handlers/collection/filter_builder.rs` rewritten
    to use the existing top-level re-export). Functions
    only used inside `service/` demoted to `pub(crate)` at
    their definition: `delete_document_in_conn`,
    `update_document_in_conn`, `update_many_single_in_conn`,
    `persist_bulk_update`, `unpublish_with_snapshot`,
    `send_verification_email`, plus the `DeleteResult`
    return type that they expose. The matching
    `pub(crate) use` re-exports from `service/mod.rs` follow
    suit. `undelete_document_in_conn` and
    `unpublish_document_in_conn` go further — only called
    inside their own files, so they become plain `fn`. The
    stale `pub use` re-exports for them in
    `collection/mod.rs` are deleted.
  - Additional cleanup:
    1. `ServiceContext::flush_event_queue` deleted — defined
       and documented but never called; all 9 callers use the
       free `flush_queue(ctx, &queue)` function instead.
       (Clippy doesn't catch this kind of dead `pub fn`
       because it's reachable from a `pub` parent and could be
       used by downstream crates.)
    2. New `EmailContext::send_verification(pool, slug,
       doc_id, email)` method dedups two identical 7-arg
       `send_verification_email` calls (one in
       `ServiceContext::maybe_send_verification`, one in
       `flush_verification_queue`).
    3. New `ContentService::email_context()` helper in
       `api/handlers/content_service.rs` and
       `AdminState::email_context()` in `admin/mod.rs`
       collapse three identical 3-clone `EmailContext { ... }`
       construction sites (gRPC `create` + `create_many`,
       admin `create_action`).
    4. `_core` suffix on transaction-agnostic functions
       renamed to `_in_conn` (more self-documenting:
       "operates on the connection in `ctx`"):
       `create_document_core` → `create_document_in_conn`,
       `update_document_core` → `update_document_in_conn`,
       `delete_document_core` → `delete_document_in_conn`,
       `update_many_single_core` → `update_many_single_in_conn`,
       `update_global_core` → `update_global_in_conn`,
       and the (now-private) `undelete_document_in_conn` /
       `unpublish_document_in_conn`. Doc comments and bench
       caller updated.
    5. Panic-on-wrong-variant accessors converted to
       `Result<&_, ServiceError>`:
       `ServiceContext::collection_def()` /
       `global_def()` / `fields()` now return
       `Result<&CollectionDefinition, _>` /
       `Result<&GlobalDefinition, _>` /
       `Result<&[FieldDefinition], _>` so misuse surfaces as
       `ServiceError::Internal` instead of crashing the
       process. All 46 call sites across `service/` updated
       to propagate with `?`; the two `post_process` helpers
       (which return `()`) use `let Ok(def) = ... else {
       return };` to skip cleanly when the wrong def variant
       is wired up.
- `src/hooks/` code-quality cleanup pass.
  Initial state: 28k LOC, 108 files, 6 files >1000 LOC.
  Final: 5327 tests pass, 0 failed; clippy clean. Active
  changes:
  - Removed every `#[allow(...)]` escape hatch — 4 of 5 at
    the root cause: stale `dead_code` on `HookEvent` (every
    variant is in use), `unreachable_code` in the
    `HookDepthGuard` test (rewrote the closure to a block
    scope so the early return doesn't have a dead `Ok(())`
    after it), `dead_code` on `validate_timezone` deleted
    (function + tests; only used by its own tests). The 5th
    (`clippy::too_many_arguments` on
    `run_field_hooks_with_conn`) disappeared as a side effect
    of the walker refactor below. Also dropped
    `clippy::only_used_in_recursion` by deleting the unused
    `lua: &Lua` param from `lua_to_json` / `lua_to_json_inner`
    and propagating the deletion through 5 helper fns + ~22
    call sites.
  - Eliminated all `super::super::` chains: 5 in
    `api/{fields,email}.rs`, `api/serializers/{auth,upload}.rs`,
    `api/parse/relationship.rs` rewritten to `crate::hooks::*`.
  - Deep-path import scan turned up 4 hits in test modules —
    fixed by promoting `DisplayConditionResult` to a top-level
    `hooks::*` re-export (`HookRunner` was already there) and
    rewriting the 4 callers to the short path.
  - Wide-arg fns refactored to typed structs or the walker
    pattern:
    - `validate_nested_rows` / `validate_leaf_sub_field`
      (5 args each) bundle `(sf, qualified)` into a
      `SubFieldCall<'_>` struct.
    - `polymorphic::check_one` (5 args): drop the redundant
      `rc: &RelationshipConfig` param — re-extracted from
      `field.relationship.as_ref()` inside the function. Now
      4 args.
    - `register_collection_functions` /
      `register_global_functions` (5 args): bundle the three
      `&'a SharedRegistry / &'a LocaleConfig /
      &'a PaginationConfig` refs into a `CrudRegisterCtx<'a>`
      struct.
    - `globals_update_inner` (6 args): bundle `slug, data_table,
      opts` into `GlobalsUpdateInput`. Now 4 args.
    - `run_field_hooks` (6 args) / `run_field_hooks_with_conn`
      (8 args including `&self`) /
      `run_field_hooks_inner` (6 args) /
      `run_field_hooks_recursive` (7 args) /
      `run_single_field_hook` (7 args): full walker refactor.
      `FieldHooksCall<'a>` bundles `(fields, event, collection,
      operation)`; `FieldWriteCtx` extends with
      `infra: Option<LuaCrudInfra>`; the recursive helpers
      become methods on a `FieldHookWalker<'a>` struct that
      holds `(lua, call)`. Public methods now take
      `(&mut data, &call)` or `(&mut data, &call, wctx)` —
      ≤ 4 args + receiver throughout.
    - `validate_fields_recursive` (7 args) /
      `validate_scalar_field` (7 args): replaced with a
      `ValidationWalker<'a>` struct holding `(lua, data, ctx)`
      with `walk()` and `scalar()` methods (≤ 4 args +
      receiver). Public callers construct the walker
      explicitly:
      `ValidationWalker::new(lua, data, ctx).walk(fields, "", false, &mut errors)`.
  - File-size splits — six files exceeded the 1000-line soft
    limit:
    - `lifecycle/execution.rs` (1132) → `execution/`:
      `mod.rs` (declarations + re-exports only),
      `runtime.rs` (315 LOC, generic hook execution),
      `after_read.rs` (151), `broadcast.rs` (95),
      `display.rs` (148), `field_hooks.rs` (479).
      Tests redistributed to live with the code they
      exercise.
    - `lifecycle/validation/recursive.rs` (1151) →
      `recursive/` with `dispatch.rs` + `scalar.rs` (the
      walker + its scalar method in a separate `impl` block).
      Tests split by topic (layout-dispatch tests in
      `dispatch.rs`, scalar/locale/richtext tests in
      `scalar.rs`).
    - `lifecycle/validation/richtext_attrs.rs` (1284) →
      `richtext_attrs/` with `extract.rs` (122 — node
      extraction from JSON + HTML), `validate.rs` (924 —
      `RichtextValidationCtx` + per-attr checks + tests),
      `before_validate.rs` (256 — before_validate transform
      pipeline).
    - `lifecycle/access.rs` (1249) → `access/` with
      `collection.rs` (478 — collection-level hook +
      `parse_access_constraints`), `field.rs` (672 —
      field-level read/write checks + recursive helpers),
      `test_helpers.rs` (125 — shared `setup_lua` /
      `make_field` / `make_user_doc` fixtures, factored out
      so both collection.rs and field.rs tests can use them
      without duplication).
    - `api/parse/fields.rs` (1068) → `fields/` with
      `constraints.rs` (173 — `Constraints` struct + numeric
      / length / default-value / date-config parsers),
      `single.rs` (861 — `parse_single_field` orchestrator
      + sub-parsers + tests), `top.rs` (38 —
      `parse_fields` entry + duplicate-name check).
    - `lifecycle/validation/sub_fields/tests.rs` (1436 — pure
      tests file) → `sub_fields/tests/` with `basic.rs`
      (Array+Blocks fundamentals), `containers.rs`
      (single-container-in-array), `nesting.rs` (multi-level
      nesting + richtext), `value_constraints.rs` (drafts +
      length/numeric/email/select bounds). Each under 470
      LOC.
    - All `mod.rs` files post-split contain only `mod`
      declarations and `pub(crate) use` re-exports; zero
      business logic lives in `mod.rs` per CLAUDE.md.
  - Stale re-exports dropped: `pub use validate::
    {validate_hook_references, validate_locale_field_collisions}`
    from `hooks/mod.rs` — both functions are only called from
    `init.rs` via `super::validate::*`, so the top-level
    re-export was dead.
  - Dropped the unused `lua: &Lua` param
    from `parse_field_admin`, `lua_table_to_json_map`,
    `lua_table_to_auth_user`, `read_context_back`,
    `json_encode`, `parse_item`, `extract_data`,
    `read_hook_result` — none used `lua` for anything other
    than pre-cascade forwarding to `lua_to_json`. ~10 fn
    signatures + 30+ call sites simplified.
  - Additional cleanup: 4 `mod.rs` files in `hooks/` had
    business logic in violation of CLAUDE.md "mod.rs files
    should contain no business logic." Extracted:
    `lifecycle/validation/mod.rs` (117 LOC) — `ValidationCtx`
    + builder to `context.rs`, `validate_fields_inner` to
    `runner.rs`. `api/mod.rs` — `VmLabel` to `vm_label.rs`.
    `lifecycle/runner/mod.rs` — `HookRunner` struct + impl
    to `hook_runner.rs`. `lifecycle/crud/mod.rs` —
    `get_tx_conn` helper + its test to `tx_conn.rs`. All
    four `mod.rs` files now contain only `mod` declarations
    and `pub use` re-exports.
  - Additional cleanup: deduped the 5-line `is_empty = match
    value { None | Null | empty-String => true, ... }`
    pattern repeated in three validators (recursive scalar,
    sub_fields, richtext_attrs). Extracted to
    `validation::is_empty_value(value: Option<&Value>) ->
    bool` in `runner.rs` with
    `pub(in crate::hooks::lifecycle::validation)` visibility
    so the three callers can `use ...::is_empty_value`
    instead of carrying the same match arm three times.

- **`types/crap.lua` is now generated from Rust source.** A
  `cargo xtask gen-lua-types` task in the new `xtask` workspace
  member assembles the file from one renderer per section
  (`src/typegen/lua/static_file.rs`), interleaving short static
  block files in `src/typegen/lua/blocks/` with derive- and
  macro-emitted output. The CI `check` job and the pre-commit
  hook both run `cargo xtask gen-lua-types --check` to keep the
  on-disk file in sync. Three new derives in `crap-cms-macros`
  drive the generation:
  - `#[derive(LuaAnnotation)]` for `--- @class` blocks (struct
    fields, with `#[lua(rename / ty / optional / skip /
    extends / rename_all)]` overrides; also auto-flattens
    nested struct fields marked `#[lua(flatten)]` via the
    companion `LuaFieldBlock` trait).
  - `#[derive(LuaAlias)]` for `--- @alias` blocks (enums —
    unit-only variants emit literal unions like `"a" | "b"`;
    single-payload variants emit type unions like
    `string | table<string, string>`; mixed is a derive error).
  - `#[derive(LuaFieldTypeViews)]` for `FieldDefinition`'s
    polymorphic per-type config classes (`crap.TextField`,
    `crap.NumberField`, etc.) driven by `#[lua(view_class =
    "...")]` on the variants of the discriminator enum
    (`FieldType`), with `#[lua(applies_to = "text, textarea")]`
    on each shared field selecting which views it appears in.
  Plus `#[lua_fn(path = "crap.X.Y", returns_doc = "...")]`
  attribute macro and `lua_table!` function-like macro that
  together register a Lua-side function table at a given path
  AND emit the corresponding `--- @param … function crap.X.Y(…)`
  doc block. Each `#[lua(doc = "…")]` per-param attribute drives
  the rendered `--- @param` description, so the Rust source is
  the single source of truth for the function's typed signature
  and docs. All 17 files under `src/hooks/lua_api/`, all 14 CRUD
  fns under `src/hooks/lua_api/crud/`, all enum aliases, and
  every Rust-backed struct in the Lua surface are now derive-
  driven. Pure-Lua helpers in
  `src/hooks/lua_api/util_helpers.lua` carry
  `-- @typegen-start … -- @typegen-end` sentinel regions; a
  small extractor at `src/typegen/lua/sentinel_extract.rs`
  parses each annotated `function util.<name>(<args>)` and its
  preceding `---` doc block into the static file, so the docs
  live next to the implementation. New docs surface for three
  previously-undocumented runtime functions
  (`crap.collections.unpublish`, `undelete`, `validate`) plus
  the bulk/versions result classes. The proc-macro auto-emits
  `#[allow(clippy::needless_pass_by_value,
  clippy::unnecessary_wraps, clippy::used_underscore_binding,
  clippy::trivially_copy_pass_by_ref)]` on each generated
  wrapper — these lints fire structurally from mlua's
  `FromLuaMulti` (owned param requirement) and the wrapper
  closure's `LuaResult<T>` return type, scoped to the macro's
  footprint so app code stays allow-free. Existing
  `cargo xtask gen-lua-types` is idempotent — re-running on a
  clean tree changes nothing; running after a Rust-side edit
  emits the diff that needs to be committed.

- **Typed `opts` structs at the CRUD boundary.** Every scalar
  options table for the Lua CRUD API
  (`crap.collections.{find_by_id, create, update, delete,
  unpublish, undelete, validate, list_versions, restore_version,
  create_many, update_many, delete_many}` and
  `crap.globals.{get, update}`) is now a Rust struct deriving
  `serde::Deserialize` + `LuaAnnotation`, decoded via mlua's
  `LuaSerdeExt::from_value` at the function boundary instead of
  N keyed `opts.get::<T>("key")` calls per fn body. The three
  bulk variants reuse the single-op `CreateOptions` /
  `UpdateOptions` / `DeleteOptions` structs since their per-doc
  semantics are identical. `crap.email.send` /
  `crap.email.queue` now take a typed `EmailOptions`, and
  `crap.http.request` takes a typed `HttpRequest` — the
  `HashMap<String, String>` headers field uses
  `#[lua(ty = "table<string, string>")]` to bridge to the Lua
  side (no automatic mapping for `HashMap<...>` in
  `LuaAnnotation`). `crap.jobs.define` now takes a typed
  `JobDefinitionConfig` — the `parse_job_definition` parser
  consumes the typed struct directly instead of reading keys
  off a raw mlua table, and the matching
  `--- @class crap.JobDefinitionConfig` block emits from the
  derive instead of from the hand-written
  `24a_jobs_helper_classes.lua` prose file. `JobLabels` (in
  `core/job/labels.rs`) gained `Serialize`/`Deserialize` derives
  so it can ride along as a nested optional field. This collapses what used to be two parallel
  definitions — a hand-written `--- @class crap.XOptions` block
  in a prose file plus untyped Rust extraction — into a single
  Rust struct that emits the Lua class docs via the same derive
  that drives every other LuaLS type. Surfaced one drift: the
  `overrideAccess` flag on `crap.globals.get` was being read
  from Rust but missing from the Lua docs; it's now documented
  end-to-end. The `where`-shaped query opts
  (`CountQuery`, `FindQuery`, `UpdateManyQuery`,
  `DeleteManyQuery`) still go through the hand-written
  `lua_table_to_find_query` decoder — typing them requires
  typing the filter-clause parser itself, which is a separate
  workstream.

- **`lua_table!` macro auto-emits the section header.** A new
  optional `header: "..."` attribute on `lua_table!` makes the
  generated `render_X_lua` emit the `-- ── crap.X ─...─`
  divider + `--- <doc>` prose + `--- @class crap.X` +
  `crap.X = {}` block before the function declarations. Divider
  widths normalize to a uniform 64 visual columns (replacing the
  62/63/64/66 mix the hand-written intro files had drifted to).
  16 namespace-stub `.lua` block files deleted as part of this:
  log, json, util, auth, access, env, http, email, config,
  locale, crypto, schema, jobs, pages, template_data,
  collections, globals, hooks. Each affected
  `static_file::render_crap_X` shrinks to one or two lines —
  the generated render fn carries everything that used to live
  in `Nx_crap_X_intro.lua`. New helper
  `format_lua_section_header(out, path, doc)` in
  `src/typegen/lua/fn_render.rs` is the format owner;
  `lua_table!` calls it from its emitted code. The remaining
  `include_str!` calls in `static_file.rs` (~23) cover
  genuine hand-written content — section transitions between
  Rust-derived classes, hand-written class blocks like
  `crap.HookContext`, `crap.RichtextNodeSpec`, the
  FindQuery-shaped CRUD opts, and output types like
  `crap.HttpResponse` / `crap.SchemaCollection` that aren't yet
  backed by `LuaAnnotation`-deriving structs. `crap.pages` also
  picked up a typed `PageOptions` Rust struct during this round
  (deriving `Deserialize` + `LuaAnnotation`).

- **Doc-only `LuaAnnotation` structs for context / result / output
  types.** New `src/typegen/lua/doc_structs.rs` holds Rust structs
  that exist solely to drive `--- @class crap.X` emission — no
  `Serialize` / `Deserialize` / `FromLua`, no runtime use. The
  matching Lua tables are still built ad-hoc by Rust handlers, but
  the doc lives next to the Rust code rather than in parallel
  hand-written `.lua` blocks. 14 types covered: hook & access
  context (`HookContext`, `AccessContext`, `AuthStrategyContext`,
  `FieldHookContext`, `ValidateContext`); CRUD result types
  (`ValidateResult`, `UpdateManyResult`, `DeleteManyResult`,
  `CreateManyResult`, `VersionSummary`, `ListVersionsResult`,
  `FindResult`); schema-introspection output (`SchemaCollection`,
  `SchemaField`); HTTP output (`HttpResponse`); job-handler
  context (`JobHandlerContext`, `JobInfo`). All section-divider
  block files (`-- ── Field Types ──` etc.) dropped — LuaLS
  doesn't consume the cosmetic dividers, and they were the bulk
  of the remaining `include_str!` calls. The remaining 14
  `include_str!`s in `static_file.rs` cover content that doesn't
  yet have a clean derive shape: the file header, `Document` /
  `FilterValue` / `FilterOperators` / `OrCondition` / `FindQuery`
  (recursive filter shapes), `CountQuery` / `UpdateManyQuery` /
  `DeleteManyQuery` (depend on the filter shape), `FieldDefinition`
  (the catch-all union), `Activation` / `AuthMethod` (tagged enum
  with per-variant fields), `RichtextNodeSpec` (function-typed
  field), and a handful of factory / sub-namespace intros.

- **Zero hand-written `.lua` block files.** The `blocks/`
  directory under `src/typegen/lua/` is gone entirely. Every
  class / alias / function block in `types/crap.lua` is now
  emitted by a Rust derive or a `lua_table!`-generated render
  fn. New machinery added in this round:
  - `LuaTypeAlias` proc-macro derive (on unit structs) for
    callable / literal-target `--- @alias` blocks. Used for
    `crap.ValidateFunction`, `crap.FieldHookFn`,
    `crap.FilterValue`.
  - `#[lua(extra_field = "[K] V")]` struct-level attr on
    `LuaAnnotation` emits a trailing `--- @field [K] V` line.
    Used for `crap.Document`'s `[string] any` index signature.
  - `LuaAnnotation` and `LuaFieldTypeViews` containers now
    accept each other's struct-level attrs as ignored
    optionals, so the same struct (e.g. `FieldDefinition`) can
    stack both derives.
  - Additional doc-only structs in
    `src/typegen/lua/doc_structs.rs`: `FieldWidth`,
    `PickerAppearance` (string aliases), `ValidateFunction`,
    `FieldHookFn` (function aliases), `Activation`,
    `AuthMethod` (discriminated-union doc class), `Document`,
    `FilterOperators`, `FilterValue`, `OrCondition`,
    `FindQuery`, `CountQuery`, `UpdateManyQuery`,
    `DeleteManyQuery` (query / filter types),
    `RichtextNodeSpec` (richtext input).
  - `crap.FieldDefinition` catch-all class: the existing Rust
    `FieldDefinition` struct now stacks
    `#[derive(LuaAnnotation, LuaFieldTypeViews)]` so the same
    source drives both the per-type subclass family
    (`crap.TextField` etc.) and the union catch-all.
    `crap.JoinConfig` is now emitted (was declared but never
    rendered).
  - The `crap` global banner moved from `00_header.lua` to an
    inline `HEADER: &str` constant in `static_file.rs`.

- **Typegen housekeeping: macro crate split + doc-structs split +
  dedup.** Three follow-on refactors after the static-file pipeline
  landed:
  - `macros/src/lib.rs` (~1700 lines) split into per-derive
    modules: `shared.rs`, `lua_annotation.rs`, `lua_alias.rs`,
    `lua_type_alias.rs`, `lua_field_type_views.rs`, `lua_fn.rs`,
    `lua_table.rs`. `lib.rs` is now the thin entry point — proc-macro
    registration + crate doc only. Each derive's container struct,
    helpers, and codegen live in their own file.
  - `src/typegen/lua/doc_structs.rs` (600+ lines) split into
    `doc_structs/{auth,aliases,context,query,result,misc}.rs` so
    each domain (auth method shape, filter/query, hook context,
    CRUD result, etc.) is navigable on its own. `mod.rs` re-exports
    everything.
  - Dedup: `CreateManyResult` and `UpdateManyResult` previously
    existed both in `src/service/collections/` (real Rust types
    used by the bulk service) and as doc-only copies in
    `doc_structs.rs`. `LuaAnnotation` now derives on the real Rust
    structs directly; the doc-only copies are gone.
    `crap.HookContext` does the same — `hooks::lifecycle::HookContext`
    now derives `LuaAnnotation` with `#[lua(ty = "...")]` overrides
    on the `DocumentFields` / `ReqContext` / `Option<Document>`
    fields, plus the new `extra_field` attr injects `hook_depth`
    (which isn't on the Rust struct — it's populated at
    `to_lua_table` time from `HookDepth` app-data). The remaining
    doc-only types are those whose Lua user-facing shape genuinely
    diverges from the Rust runtime representation (`Activation`,
    `AuthMethod`: Rust-idiomatic tagged enums vs Lua flat
    discriminated-union; `Document` /  `FindQuery`: denormalized
    Lua view vs `DocumentFields` / `FilterClause` Rust internals;
    etc.). Each doc-only submodule's docstring explains the
    divergence so a future contributor knows when to derive on a
    real type vs add a doc-only.
  - Stale `Phase 1-7` comments removed from `typegen/lua/{mod,
    annotation, fn_spec, fn_render, ensure_table}.rs` and
    `macros/src/lib.rs`. The migration is done; the comments
    described work that's already in `git log`.

## [0.1.0-alpha.8] — 2026-05-03

### Breaking Changes

Read this section first when upgrading from `alpha.7`. Each item links
to the detailed entry with full migration steps.

**Deployment / config**

- `server.trust_proxy = true` now fails startup unless
  `server.trusted_proxies` is set. Add an allowlist (bare IPs, CIDR
  ranges, or `"*"`) before upgrading. See *Security → `trust_proxy`
  requires an allowlist*.
- `mcp.api_key` must be at least 32 characters when `mcp.http = true`
  — startup fails otherwise. Rotate short keys to
  `openssl rand -hex 32` before upgrading. See *Security → MCP HTTP
  key hardening*.
- JWT validation is pinned to `HS256` and `exp` is now required.
  Tokens issued under a different algorithm or without an `exp`
  claim are rejected. See *Security → JWT validation hardening*.
- `auth.session_absolute_max_age` defaults to 30 days. Existing
  sessions older than 30 days from their original login (or last
  refresh, for legacy tokens) are forced to re-authenticate. Set to
  `0` to disable. See *Security → Absolute session cap*.

**Template overlays**

- `templates/components/` directory removed. Overlays at
  `config/templates/components/{breadcrumb,pagination,version_sidebar,version_table}.hbs`
  silently stop applying — move them to `templates/partials/`, and
  rename the two `_`-named files to use `-`. See *Changed → BREAKING:
  `templates/components/` directory removed*.
- **Static-asset layout reshuffle — old paths now 404.** CSS, vendor
  bundles, icons, and plumbing JS components moved into role-grouped
  subdirs (`static/styles/`, `static/vendor/`, `static/icons/`,
  `static/components/_internal/`). Config-dir overlays at any old
  path stop serving. Run `crap-cms templates layout` for an exact
  `git mv` migration recipe. See *Changed → BREAKING: static-asset
  layout reorganized into role-grouped subdirs*.
- Inline `style="..."` attributes in overlay templates no longer
  execute. CSP `style-src` no longer allows `'unsafe-inline'`. Use
  classes, the `hidden` attribute, or `data-*` selectors instead;
  programmatic `element.style.foo = ...` from JS is exempt. See
  *Security → CSP hardening: `'unsafe-inline'` removed from
  `style-src`*.
- Inline `<script>` tags in overlay templates must carry
  `nonce="{{crap.csp_nonce}}"` to execute. Inline event handlers
  (`onclick=`, …) are blocked outright — port them to Web Components
  or addEventListener-based wiring. See *Security → CSP hardening:
  nonce-based `script-src`*.
- Overlays that replace `templates/layout/base.hbs` and reproduce the
  `<script id="crap-i18n">` data island must call `{{{admin_i18n}}}`
  inside the script tag (replacing the previous per-key `"key":
  "{{t \"key\"}}"` JSON). Without this, the admin JS `t(key)`
  helper falls back to raw keys and every translatable label
  shows up untranslated.
- **htmx 1.9.12 → 2.0.9.** htmx is now vendored at `static/htmx.js`
  and served from `'self'`; the CDN `<script src="https://unpkg.com/…">`
  is gone, and `https://unpkg.com` is no longer in the default CSP
  `script-src`. Overlays that referenced the old pinned URL/SRI
  must drop both. Most htmx 2 breaking changes do not affect us
  (we use no `hx-on=`, no `hx-ws`/`hx-sse`, no extensions), but
  overlays may need adjustment for: `hx-on=` syntax (now
  `hx-on:event-name`), `selfRequestsOnly` defaults to `true`
  (cross-origin requires opt-in), and DELETE encodes form fields
  in the URL by default. See *Changed → htmx 2.0.9 vendored
  locally* for the full migration list.

**Web Components / overlay JS**

- Singleton discovery events renamed: `crap:toast` →
  `crap:toast-request`, `crap:delete-dialog` →
  `crap:delete-dialog-request`. The new events are discovery-only;
  read `event.detail.instance` and call methods on it (or use the
  `window.crap.*` sugar).
- Globals moved: `window.CrapTheme` → `window.crap.theme`,
  `window.CrapDeleteDialog` → `window.crap.deleteDialog`.
- `<crap-toast>.show(message, type, duration)` (positional args)
  removed. Use `show({ message, type, duration })`.
- `<crap-confirm>` no longer renders its own dialog — it delegates to
  the page-singleton `<crap-confirm-dialog>`. Custom layouts that
  drop the singleton fall back to the native `window.confirm()`.
- `<crap-password-toggle>` now self-renders its toggle button. The
  required template shape collapsed to just
  `<crap-password-toggle><input type="password" …/></crap-password-toggle>`
  — drop the wrapper class and the inner `<button>`. The
  `.form__password-wrapper` / `.form__password-toggle` CSS classes
  were removed. See *Changed → `<crap-password-toggle>` shadow DOM*.
- Web Component styles are now constructable stylesheets
  (`new CSSStyleSheet()` + `adoptedStyleSheets`), not `<style>` blocks
  inside `shadowRoot.innerHTML`. Overlays that previously injected a
  `<style>` block via `shadowRoot.innerHTML` will not survive the
  CSP. Override theming through the documented CSS custom properties
  (which pierce the shadow boundary) or push onto
  `shadowRoot.adoptedStyleSheets` directly.
- `static/components/richtext.js` split into `richtext/` submodules
  (`schema`, `plugins`, `toolbar`, `styles`, `link-modal`,
  `node-modal`, `node-view`). Per-submodule overlays now work
  granularly, but overlays that re-implemented the whole monolith
  need to be re-pointed.

### Security

- **Upload path traversal hardening** — `LocalStorage` now validates every
  storage key, rejecting `..` traversal, absolute paths, backslash-based
  separators, and null bytes before touching the filesystem. `local_path`
  and `exists` fail safe (returning `None` / `false`) on invalid keys.
  Attacker-controlled filenames that slip past sanitisation in a future
  caller will no longer escape `base_dir`.
- **Upload MIME/extension cross-check** — uploaded files whose extension
  implies a browser-renderable type (HTML, SVG, XHTML, XML, JavaScript)
  are rejected unless the actual content matches. Closes the "HTML
  payload stored as `evil.html`, served as `text/html`" XSS vector for
  `image/*`-allow-listed upload fields.
- **`trust_proxy` requires an allowlist** — `server.trust_proxy = true`
  now fails startup unless `server.trusted_proxies` is set. The
  allowlist takes bare IPs, CIDR ranges (`10.0.0.0/8`, `::1/128`), or
  the explicit `"*"` wildcard for dev / isolated-network deployments
  that intentionally want to accept `X-Forwarded-For` from any peer.
  `client_ip` only honours XFF when the immediate peer IP is in the
  allowlist; otherwise the TCP peer address is used, preventing clients
  from rotating per-IP rate-limit buckets by spoofing the header.
- **JWT validation hardening** — `JwtTokenProvider` now pins the
  algorithm to `HS256` via `Validation::new(...)` (previously used
  `Validation::default()` which, while correct today, would silently
  accept a different default in future jsonwebtoken releases). Tokens
  whose header declares any other algorithm — including the classic
  `"alg": "none"` — are rejected outright. `required_spec_claims` is
  also no longer cleared, so any hand-crafted token without an `exp`
  claim is refused rather than treated as a non-expiring token.
- **SVG XXE pre-upload scan** — SVGs containing `<!DOCTYPE>` or
  `<!ENTITY>` declarations are now rejected at upload time. Closes the
  XXE / external-entity vector for any future code path that decides to
  parse or inline-render SVGs server- or client-side, regardless of the
  existing `Content-Disposition: attachment` + CSP-sandbox defences on
  the serve side.
- **Upload decompression-bomb ratio cap** — `check_image_dimensions`
  now also rejects images whose pixel count divided by file size
  exceeds 500 pixels/byte, closing the class of "tiny, heavily
  compressed, absurd dimensions" attacks that slip under the 100 MP
  absolute cap. Normal photographs sit in the single-digit range.
- **Upload error responses scrubbed** — `/api/upload/*` endpoints no
  longer echo inner error detail (multipart parse errors, tokio task
  join errors, service errors, access-check errors) to the client. Full
  detail is logged at `error` level; clients see a generic phrase with
  the same status code, so DB / parser / backend identifiers stay out
  of the wire.
- **SMTP plaintext warning** — when `email.smtp_tls = "none"` is paired
  with a non-loopback host, startup emits a `warn!` naming the host and
  noting that credentials travel unencrypted. Local dev SMTP
  (`localhost`, `127.0.0.1`, `::1`) stays silent; no config change is
  required.
- **Configurable CSRF cookie lifetime** — new `admin.csrf_cookie_lifetime`
  (default `86400`, 24h). Accepts integer seconds or human strings
  (`"4h"`, `"30m"`). Previously hardcoded; shorten to narrow the replay
  window for stolen double-submit tokens.
- **Absolute session cap via `auth_time`** — new `auth.session_absolute_max_age`
  config (default `2592000` = 30 days) caps cumulative session
  lifetime from original login, regardless of how many times the token
  has been refreshed. Claims now carry an `auth_time` field
  (OIDC-standard name) set at login and preserved across refreshes;
  legacy tokens without the claim fall back to `iat` on first refresh,
  giving them up to 30 days from their most-recent issuance before the
  cap kicks in. Set `session_absolute_max_age = 0` to disable the cap
  for long-lived internal-tool sessions; values above 30 days emit a
  startup `warn!`.
- **MCP HTTP key hardening** — `mcp.api_key` must be at least 32 characters
  when `mcp.http = true`; startup fails with a clear message pointing at
  `openssl rand -hex 32` / `/dev/urandom` otherwise. Failed Bearer-auth
  attempts on `/mcp` are now logged at `warn` with the peer IP and
  whether an Authorization header was supplied, so operators can spot
  brute-force scans without leaking the attempted key into logs.
- **Dependency patches for active advisories** —
  - `rustls-webpki` bumped past RUSTSEC-2026-0098 / 0099 / 0104
    (name-constraint bypass and CRL-parsing panic). The old
    `0.101.7` line (pulled in transitively via `rust-s3 0.35`) is
    retired by bumping `rust-s3` to `0.37`; the `0.103.x` line in
    `reqwest` / `lettre` / `quinn` moves to `0.103.13`.
  - `rand 0.9.x` bumped to `0.9.4` and `rand 0.10.x` to `0.10.1`
    (RUSTSEC-2026-0097). `nanoid 0.4` → `0.5` retires the last
    runtime path to `rand 0.8.5`: the new release moves nanoid's
    own dependency to `rand = "0.9"`. The 0.8.5 entry that remains
    in `Cargo.lock` is now a build/test-only transitive (via
    `scraper` → `html5ever` → `phf_codegen`) — not present in the
    shipped binary. nanoid's public API (`nanoid::nanoid!()` /
    `nanoid!(10)`) is unchanged at every call site; default-RNG
    output is bit-identical for the alphabets we use.
- **`cargo audit` is now a CI gate** — `.github/workflows/ci.yml`
  installs `cargo-audit` and runs it on every PR. A committed
  `.cargo/audit.toml` records the one advisory we knowingly accept
  (RUSTSEC-2024-0436, `paste 1.0.15` "no longer maintained") with
  the rationale: `paste` is a proc-macro that runs only in the
  compiler and does not ship in the binary; the deprecation is not
  a CVE; the crate is upstream-pinned in `rav1e 0.8.1` (currently
  the latest), which we need transitively for the `image` crate's
  `avif` feature, which we use for upload format conversion. Will
  be revisited each release; any new advisory will fail CI by
  default.
- **CSP hardening: nonce-based `script-src`** — `'unsafe-inline'` has been
  removed from the default `script-src` directive. A fresh nonce is
  generated per request, inserted into the `Content-Security-Policy`
  header, and exposed to admin templates as `{{crap.csp_nonce}}`. Inline
  `<script>` tags in built-in and overlay templates must now emit
  `<script nonce="{{crap.csp_nonce}}">…</script>` to execute. Inline
  event handlers (`onclick=`, …) are also blocked — the password
  visibility toggle has been refactored into a proper
  `<crap-password-toggle>` Web Component as a reference pattern.
- **CSP hardening: `'unsafe-inline'` removed from `style-src`** — the
  default `style_src` directive is now `'self' https://fonts.googleapis.com`
  with no `'unsafe-inline'`. Steps required to get there:
  - **Web Components** migrated to constructable stylesheets
    (`new CSSStyleSheet()` + `adoptedStyleSheets`) instead of `<style>`
    blocks injected via `shadowRoot.innerHTML`. Constructable stylesheets
    are CSP-exempt by spec.
  - **Dynamic page-level `<style>` injection** in
    `<crap-relationship-search>` and `<crap-create-panel>` rewritten to
    push onto `document.adoptedStyleSheets`.
  - **Templates** use the `hidden` attribute / classes / data-attribute
    selectors instead of `style="..."`. Theme-picker swatches keyed off
    `data-theme-value` attribute selectors.
  - **`<crap-richtext>`'s custom-node modal** applies per-field widths
    via programmatic `element.style.width` (CSP-exempt) rather than
    inline `style="..."` strings.
  - **HTMX's runtime indicator-style injection** disabled via
    `<meta name="htmx-config" content='{"includeIndicatorStyles":false}'>`
    in `base.hbs` (HTMX otherwise calls
    `head.insertAdjacentHTML('beforeend', '<style>...</style>')` at boot
    to add `.htmx-indicator` rules — the only inline-style site in the
    HTMX 1.9 source). The equivalent `.htmx-indicator` rules now ship in
    `static/styles.css`.
  Override authors who need `'unsafe-inline'` back (e.g. for a
  third-party library that requires it) can re-add it in their
  `[admin.csp]` config.
- **Web Components migrated from `innerHTML` to `h()` builder** — every
  Web Component under `static/components/` now constructs DOM via a
  small ~45-line `h()` helper (`static/components/h.js`,
  hyperscript-shape `h(tag, props, ...children)` matching the
  Preact/Vue 3/Mithril convention) instead of HTML-string template
  literals. Defense-in-depth against XSS for a CMS that, by definition,
  handles user-contributed content: with `h()`, untrusted values can
  only enter the DOM through `textContent` / `setAttribute`, both of
  which the browser treats as data — there is no path that interprets
  a string as markup. The previous `richtext.js` `_esc()` static
  helper has been deleted: no caller remains. The two trust-boundary
  sites that legitimately parse server-rendered HTML
  (`create-panel.js` `DOMParser`, `richtext.js` ProseMirror init) are
  preserved with explicit `// SAFETY: …` comment blocks naming the
  trust assumption. Net diff: 39 `innerHTML` writes removed across 14
  components; 1 deliberately-annotated parse site remains. JSDoc
  generic `@template {keyof HTMLElementTagNameMap} K` propagates the
  correct element type to tsserver, so `h('button', {})` narrows to
  `HTMLButtonElement` in IDE hover.
- **MFA brute-force protection — single-use codes + constant-time
  compare** — `verify_mfa_code` previously cleared the stored code
  only on success. An attacker holding a valid MFA-pending JWT could
  brute-force the 6-digit code at request rate (1M codes / 5-min
  window). The code is now single-use: clear-on-every-attempt,
  success or failure, so a typo means re-requesting a fresh code.
  Comparison goes through `subtle::ConstantTimeEq` so response-time
  variance can't recover the stored code byte-by-byte. Regression
  tests in `db/query/auth/mfa.rs::tests` cover the wrong-then-correct
  rejection, the expired-code path, and the missing-user / no-code
  edges.
- **Polymorphic relationship writes enforce the `polymorphic`
  allowlist** — fields declared as `relationship = { polymorphic =
  ["posts", "articles"] }` previously accepted any `(collection, id)`
  pair on the write path: `db::query::join::hydrate::save::save_join_data_inner`
  (and the scalar-column write for non-`has_many`) trusted whatever
  the client submitted. The stored ref then leaked at enrich time as
  a label from a collection the field author never intended to
  expose. New `check_polymorphic_allowlist` validation walks both the
  array shape (`["collection/id", …]` for `has_many`) and the scalar
  / object shape, no-ops on plain (non-polymorphic) relationships,
  and returns a field-error with the rejected collection name. 8
  unit tests cover the scalar / array / object shapes,
  allowed-vs-disallowed targets, and the non-polymorphic no-op path.
- **`upload.s3.secret_key` redacted in `Debug` and `Serialize`** — was
  the only secret in `CrapConfig` stored as a bare `String`. The other
  three (`auth.secret` → `JwtSecret`, `email.smtp_pass` →
  `SmtpPassword`, `mcp.api_key` → `McpApiKey`) all wrap a redacted-
  on-output newtype emitting `"[REDACTED]"`. New `S3SecretKey`
  follows the same pattern (`config/s3_secret_key.rs`);
  `tracing::debug!("{:?}", config)` and any JSON dump of `CrapConfig`
  no longer leak the S3 credential. Also covers a Lua-hook leak
  vector via `crap.config.get`-style serialization paths.
- **Init-only registration APIs refuse runtime calls** — six
  registration APIs that only make sense during init now error
  loudly when called from a runtime hook instead of silently
  no-op'ing or fragmenting across the Lua VM pool:
  `crap.pages.register`, `crap.template_data.register`,
  `crap.richtext.register_node`, `crap.collections.define`,
  `crap.globals.define`, `crap.jobs.define`. Each checks for an
  `InitPhase` marker in `lua.app_data` (set during def-loading and
  `init.lua` execution, removed afterwards) and returns a runtime
  error pointing the caller at `init.lua` if absent. Without these
  guards a runtime registration would either land in one VM of the
  pool and be intermittent across requests
  (`template_data.register`, `richtext.register_node`), land in
  `SharedRegistry` without the corresponding migration / sidebar
  entry / scheduler enrollment and surface as confusing "no such
  table" or 404 errors at first use (`collections.define`,
  `globals.define`, `jobs.define`), or land in the per-VM named
  registry only and never reach the live `CustomPageRegistry`
  (`pages.register`). Regression test per API asserts the runtime
  call is rejected and the registry state remains untouched.
- **Scaffold-generated slugs revalidated** — every `crap-cms make`
  command (`page`, `slot`, `node`, `field`, `theme`, `component`)
  runs the slug through `validate_template_slug` (`[a-z0-9_-]+`, no
  leading / trailing hyphens) before writing files. The `make
  component` HTML custom-element rule (must contain a hyphen,
  lowercase ASCII alphanumerics) is enforced separately. Closes a
  path-traversal vector for callers who pipe untrusted strings into
  `crap-cms make`.
- **Version restore re-validates the snapshot** — the restore write
  path (`service::versions::restore_with_snapshot`) now calls
  `validate_fields` before writing the snapshot back to the table.
  Previously a snapshot that was valid when first saved could become
  invalid against later collection-definition changes (added required
  fields, tightened constraints) and still land in the table on
  restore, producing rows that fail every subsequent validation pass.
  `WriteHooks` gained a `validate_fields` method (with `ValidateResult`
  alias to disambiguate from the shadowed `Result`), implemented by
  both `RunnerWriteHooks` and `LuaWriteHooks`.
- **Richtext custom node names can't shadow built-ins** —
  `crap.richtext.register_node("paragraph", …)` previously appeared
  to succeed but the resolver still picked the built-in, leaving the
  user to wonder why their custom node never ran. A new
  `RESERVED_NODE_NAMES` const enumerates every built-in node + mark
  and rejects matches at registration time with a clear error.

### Added

- **Admin UI customization architecture.** A coherent set of override
  surfaces so config-dir overlays can add and replace pieces of the
  admin without forking templates or patching Rust. The customization
  motion is unchanged — drop a file at the matching path inside the
  config dir's `static/` or `templates/` folder — but there are now
  additive mechanisms alongside the existing whole-file replacement.

  **Slot system** — new `{{slot "name"}}` Handlebars helper renders
  every `*.hbs` file under `templates/slots/<name>/` in alphabetical
  order. Slots are *additive*: your slot file runs alongside upstream's
  defaults instead of replacing them. Slot templates render against
  the same context as their host page. Built-in slot points are
  declared in `templates/slots/<name>/.gitkeep`-style manifests; pick
  by what you want to add (extra dashboard widget, sidebar entry,
  metadata tag) rather than where you want to edit. See
  `docs/src/admin-ui/guides/slots.md`.

  **`{{data "name"}}` helper** — pulls a named blob registered from
  Lua via `crap.template_data.register("<name>", function(ctx) … end)`.
  The registered function runs against the same `ctx` the renderer
  uses (locale, current user, request path) and returns a Lua table
  serialized to template scope. This is the canonical way to inject
  dynamic data into a slot or custom page without forking the host
  template's handler.

  **`[admin] site_name` config** — typed `String` field on `[admin]`
  exposed to templates as `{{crap.site_name}}`. Used by the new
  `templates/partials/logo.hbs` and `meta-tags.hbs` partials so the
  brand wordmark and `<title>` follow one source of truth. Default
  remains the literal `"Crap CMS"`.

  **New partials at `templates/partials/`** — `logo.hbs` (header +
  login wordmark), `meta-tags.hbs` (`<meta>` block in `<head>`),
  `icon-font.hbs` (Material Symbols stylesheet link). Override any
  one by dropping a same-named file at the matching path in the
  config dir; the existing template-overlay mechanism resolves config
  first, embedded second.

  **`static/components/custom.js` auto-import seam** — the default
  `static/components/index.js` now does `import('./custom.js').catch(()=>{})`
  after loading every built-in component. To register bespoke Web
  Components, drop `static/components/custom.js` in the config dir
  with `import` statements for your modules. The default ships an
  empty file (placeholder) so the import never 404s.

  **`_internal/` plumbing convention** — modules that are wired into
  the admin runtime but not part of the public override surface live
  under `static/components/_internal/`. The 33 user-facing components
  stay flat at `static/components/`. Hugo's `_default/` and Next.js
  `_folder` precedent informs the underscore-prefix convention.

- **Per-field render templates: `admin.template` + `admin.extra`.**
  Two new optional keys on a field's `admin = {…}` block bind a
  per-field render template path and a freeform configuration map.
  Replaces the old "rename my field type" hack for one-off custom
  widgets.

  ```lua
  fields = {
    rating = {
      type = "number",
      admin = {
        template = "fields/rating",      -- resolves under templates/
        extra = { max = 5, allow_half = false },
      },
    },
  }
  ```

  At render time, `RenderFieldHelper` reads `template` from the
  flattened `BaseFieldData` and falls back to `fields/<field_type>`
  when unset. The `extra` map is exposed to the template as
  `{{extra.max}}`, `{{extra.allow_half}}`, etc. Both fields are
  threaded through every `BaseFieldData` construction site (six
  builders: `single`, enrich/`children`, enrich/`nested`,
  enrich/`field_types`, collections/items/`create_form`,
  collections/item/`edit_form`) and survive deeply nested
  array/group composition — verified by
  `enriched_sub_field_preserves_admin_template_and_extra_when_nested`.

  **Path validation** — `validate_template_name` rejects 15
  attack vectors: empty paths, leading/trailing `/`, `//`, `..`,
  `.`, NULL bytes, backslashes, percent-encoding, newlines, and any
  character outside `[a-zA-Z0-9/_-]`. No way to traverse out of the
  templates root.

  **Lua parse plumbing** — `&Lua` is now threaded through the
  collection/field parse chain
  (`parse_collection_definition` → `parse_fields_section` →
  `parse_fields` → `parse_single_field` → `parse_field_admin`) so
  `admin.template` validation and `admin.extra`'s `lua_to_json`
  conversion happen at definition time, not render time. Sequences
  and scalars in `extra` are rejected — must be a Lua table that
  serializes to `serde_json::Map<String, Value>`.

- **Custom admin pages.** Filesystem-routed at `/admin/p/<slug>` from
  any `templates/pages/<slug>.hbs` file. Renders against the standard
  admin context (`crap.*`, `user`, `nav` all available); pull
  page-specific data via `crap.template_data.register(<slug>_data, …)`
  and `{{data "<slug>_data"}}`. Sidebar entry registers via
  `crap.pages.register("<slug>", { section, label, icon, access })`
  in `init.lua` — section, icon, and access function are optional.
  Path validation matches the `admin.template` rules. See
  `docs/src/admin-ui/scenarios/05-custom-page.md`.

- **Six new `crap-cms make` scaffolds.** Each command writes the
  right files at the right paths and prints any registration snippet
  to paste into `init.lua`. Slug validation rejects the same attack
  vectors as `admin.template`; `--force` overwrites existing files.
  - `make page <slug>` — writes `templates/pages/<slug>.hbs`, prints
    a `crap.pages.register` snippet (sidebar entry is optional —
    pages route either way).
  - `make slot <name> [--file <filename>]` — writes
    `templates/slots/<name>/<filename>.hbs`.
  - `make node <name> [--inline]` — scaffolds a custom richtext node
    template + `crap.richtext.register_node` Lua snippet.
  - `make field <name> [--base-type <type>]` — generates 3
    coordinated files: `templates/fields/<name>.hbs`,
    `lua/plugins/<name>.lua` with `admin.template` + `admin.extra`
    wiring, and `static/components/<name>.js` Web Component stub.
  - `make theme <name>` — writes
    `static/styles/themes/themes-<name>.css` with the full token
    catalogue commented out for selective override.
  - `make component <tag>` — writes `static/components/<tag>.js`
    with a Web Component skeleton; validates HTML custom-element
    tag rules (must contain a hyphen, lowercase, ASCII alphanumerics).

- **`crap-cms templates` improvements: `layout`, drift detection.**
  - New `templates layout [config_dir]` subcommand — read-only
    migration recipe that scans the config dir for templates living
    in legacy paths (`templates/components/*` and any layout files
    that moved during the static-asset reshuffle) and prints exact
    `git mv` commands. Verified path map covers `auth/`,
    `collections/`, `dashboard/`, `errors/`, `globals/`. Reports
    nothing when the config dir is already current.
  - `templates status` and `templates diff -C <dir> <path>` learned
    drift detection via an optional `{{!-- crap-cms:source X.Y.Z --}}`
    header that `templates extract` now writes. When the embedded
    upstream version moves past the recorded source, `status`
    reports `behind`; `diff` shows the exact upstream change so the
    operator can re-sync intentionally. Files without a source
    header report as `unknown source — use git for diff` and remain
    diffable manually.

- **Customization summary in `crap-cms status`.** New line in the
  default status output: `Customizations: N override(s), N
  addition(s) — N need attention`. Counts come from
  `customization_counts()` in `commands/templates.rs`: `overrides`
  is files that shadow an embedded default, `additions` is files
  that introduce new pages/slots/components without an upstream
  match, `actionable` flags overrides whose recorded source has
  drifted past the embedded version. Suppressed entirely when all
  four counts are zero. Hint line points at `crap-cms templates
  status` for the per-file breakdown.

- **Admin UI documentation rewrite (~2,100 lines).** New structure
  under `docs/src/admin-ui/`: 8 task-shaped scenarios
  (`01-restyle` through `08-upgrade`), `guides/` for cross-cutting
  concerns (themes, template overlay, slots), `reference/` for
  flat lookup pages (CSS variables, components, template context),
  and `upgrade/migrating-from-old-layout.md` for the static-asset
  reshuffle. Replaces the previous "Components", "Customization",
  "Custom Pages" essays with a four-axis decision table at
  `docs/src/admin-ui/index.md` keyed by what kind of change you're
  making. Custom richtext nodes (existing feature) and custom field
  types are now first-class scenarios.

- **AND + OR filter composition in the admin list drawer.** Each
  row in the filter drawer now has a per-row connector dropdown
  (`AND` / `OR`, default `AND`; the very first row's connector is
  hidden via CSS — there's no previous row to connect to). Adjacent
  `OR` rows form a single OR-clause; an `AND` row breaks the streak
  and starts the next clause. Walking
  `[A][AND B][OR C][AND D][OR E]` produces
  `A AND (B OR C) AND (D OR E)` — two independent OR-clauses AND'd
  at the top level. Mirrors the existing
  `FilterClause::Or(Vec<Vec<Filter>>)` shape that the gRPC/Lua side
  has accepted via JSON `or` keys all along; the admin URL grammar
  is the new piece.

  **URL grammar** — `where[field][op]=value` (unchanged) is the
  top-level AND form. `where[or][G][N][field][op]=value` adds the
  OR form: bucket `N` of OR-clause `G`. Multiple entries with the
  same `(G, N)` AND together inside the bucket; different `N`
  values inside the same `G` are OR'd; different `G` values are
  independent OR-clauses AND'd at the top level.
  `parse_where_params` recognises both grammars and reassembles
  `Vec<FilterClause>` accordingly. URL-encoded brackets
  (`where%5Bor%5D…`) work too. Existing bookmarks against the
  flat AND form keep working.

  **Same-field-same-op auto-merge** — within each AND-context
  (top-level + each OR-bucket independently), repeated
  `(field, Equals)` filters collapse into a single
  `FilterOp::In(values)`; repeated `(field, NotEquals)` collapse
  into `NotIn`. `?where[title][equals]=A&where[title][equals]=B`
  becomes `WHERE title IN ('A','B')`, not the silently-empty
  `WHERE title='A' AND title='B'` it produced before. Other ops
  (`contains`, `gt`, …) stay AND'd because they're additive, not
  redundant.

  **`_status` alignment** — `extract_status_filter` returns
  `Option<Vec<String>>`, collecting every `_status` value across
  both URL grammars and de-duplicating. Service-layer injects
  `_status = X` for one value, `_status IN (X, Y, …)` for many. So
  picking both `draft` and `published` in the drawer widens to "show
  both" instead of silently dropping one.

  **Test coverage** — 31 unit tests in `parse_where_params` /
  `extract_status_filter` (top-level AND, OR buckets, multi-clause
  OR, in-bucket merge, system-column rejection inside buckets,
  URL-encoded forms, mixed top + OR). Two new integration tests in
  `tests/admin_collections.rs`: `list_items_or_clause_widens_results`
  pins same-field IN merge + cross-field OR, and the existing
  `list_items_url_status_filter_narrows_drafts_only` got an
  `_status IN (draft, published)` case.

  **Out of scope (v1)** — AND-inside-OR-bucket round-trip in the
  drawer is lossy: the URL grammar parses multi-filter buckets
  correctly, but the drawer renders them as separate OR rows; on
  re-apply they re-bucket as singletons. Document; revisit if it
  bites. A "match all / any" top-level toggle is achievable today
  by setting every row to OR, so it's left out of v1 too.

- **`crap-cms fmt` command — built-in Handlebars template formatter.**
  Plays the same role for `templates/*.hbs` that `cargo fmt` plays for
  Rust and `biome` plays for JS/CSS. Implements a project-specific rule
  set (block helpers indent their bodies, attributes stack at 2+,
  inline collapse for short single-attr/no-attr tags, comments
  preserved verbatim, mustache spacing normalised to compact form,
  void elements self-closed) that no off-the-shelf formatter
  (djlint, prettier-plugin-glimmer) implements correctly. Idempotent
  by property test. New flags:
  - `crap-cms fmt` — format every `.hbs` under the given paths
    in place. Default scope `templates/`.
  - `crap-cms fmt --check` — exit non-zero if any file would change.
    CI gate.
  - `crap-cms fmt --stdio` — read from stdin, write formatted result
    to stdout. Used by editor formatter integrations
    (conform.nvim, etc.). Mutually exclusive with `--check`.

  Wired into the pre-commit hook (`cargo run --quiet --bin crap-cms --
  fmt --check`) and into `.github/workflows/ci.yml` as a separate step
  alongside `cargo fmt --check` / `clippy` / `biome ci`. All 72
  built-in templates were re-formatted with the new tool. Documentation
  in `docs/src/admin-ui/template-formatter.md` and the CLI reference.

  **Raw-content elements** — the body of `<script>`, `<style>`,
  `<pre>`, and `<textarea>` is captured verbatim and passes through
  the formatter without re-indentation, mustache parsing, or
  whitespace collapse. These elements have their own grammar
  (JS/CSS/JSON/preformatted text) that the formatter must not
  rewrite. The matching close tag (`</script>` etc.) is found by a
  case-insensitive linear scan, mirroring the HTML5 parser's
  raw-text content model. Without this, a `<script
  type="application/json">` data island with mustache
  interpolations inside JSON string values (or any non-trivial
  inline `<script>` body) would be reformatted into invalid output.

- **`{{{admin_i18n}}}` helper** — emits the admin-JS translation
  bundle as a single JSON object string, scoped to the current
  `_locale`. Used by `templates/layout/base.hbs` to populate the
  `<script id="crap-i18n">` data island that `static/components/i18n.js`
  reads via `t(key)`. Replaces the previous hand-rolled per-key
  `"key": "{{t \"key\"}}"` JSON construction, which couldn't survive
  the template formatter. Overlay authors who replace the
  `crap-i18n` data island markup must call `{{{admin_i18n}}}`
  inside it to keep `t()` working in the admin UI; the curated key
  list is in `src/admin/templates/helpers/admin_i18n.rs`.

- **Shell completions** — `crap-cms update completions <shell>` generates
  completions for bash, zsh, fish, elvish, and powershell. For bash,
  zsh, and fish, completions are also auto-installed after
  `crap-cms update use` and bare `crap-cms update`:
  - Zsh install path is chosen by probing the user's `$fpath` (via
    `zsh -i -c 'print -l $fpath'`). Prefers `~/.zfunc` when already
    configured, otherwise picks the first user-owned directory on
    `$fpath`. Falls back to `~/.zfunc` and emits an activation hint
    on every install if nothing workable is found.
  - Bash installs under `$XDG_DATA_HOME/bash-completion/completions/`.
    A hint is shown if the `bash-completion` entry point isn't present
    on the system.
  - Fish installs under `$XDG_CONFIG_HOME/fish/completions/` (auto-loaded).
  - `crap-cms update completions <shell> --uninstall` removes a specific
    shell's installed file; `--uninstall` without a shell removes all.
    `crap-cms update uninstall` of the last installed version also cleans
    up any auto-installed completion files.

- **`bench` command** — benchmark hooks, queries, and write cycles for
  developer performance profiling:
  - `bench hooks` — time individual Lua hooks with interactive selection
    wizard (`MultiSelect`). Supports `--hooks`, `--exclude`, `--all` for
    non-interactive use. Uses real documents from the DB when available,
    falls back to synthetic data. Catches hook errors without stopping.
  - `bench queries` — time find queries per collection with optional
    `--where` JSON filter clause (same format as gRPC API). `--explain`
    shows SQLite `EXPLAIN QUERY PLAN` output with real index usage.
  - `bench create <collection>` — time a full document create cycle
    (validation + hooks + persist) with automatic transaction rollback.
    `--no-hooks` for pure persist timing. Confirmation prompt when hooks
    are enabled (skip with `-y`). Unique fields auto-randomized per
    iteration to avoid constraint violations.

- **`status --check` health audit** — best-practice audit for project
  configuration. Checks 24 rules across security, performance, config,
  and operations:
  - **Security**: auth secret strength/placeholder detection, brute-force
    protection, default_deny, access rules coverage, rate limiting with
    auth collections, CORS wildcard + credentials conflict.
  - **Performance**: max_depth, cache disabled with relationships,
    pool_max_size, connection_timeout, compression, pagination max_limit,
    too many hooks/before_change hooks, too many live_mode "full" collections.
  - **Config**: dev_mode, default_depth vs max_depth, email provider "log"
    with verify_email enabled.
  - **Operations**: pending migrations, auth collection without soft_delete,
    upload collection without versioning, soft_delete without retention
    policy, empty auth collection (0 users).

### Changed

- **BREAKING: static-asset layout reorganized into role-grouped subdirs.**
  The previously-flat `static/` directory now groups files by role:
  - `static/styles/{base,parts,layout,themes}/` — CSS split by
    concern, composed via `static/styles/main.css` (replaces the
    flat `static/styles.css` + per-concern siblings).
  - `static/vendor/{codemirror,htmx,prosemirror}.js` — vendored
    third-party bundles (replaces flat `static/codemirror.js` etc.).
  - `static/icons/` — Material Symbols woff2 + stylesheet.
  - `static/components/_internal/` — plumbing modules
    (`css.js`, `global.js`, `groups.js`, `h.js`, `i18n.js`,
    `picker-base.js`, `util/*`). The 33 user-facing components stay
    flat at `static/components/`.

  **No compat aliases.** Old static paths (`/static/styles.css`,
  `/static/htmx.js`, `/static/components/css.js`, …) now 404 outright.
  Config-dir overlays still living at the old paths stop applying.
  This was a deliberate "the best part is no part" call — runtime
  alias tables rot silently and add complexity for everyone forever
  to save a one-time migration effort for the small group of users
  who actually have overlays.

  **Migration recipe** — run `crap-cms templates layout` for an
  exact `git mv` script that updates any overlay files in your
  config dir from old paths to new ones. Recipes don't run anything;
  copy + paste only when you're ready. Background and full path map
  at `docs/src/admin-ui/upgrade/migrating-from-old-layout.md`.

- **BREAKING: htmx 2.0.9 vendored locally.** The admin layout no
  longer pulls htmx from `https://unpkg.com`; it serves
  `static/htmx.js` from the same origin via a new
  `scripts/bundle-htmx.sh` (mirrors the existing
  `bundle-prosemirror.sh` / `bundle-codemirror.sh` pattern). The
  script downloads the upstream artifact, verifies a pinned
  SHA-384 against tampering, and writes a banner-prefixed
  vendored copy. Re-run when upgrading htmx; bump `VERSION` and
  `EXPECTED_SHA384` together after independently verifying the
  new release.

  **CSP impact** — `https://unpkg.com` removed from the default
  `script-src`; `'self'` now covers every script the built-in
  admin loads (`/static/htmx.js`, `/static/codemirror.js`,
  `/static/prosemirror.js`, `/static/components/index.js`). One
  fewer third-party origin in the trust boundary; one fewer DNS
  lookup at page load.

  **htmx 2 behavioral changes that may affect overlays** —
  - `hx-on="..."` (single attribute) replaced by per-event
    `hx-on:event-name="..."` (kebab-case). The built-in templates
    use neither form, but overlay templates that did need the
    rename.
  - `selfRequestsOnly` config defaults to `true`. Overlays that
    issue cross-origin htmx requests must set it to `false`
    explicitly via the `<meta name="htmx-config">` JSON.
  - `methodsThatUseUrlParams` now includes `delete`; htmx-driven
    DELETE requests with form fields encode them in the URL
    instead of the body. Built-in delete forms have no user-input
    fields so this is a no-op for us; overlays with custom
    DELETE forms should verify their server-side parsing.
  - Default `scrollBehavior` changed from `smooth` to `instant`.
    Restore the old default with
    `<meta name="htmx-config" content='{"scrollBehavior":"smooth"}'>`
    if the new feel is unwanted.
  - `hx-ws` and `hx-sse` attributes are gone from core htmx —
    install the corresponding extensions if you used them.
    (We use neither; live events go through `<crap-live-events>`.)
  - The `htmx.config.includeIndicatorStyles=false` workaround
    we ship to keep `style-src` free of `'unsafe-inline'` still
    works in 2.x; nothing to change there. As an alternative,
    htmx 2 added `inlineStyleNonce` / `inlineScriptNonce`
    config options that accept our per-request nonce and let
    htmx inject its own styles/scripts with the correct nonce.

- **BREAKING: `templates/components/` directory removed — the four
  partials it held (`breadcrumb`, `pagination`, `version_sidebar`,
  `version_table`) moved to `templates/partials/` for naming
  consistency.** The two `_`-named files were renamed to use `-` to
  match other partials. Update overlay references:
  - `{{> components/breadcrumb}}` → `{{> partials/breadcrumb}}`
  - `{{> components/pagination}}` → `{{> partials/pagination}}`
  - `{{> components/version_sidebar}}` → `{{> partials/version-sidebar}}`
  - `{{> components/version_table}}` → `{{> partials/version-table}}`

- **Six new partials cover the most common template duplications.**
  Override authors get one place to retheme each pattern.
  - `partials/htmx-nav-link.hbs` — the
    `<a class="button button--…" href="…" hx-get="…" hx-target="body"
    hx-push-url="true">…</a>` pattern that was hand-written 30× across
    the templates. Accepts `href`, `label_key`/`label`, `variant`,
    `size`, `icon` parameters.
  - `partials/status-badge.hbs` — `<span class="badge badge--{status}">
    {status}</span>` pill, repeated 5× across sidebars and table rows.
  - `partials/error-page.hbs` — full error-page card (h1 + message +
    optional detail + back-to-dashboard button) used by
    `errors/404.hbs`, `errors/403.hbs`, `errors/500.hbs`. Each error
    page collapsed to a one-liner `{{> partials/error-page code=…
    message_key=…}}`.
  - `partials/warning-card.hbs` — `<div class="card card--warning">`
    container with title and slotted body. Used by `delete_confirm` and
    both `restore_confirm` templates.
  - `partials/loading-indicator.hbs` — `<div id="upload-loading"
    class="loading-indicator|edit-sidebar__save-indicator">…</div>`
    target for HTMX `hx-indicator`. `variant="sidebar"` parameter
    selects the compact sidebar styling.
  - `partials/form-actions.hbs` — `<div class="form__actions">` wrapper
    + cancel link, with action buttons in a partial-block slot. Used
    by all three confirm pages.

- **`layout/auth.hbs` consolidates head + auth-card chrome.** The four
  auth pages (`login`, `forgot_password`, `reset_password`, `mfa`) and
  two auth-style error pages (`auth_required`, `admin_denied`) each
  re-stated ~35 LOC of identical head boilerplate (theme-FOUC script,
  CSRF auto-injection script, stylesheet + module-script tags) plus
  the `<div class="auth-card">` outer wrapper and CMS-logo header. All
  six pages now extend `{{#> layout/auth title=…}}…{{/}}`. The layout
  accepts `header_icon_kind="material"` (with `header_icon=…`) for the
  two error pages that prefer a Material-Symbol over the SVG logo, and
  optional `header_title` to override the default "Crap CMS" title.

- **`partials/field.hbs` accepts explicit-param overrides** in addition
  to the inherited field-context values it already used. Calling
  `{{#> partials/field label="My Label" error="bad"}}…{{/}}` with
  explicit args now works as documented (it always did, but wasn't
  spelled out — handlebars-rust merges partial-block params into the
  parent context). Unit test
  `field_partial_explicit_params_override_inherited_context` locks
  this in.

- **Auth pages: CSRF cookie regex normalised.** The inline auth-page
  CSRF auto-injection script used `(?:^|; )` (single-space form) for
  the cookie match, while `static/components/util/cookies.js` (and the
  rest of the codebase) uses `(?:^|;\s*)`. Auth pages were the last
  hold-out; now consistent.

- **BREAKING: Web Component admin library — singleton API harmonization.**
  The `static/components/` admin-UI library now has a single singleton
  discovery convention and a unified `window.crap` namespace. Migration
  of any custom overlays:
  - Event `crap:toast` → renamed to `crap:toast-request`. The detail
    shape is unchanged: `{ message, type?, duration? }`.
  - Event `crap:delete-dialog` → renamed to `crap:delete-dialog-request`.
    The new event is **discovery-only** (`detail.instance` is filled in
    by the singleton); to open the dialog, call
    `instance.open(opts)` after discovery, or use the convenience
    `window.crap.deleteDialog.open(opts)`.
  - `<crap-toast>` instance method `show(message, type, duration)`
    (positional) → removed. Use `show({ message, type, duration })`.
  - Global `window.CrapTheme` → moved to `window.crap.theme` (same
    `get` / `set` / `apply` methods).
  - Global `window.CrapDeleteDialog` → moved to
    `window.crap.deleteDialog` (same `open(opts)` method).
  - `<crap-confirm>` no longer renders its own dialog — it delegates
    to the page-singleton `<crap-confirm-dialog>` via the discovery
    pattern. Falls back to native `window.confirm()` (with
    `console.warn`) if no `<crap-confirm-dialog>` is mounted.
  - New `static/components/global.js` builds the `window.crap`
    namespace at admin-page load. Properties: `toast(opts)`,
    `confirm(message, opts?)` (returns `Promise<boolean>`),
    `drawer.{open, close}`, `deleteDialog.{open}`,
    `createPanel.{open, close}`, `theme.{get, set, apply}`, `csrf()`.
    Documented as **sugar** over the canonical event-discovery + module
    APIs.

- **Field templates — shared `partials/field` wrapper with three
  structural variants.** The label + required marker + locale badge +
  error + help boilerplate (~250 lines, repeated across 14 templates)
  now lives in `templates/partials/field.hbs`. The partial accepts a
  `variant` parameter:
  - `default` (no variant arg) — `<label for=…>` above the slot. Used
    by `text`, `textarea`, `email`, `password`, `number`, `date`,
    `json`, `code`, `richtext`, `select`, `relationship`, `upload`.
  - `variant="fieldset"` — `<fieldset class="form__radio-group"><legend>…`
    wrapping the slot. Used by `radio`.
  - `variant="checkbox"` — `<div class="form__checkbox">` with the
    slot then `<label for=…>` inline. Used by `checkbox`.
  All three variants share the same required-marker, locale-badge,
  error-paragraph, and help-paragraph logic, so overrides to the
  contract apply uniformly. Layout fields (`array`, `blocks`, `group`,
  `row`, `tabs`, `collapsible`, `join`) keep their custom rendering.
  Override authors can replace the partial in their config-dir
  `templates/partials/field.hbs` to retheme every field in one place.

- **`<crap-password-toggle>` — shadow DOM, self-styling, no markup
  contract.** The component now renders its own toggle button + icon
  inside its shadow root and styles the slotted input via
  `::slotted(input)`. The required template shape collapsed from:

  ```html
  <crap-password-toggle class="form__password-wrapper">
    <input type="password" name="password" />
    <button type="button" class="form__password-toggle"
            aria-label="Toggle password visibility">
      <span class="material-symbols-outlined">visibility</span>
    </button>
  </crap-password-toggle>
  ```

  to:

  ```html
  <crap-password-toggle>
    <input type="password" name="password" />
  </crap-password-toggle>
  ```

  Migrated `templates/fields/password.hbs`, `templates/auth/login.hbs`,
  `templates/auth/reset_password.hbs`. The `.form__password-wrapper`
  class and the `.form__password-toggle` rule set in `static/forms.css`
  have been removed (component-owned now). Auth pages now load
  `/static/components/index.js` like the admin pages instead of
  cherry-picking `password-toggle.js`. New regression test
  `browser_password_toggle::password_toggle_reveals_and_hides_value`.

- **Picker base class — three pickers consolidated.** The shared
  toggle / dropdown / outside-click logic that was duplicated across
  `<crap-locale-picker>`, `<crap-ui-locale-picker>`, and
  `<crap-theme-picker>` (~50 LOC apiece, ~150 LOC total of near-
  identical code) now lives in `static/components/picker-base.js`
  (`CrapPickerBase`). Each subclass declares the toggle/dropdown/item
  selectors, the open-class, and the `dataset` key holding the option
  value as static class properties; the only behaviour each provides
  is `_onValue(value)` and an optional `_afterToggle()` hook for
  per-toggle state refresh (the theme picker uses this to highlight
  the active option). Per-picker tags stay distinct because templates
  and tests reference them, only behaviour is deduplicated.

- **Sidebar panels — shared `partials/sidebar-panel` wrapper.** The
  `<div class="edit-sidebar__panel">…<panel-header><icon> <label></panel-header>…<panel-body>…</panel-body></div>`
  shell that recurred 8 times across `collections/edit_sidebar.hbs`,
  `globals/edit_sidebar.hbs`, and `components/version_sidebar.hbs`
  now lives in `templates/partials/sidebar-panel.hbs`. Callers pass
  optional `icon` and `label_key` (translation key) parameters and
  slot the body content. Saves ~100 LOC, gives overlay authors a
  single retheme target for sidebar panel chrome.

- **Array/blocks row header — shared `partials/array-row-header` wrapper.**
  The drag handle + toggle + title + error-badge + 4 action buttons
  (`move-up`, `move-down`, `duplicate-row`, `remove-array-row`) that
  recurred 4× across `array.hbs` and `blocks.hbs` (initial render +
  `<template>` clone in each) now lives in
  `templates/partials/array-row-header.hbs`. Callers pass `expanded`
  (bool), `has_errors` (bool), and slot the row title content. Saves
  ~80 LOC. Action buttons remain `data-action` attributes delegated
  to `array-fields.js` — markup is pure HTML, so a server-rendered
  partial fits cleanly (per the project rule: no JS logic → partial,
  JS logic → Web Component).

- **`password`, `radio`, `checkbox` field templates — alignment with
  the standard wrapper.** Adopting `partials/field` fixes several
  pre-existing inconsistencies:
  - `password.hbs` — `locale_locked` badge is now rendered (was
    silently dropped); error/description paragraphs now in the
    canonical order (error first, then help).
  - `checkbox.hbs` — required-marker (`*`) is now rendered when the
    field is required (was silently dropped); the input now carries
    the HTML `required` attribute when the field is required, so the
    browser blocks submit instead of relying solely on server-side
    validation feedback.

- **Admin-UI util module — six near-identical helpers consolidated.**
  Cross-cutting helpers previously inlined across 12 component files
  now live in `static/components/util/`:
  - `cookies.js` — `readCsrfCookie()`, `readCookie(name)`. Replaces 6
    duplicate CSRF readers (delete-dialog, validate-form, conditions,
    list-settings, create-panel, ui-locale-picker, session-guard).
  - `toast.js` — `toast({ message, type, duration })`. Replaces 4
    inline `dispatchEvent(new CustomEvent('crap:toast', ...))` blocks.
  - `htmx.js` — `getHttpVerb(e)`. Replaces 3 sites with subtly
    different verb-extraction / case-handling.
  - `discover.js` — `discoverSingleton(eventName)`. Standardizes the
    event-discovery dance for callers of `<crap-drawer>`,
    `<crap-confirm-dialog>`, `<crap-create-panel>`,
    `<crap-delete-dialog>`.
  - `json.js` — `parseJsonAttribute(el, attr, fallback)`,
    `readDataIsland(host, id, fallback)`. Replaces 3 inline JSON-attr
    parsers (richtext × 2, list-settings) and consolidates the
    data-island read pattern.
  Override authors can drop a replacement file at the matching path
  inside their config directory's `static/components/util/` folder.

- **`init` scaffolds `.mcp.json`** — new projects include a Claude Code
  MCP configuration file out of the box. Running Claude Code from the
  config directory auto-connects to the CMS's MCP server.

- **`status` command enhanced** — now displays:
  - Server configuration (ports, compression, rate limiting).
  - Trash count per collection (soft-deleted documents) and `soft_delete` tag.
  - Versioning details (drafts, max versions) per versioned collection.
  - Access rules overview (read/create/update/delete functions per
    collection and global, with default deny/allow indicator).
  - Hooks assignments (which lifecycle hooks are wired, with function names).
  - Live event configuration (mode per target, or summary).

- **Quieter startup logging** — per-collection field listings and runner
  VM `crap.log.info()` messages demoted to `debug`. Only the init VM logs
  at `info` level. HookRunner pool creation summarized as a single line
  with VM count and elapsed time (e.g., "HookRunner ready: 22 VM(s) in
  1236ms"). Loaded collections summary now includes hook count.

- **Startup health check nudge** — after booting, the server runs the
  `status --check` audit silently and logs a one-liner if warnings are
  found (e.g., "6 health check warning(s) found — run `crap-cms status
  --check` for details").

- **Hook execution timing in dev_mode** — when `dev_mode = true`, each
  hook invocation and per-event totals are logged at `debug` level with
  elapsed milliseconds (e.g., "hooks.auto_slug: 0.31ms").

### Fixed

- **`_status` filter from the admin UI was silently dropped.** The
  filter builder exposes `_status` as a filterable field for
  collections with drafts (`build_filter_fields` adds it whenever
  `def.has_drafts()`), producing URLs like
  `?where[_status][equals]=draft`. But `_status` is a system column
  (`_*` prefix), and the architecture rejects user filters on system
  columns at two layers:
  `admin::handlers::query::filter::parse_where_params` filters them
  out (the field-validity check), and `validate_user_filters` would
  reject them at the service-layer entry point in any case. Net
  effect: the filter URL would update, but the page would render
  unfiltered (showing both drafts and published, since the admin
  list always passes `include_drafts = true`).

  Fix: new typed param `status_filter: Option<Vec<String>>` on
  `FindDocumentsInput` (mirrors the existing `trash: bool` pattern
  for `_deleted_at`). The admin list handler reads every
  `where[_status][equals]=X` value from the raw query — both
  top-level and OR-bucket forms — via `extract_status_filter()` and
  forwards them as a typed param, so `_status` reaches the SQL via
  the trusted post-validation injection path in
  `build_effective_query` (`Equals` for one value, `IN (…)` for two
  or more). Generic-user-filter rejection of system columns is
  unchanged. Other read surfaces (gRPC, MCP, Lua) continue to
  control draft visibility through the typed `include_drafts` flag.

  Regression test
  `list_items_url_status_filter_narrows_drafts_only` creates one
  published and one draft document on a `has_drafts` collection,
  hits `?where%5B_status%5D%5Bequals%5D=draft`, and asserts the
  rendered `tbody` has exactly one row (the draft) with the
  published row absent. Symmetric coverage for `_status=published`,
  for the empty-value "All" case, and for the multi-`_status` OR
  case (both rows shown when `_status IN (draft, published)`).
  Plus 11 unit tests for `extract_status_filter` (raw +
  URL-encoded + missing + wrong op + non-system-column + collects
  from OR-buckets + dedupes + mixed top-and-OR).

- **Drafts hidden on page 1 of admin lists with cursor pagination.**
  Collections with drafts and a `default_sort` whose key is NULL on
  draft rows (e.g. the example `posts` config sets
  `default_sort = "-published_at"`, and the
  `set_published_at` hook only fills it on publish) sorted drafts to
  the bottom — SQLite places NULLs last in DESC. With the default
  `per_page = 20`, the first page of 28 mixed posts showed 20
  published rows; the 2 drafts paginated out of sight.

  Fix at the SQL builder: when `def.has_drafts()` and the user's
  sort isn't already `_status`, `apply_order_by`
  (`src/db/query/read/find.rs`) prepends `_status DIR` to the ORDER
  BY (ASC normally, flipped to DESC under `using_before` so
  `before_cursor` walks the same composite order in reverse). Effective
  order becomes `(_status, sort_col, id)` and `'draft' < 'published'`
  alphabetically surfaces drafts above published. When the WHERE
  clause already pins `_status` to a single value (drafts/published
  filter, or `include_drafts=false` injection on public reads) the
  prepend is a no-op SQL-wise.

  Cursor pagination kept symmetric across the draft↔published
  boundary by extending the cursor encoding: `CursorData` gained a
  `status_val: Option<String>` (`#[serde(default)]` so legacy
  bookmark URLs decode and fall back to single-column keyset),
  populated by `cursor_from_doc` when the row is on a drafts-enabled
  collection. `apply_cursor_keyset` builds a composite `(_status
  outer_op cursor_status) OR (_status = cursor_status AND <inner
  keyset>)` so prev returns the drafts that next skipped.
  `apply_select_filter` was extended to keep `_status` regardless of
  caller-provided `select` so cursor encoding never falls back to a
  bogus default. The shared gate predicate
  `cursor::cursor_status_active(has_drafts, sort_col)` is used by
  both `apply_order_by` and `PaginationResult::cursor` to keep the
  SQL writer and the cursor encoder locked together.

  Regression tests:
  `drafts_sort_above_published_in_admin_list`,
  `cursor_round_trip_preserves_drafts_on_page_1` (5 published + 2
  drafts, page → next → prev returns the original page 1 in order),
  `before_cursor_on_draft_walks_draft_bucket` (using_before symmetry
  on a draft sort_val), `legacy_cursor_without_status_val_still_works_on_drafts_collection`
  (backward compat for old cursor URLs), `apply_select_filter_keeps_status_for_cursor`,
  `drafts_first_does_not_disturb_status_filtered_query` (no-op when
  WHERE pins `_status`).

- **`Save as draft` blocked by required-field validation.** Three
  layers needed adjustment to make this work end-to-end on a
  collection with `has_drafts = true` and required fields:

  1. **Browser native validation** — field templates emit the HTML
     `required` attribute when `field.required = true`, so clicking
     any submit button on a form with empty required fields would be
     blocked by browser constraint validation before the request
     even left. Fixed by adding `formnovalidate` to the "Save as
     draft" submit buttons in `templates/collections/edit_sidebar.hbs`
     and `templates/globals/edit_sidebar.hbs`. HTML5's
     `formnovalidate` on a submit button bypasses the form's
     constraint validation for that submit only.

  2. **Pre-submit validate endpoint** — `<crap-validate-form>`
     intercepts `htmx:beforeRequest`, posts the form data to a
     `/validate` JSON endpoint, and only lets the real submission
     proceed if validation passes. The component built its
     payload via `new FormData(form).entries()` — which silently
     drops the *submitter button's* `name=value` pair (that's only
     included by the browser during *native* form submission, not
     when constructed standalone). Net effect: `_action=save_draft`
     was missing from the validate request, so the server saw
     `payload.draft = false`, ran non-draft validation, returned
     `{ title: "Field is required" }`, and the JS rendered inline
     errors on a draft save the user explicitly asked for.

     Fixed in `static/components/validate-form.js` by tracking the
     last clicked submit button on the component instance
     (`_lastSubmitter`, captured via a capture-phase click listener)
     and passing it as the second argument to `new FormData(form,
     submitter)`. Browsers without that constructor signature fall
     back to manual `fd.append(submitter.name, submitter.value)`.

  3. **Server validation** — already correctly skipped required
     checks for drafts (`src/hooks/lifecycle/validation/checks/required.rs:8-23`
     returns early when `is_draft = true`). No change needed; the
     existing pipeline just couldn't fire because layers 1+2 were
     blocking the request from reaching it.

  Regression tests:
  `html_versions::save_draft_button_carries_formnovalidate` (asserts
  the rendered HTML carries the attribute on both create and edit
  pages) and
  `browser_validation::save_as_draft_skips_required_via_validate_endpoint`
  (full browser flow: open create page, leave required field empty,
  click Save as draft, assert no inline `form__error[data-validate-error]`
  appears). Both fail without their respective fix.

- **Filter drawer auto-applied `_status=published` on empty state.**
  `<crap-list-settings>::_buildFilterUI` previously rendered ONE
  default row when the URL had no filters (`presets.length > 0 ?
  presets : [null]`). That row hydrated to the first field's first
  op + first value — typically `_status = "published"` for
  collections with drafts, since `build_filter_fields` lists
  `_status` first. User opens drawer, doesn't realise the row is
  pre-configured, clicks Apply → URL gets
  `?where[_status][equals]=published` and the list silently
  narrows. Fix: drawer opens with zero rows when URL has no
  filters; the "+ Add condition" button is the explicit
  affordance. `_collectFilters` also now skips rows whose `field` /
  `op` are empty, or whose `value` is empty (except for `exists` /
  `not_exists` which take no value). Regression test
  `filter_drawer_empty_when_no_url_filters` exercises the
  open-drawer-and-apply-without-changes flow.

- **Cursor pagination + filter change produced empty/wrong
  results.** When the user paginated with `after_cursor=…` /
  `before_cursor=…` in the URL and then changed the filter via the
  drawer, `_buildFilterUrl` deleted only `where[…]` params,
  preserving the stale cursor. The cursor was issued against the
  previous result set; with a different filter, the cursor's
  keyset comparison narrowed the WHERE clause to empty (or
  wrong-position rows). Fix: strip `after_cursor` and
  `before_cursor` alongside `where[…]` on every filter apply, then
  reset to `page=1`. Regression test
  `filter_apply_strips_stale_cursor`.

- **Filter-apply navigated to nowhere with htmx 2.** After the htmx
  1.9 → 2.0.9 migration, applying a filter from the
  `<crap-list-settings>` filter drawer silently failed to update the
  URL or refresh the list. Two stacked option-API renames in
  `htmx.ajax()` between versions:
  1. `pushUrl` (1.x) → `push` (2.x). The 1.x key is silently dropped.
  2. `push` takes a *string* (`"true"` or a path), not a boolean.
     Passing `push: true` got coerced to the string `"true"` and
     pushed the literal URL `/admin/collections/true` into history —
     visible as the wrong URL in the address bar after applying a
     filter.
  Fixed in `static/components/list-settings.js::navigate()` —
  passes `push: <path>` with the actual destination path. Four
  regression tests now pin the behaviour:
  `filter_builder_preset_value_change_applies` (preset value swap →
  URL reflects new value),
  `filter_builder_apply_actually_filters_the_list` (apply → list
  actually narrows: 2 rows → 1 row, only the matching status visible
  in `<tbody>`), `filter_builder_multi_row_reopen_edit_persists`
  (multi-row apply → reopen → edit → all rows survive with edited
  values), and `filter_builder_preserves_user_edit_across_op_change`.
  Plus the integration test
  `list_items_url_filter_narrows_results` covers the server-side
  pipeline directly with both raw and URL-encoded `where[…]` query
  strings. The user's reported "additional filter rows get lost"
  symptom was a downstream effect of the same bug — without URL
  pushing, the post-apply page state never reflected the new filter
  set, so reopening the drawer from the unchanged URL appeared to
  lose rows.

- **Filter-builder dropped user edits on op-change.**
  `static/components/list-settings.js`'s `_buildFilterRow` captured
  the URL-derived `preset` by closure. When the user changed the op
  (which can re-render the value input — `exists` / `not_exists`
  drop the input entirely, switching back rebuilds it), the rebuild
  used the stale `preset.value` and silently overwrote whatever the
  user had just selected. Symptom: open the filter drawer with a
  preset like `?where[status][equals]=published`, switch the value
  dropdown to `draft`, change the op — the value snaps back to
  `published`. Pre-existing bug from the alpha.8 webcomponents
  refactor (`34410b8`); not introduced by the recent declarative
  htmx work, but found while investigating filter behaviour. Fix:
  `renderOp()` and `renderValue()` now read the current DOM state
  (`opSelect.value`, `valueWrap.querySelector('[name="filter-value"]')?.value`)
  before the rebuild and fall back to the preset only when the input
  doesn't yet exist or has no value. Regression test
  `filter_builder_preserves_user_edit_across_op_change` exercises
  the URL-preset → value-edit → op-change → value-survives flow
  end-to-end.

- **`<crap-create-panel>` form submission rewritten as declarative
  htmx.** Removes ~110 lines of imperative submit logic
  (`_submitForm`, `_handleSubmitResponse`, `_sendForm`,
  `encodeFormBody`, the multipart-vs-urlencoded branch, the strip-htmx
  pass) and the `readCsrfCookie` import. The injected form keeps its
  server-rendered `hx-post` (or `hx-put`); the panel sets
  `hx-target="this"`, `hx-swap="outerHTML"`, `hx-select="#edit-form"`
  (so the server's full-edit-page validation re-render gets sliced
  down to just the form fragment), and `hx-headers='{"X-Inline-Create":"1"}'`.
  htmx 2 picks the request encoding from the form's native `enctype`
  attribute (which `templates/collections/edit.hbs` already emits as
  `multipart/form-data` only when `collection.is_upload`) — the
  multipart-vs-urlencoded encoding bug we hit earlier in this release
  becomes structurally impossible, since the client never inspects the
  FormData to decide. Two `htmx:beforeSwap` / `htmx:afterRequest`
  listeners on the panel body (which survives across form re-renders)
  intercept the `X-Created-Id` success header to fire the `onCreated`
  callback + close the panel; on validation error the swapped form
  fragment shows inline field errors as before. Regression tests
  `relationship_inline_create_selects_item` and
  `relationship_inline_create_validation_error_rerenders` exercise
  both happy path (with file upload — multipart) and validation
  re-render (urlencoded) end-to-end.

  **Server side**: new `htmx_inline_created(id, label)` response
  builder in `src/admin/handlers/shared/response.rs`. Returns 200 with
  `X-Created-Id` / `X-Created-Label` headers and an empty body — *no*
  `HX-Redirect`, since the panel keeps the parent page. The create
  handler reads `X-Inline-Create: 1` off the request and branches to
  this builder; page-level creates keep the old `htmx_redirect_with_created`
  flow.

- **`<crap-list-settings>` column save rewritten as declarative
  htmx.** Removes the manual `fetch()` POST + CSRF-header construction
  for `/admin/api/user-settings/{slug}` (~15 lines). The column-picker
  form now carries `hx-post`, `hx-swap="none"`; the existing
  `htmx:configRequest` listener in `templates/layout/base.hbs`
  threads CSRF; an `htmx:afterRequest` listener on the form fires
  `drawer.close()` + `window.location.reload()` on success. Drops
  the `readCsrfCookie` import. The form lives inside
  `<crap-drawer>`'s shadow DOM, so `htmx.process(form)` is invoked
  after `appendChild` to register it (htmx auto-discovery doesn't
  traverse shadow roots).

- **Live-search input UX cleanup.** `templates/collections/items.hbs`:
  the search input changed from `type="text"` to `type="search"`
  (the existing `hx-trigger="…, search"` was unreachable on a
  `text` input — the `search` event only fires from the browser's
  native clear-X on `type="search"`). Adds `hx-indicator="#upload-loading"`
  so the 300ms-debounced searches show a loading state.

- **`getHttpVerb()` dual-shape comment.** `static/components/util/htmx.js`
  documents *why* the helper checks both `evt.detail.requestConfig.verb`
  (htmx 2) and `evt.detail.verb` (htmx 1.x legacy + some 2.x events
  that retain the flat shape). The comment guards against a future
  drive-by cleanup deleting one of the paths.

- **Textarea-style fields accreted leading whitespace on every save.**
  `templates/fields/{textarea,json,code,richtext}.hbs` rendered
  `{{value}}` on its own indented line between `<textarea>` and
  `</textarea>`. Per HTML5 only the *first* LF after `<textarea>` is
  stripped — every other byte (including the source template's 4
  spaces of indentation and the trailing `\n  ` before
  `</textarea>`) becomes part of the field's submitted value. On
  save, that whitespace round-tripped to the database and the next
  render wrapped it again, so each save grew the value by another
  indent level. Source now uses `>{{value}}</textarea>` flush; the
  template formatter learned to hug `</tag>` against the body when
  the body has no trailing newline (idempotent on the new form).
  Regression test
  `textarea_field_value_does_not_accrete_whitespace_on_round_trip`
  exercises all four field types through two render passes.

- **Globals edit form: missing loading indicator.** `globals/edit.hbs`
  had no `hx-indicator="#upload-loading"` attribute on the form, and
  `globals/edit_sidebar.hbs` was missing the indicator markup, so the
  user got zero visual feedback during a global save. Both gaps fixed;
  regression test
  `html_globals::global_edit_form_has_loading_indicator` covers it.

- **Dead templates removed.** `templates/collections/edit_actions.hbs`
  and `templates/globals/edit_actions.hbs` were stale duplicates of
  the live save-panel logic that lives inside `*/edit_sidebar.hbs`.
  Confirmed via grep that no template, helper, or handler referenced
  them. Deleted.

- **False orphan column warnings for localized fields** — the migration
  system incorrectly warned about locale-suffixed columns (e.g.,
  `title__en`, `title__de`) as orphans even when the field was
  `localized = true` and the locale was configured. The expected column
  set now correctly includes locale-suffixed variants.

- **`has_many` select lost values on save and rejected edits on validate** —
  two reinforcing bugs in the multi-select pipeline:
  - **Save path**: `parse_form` extracted HTML form bodies as
    `Form<HashMap<String, String>>`, which silently drops duplicate keys.
    `<select multiple>` submits `skills=a&skills=b` as two entries with
    the same name, so every save was truncated to the last selection.
    Now parsed as `Vec<(String, String)>` with duplicate keys collapsed
    into a comma-joined string (the shape `transform_select_has_many`
    already expected). Same fix applied to the multipart path.
  - **Validate path**: the `<crap-validate-form>` JSON endpoint sends
    array values as JSON arrays. `values_to_string_map` serialised those
    via `Value::to_string()` into `["a","b"]`, which
    `transform_select_has_many` then split on the embedded commas —
    producing garbage like `"skills has an invalid option: \"motion\"]"`
    because each JSON-quoted element was treated as a literal option.
    Transform now detects a canonical JSON string array and forwards it
    unchanged; falls back to comma-splitting only for traditional form
    input.

- **Upload error responses: scrub `Transient` too** — `/api/upload/*`
  responses for `ServiceError::Transient` echoed the inner DB / pool
  error text (e.g. "database is locked") to the client, inconsistent
  with the existing scrubbing for `Internal`. Now logged at `error` and
  replaced with a generic "Service temporarily unavailable" string; the
  503 status and retry semantics are unchanged.

- **Image conversion queue stored absolute filesystem paths** — after
  the upload path-traversal hardening, `LocalStorage` rejects keys that
  start with `/`. The image-variant enqueue path had been recording
  `storage.local_path(...)` (an absolute path) instead of the storage
  key, so every queued WebP/AVIF conversion failed with "Source image
  not found" at dequeue time. Queue entries now record the storage key
  directly, matching what `storage.get()` / `storage.put()` expect.
  Also works unchanged for S3 / custom backends (where `local_path`
  returns `None`).

- **Image queue could orphan entries in `processing`** — the conversion
  finalizer ran "update collection doc URL" and "mark entry completed"
  as two sequential statements. If the first hit `SQLITE_BUSY` after
  the file was already written (e.g. under write contention from
  concurrent conversions), the row stayed in `processing` forever —
  only a server restart's `recover_stale_images` could free it. Both
  writes are now in a single transaction; on rollback the entry is
  marked `failed` so startup / `crap-cms images retry` can pick it
  back up. Image-queue errors are also logged with the full anyhow
  cause chain so the underlying SQLite / pool reason surfaces instead
  of just the outer "execute failed: UPDATE …" wrapper.

- **Inline create panel: form submission for non-upload collections** —
  `<crap-create-panel>`'s `_submitForm` was sending its `FormData` body
  via `fetch(body: formData)`, which the browser encodes as
  `multipart/form-data`. Server-side, `parse_form` for non-upload
  collections uses axum's `Form` extractor which only accepts
  `application/x-www-form-urlencoded` — every inline-create POST hit the
  parse-error path and got redirected to the create page (200 OK with
  HTML body, no `X-Created-Id` header), so the panel never closed. Fixed:
  `_submitForm` now picks the encoding based on whether the form
  contains a non-empty `File` value — multipart for uploads, URL-encoded
  otherwise. This was masked in development because the e2e browser
  suite was effectively non-functional (see Internal section).

- **JPEG EXIF orientation now applied before re-encode** — uploaded
  JPEGs from phones (which rely on the EXIF `Orientation` tag rather
  than rotating the pixel data) used to display sideways or mirrored
  after upload. The image crate strips EXIF on re-encode but doesn't
  auto-rotate, so the orientation hint was lost and the original
  pixel data shipped through unchanged. New `core/upload/exif.rs`
  reads the tag with `kamadak-exif` (new dep), applies the rotation
  / flip via `image::imageops` (8 cases), and passes the corrected
  image into the conversion pipeline. 8 unit tests cover every
  orientation value plus an end-to-end JPEG round-trip.

- **NaN / Infinity rejected in number fields** — number-field
  validation previously checked `min` / `max` bounds without first
  ensuring the value was finite. `Number::as_f64()` returns the
  inner value verbatim, so `serde_json::Value::Number(NaN)` /
  `Infinity` slipped through every comparison (NaN comparisons are
  always false). The bounds check now starts with `is_finite()` and
  emits a `validation.number_not_finite` error otherwise. Same code
  path that gates `min` / `max`; alpha.4 introduced a sibling check
  on a different write path.

- **gRPC `ServiceError → Status` mapping aligned to gRPC spec** —
  - `ServiceError::UniqueViolation` was mapped to `INVALID_ARGUMENT`;
    it's now `ALREADY_EXISTS` (code 6). Client SDKs branch on this
    code for "use existing / pick another" flows; the old mapping
    made conflicts indistinguishable from plain validation errors.
  - `ServiceError::InvalidToken` was mapped to `INVALID_ARGUMENT`;
    it's now `UNAUTHENTICATED` (code 16). Client SDKs trigger
    token-refresh on this code; the old mapping looked like a
    malformed request and silently suppressed refresh.
  Per-variant regression tests in
  `api/handlers/collection/error_mapping.rs::tests` lock both new
  codes in.

- **Job claim no longer pre-bumps the attempt counter** —
  `parse_job_row` previously added `+1` to the parsed `attempts`
  column on the assumption that the row would be bumped server-side
  on success; the SQLite path then ALSO bumped it, doubling the
  increment. Postgres' `FOR UPDATE SKIP LOCKED` claim already bumps
  and parses in one round trip, but the SQLite path separates them.
  `parse_job_row` now reports the row's stored count verbatim; the
  SQLite path bumps locally before returning. Net effect: retry-budget
  calculations finally match across backends. Doc-string on the
  function spells out the contract; regression test
  `claim_reports_attempt_count_consistent_with_db_increment` pins
  it.

- **`S3Storage::exists()` propagates non-404 errors** — the previous
  fall-through returned `Ok(false)` for any error, so a 403
  AccessDenied or 503 Slow Down silently looked like a missing file
  and let upload-then-verify orphan its DB rows on transient
  outages. New `is_not_found_error` classifier matches `404` /
  `NoSuchKey` / `Not Found`; everything else surfaces as `Err`
  with the underlying message preserved. Unit tests cover both the
  recognised forms and the non-404 (auth / transient / network /
  signature) failures.

- **S3 region parse rejects garbage strings at startup** —
  `aws_region::Region::FromStr` is infallible: unknown strings fall
  through to `Region::Custom { region: x, endpoint: x }`, which
  DNS-fails at first request with no startup hint. `create_s3_storage`
  now matches on `Ok(Region::Custom { … })` (only valid when an
  explicit `upload.s3.endpoint` is set) and bails with a clear
  diagnostic pointing the operator at known region codes or
  `upload.s3.endpoint` for custom S3-compatible providers. Sanity
  test on `eu-west-1`, custom-endpoint bypass test on `auto`.

- **Unpublish on collections with localized fields no longer fails
  silently** — `unpublish_document` (and `persist_unpublish`,
  `unpublish_global_document`) called `find_by_id_raw(... None ...)`,
  which fell back to bare column names from `get_column_names` —
  e.g. `title` instead of `title__en` / `title__de`. SQLite errored
  `no such column: title`, the catch-all error arm in `do_update`
  redirected silently, and the user saw "unpublish button does
  nothing." `LocaleConfig` is now threaded through `ServiceContext`
  (new optional `locale_config` attachment + `default_locale_ctx()`
  helper); the unpublish path builds a `LocaleMode::Default` context
  for the raw read so the SELECT references the actual locale-
  suffixed columns. Bug existed across every unpublish surface
  (admin collections + globals, gRPC, MCP, Lua CRUD); all six
  fixed. The fix uses `LocaleMode::Default` (resolved at the default
  locale, flat keys) rather than `LocaleMode::All` (grouped
  `{en, de}` objects) so the snapshot saved by `persist_unpublish`,
  the BeforeChange / AfterChange hook context, and the broadcast
  event all match the shape produced by every other write path —
  preserving snapshot fidelity for non-default locales is the same
  as regular draft saves and is a separate change.
  `versioned_collection_unpublish_with_localized_field` exercises
  the full PUT path, asserts `_status = 'draft'` post-unpublish,
  and pins the snapshot shape (`title` is a flat string, not a
  grouped object).

- **`locale_locked` recomputed for nested fields** — sub-field
  enrichment (`build_sub_field_base`, `build_child_base`) previously
  inherited `locale_locked` from the parent context. With a parent
  group that's `localized = false` containing a child that's
  `localized = true`, the child wrongly appeared locked when editing
  in a non-default locale. Both builders now recompute
  `locale_locked = non_default_locale && !sf.localized` from the
  sub-field's own definition; the `dispatch_sub_field_type` Code
  arm threads `non_default_locale` into the new builder signature.

### Internal

- **Typed admin context structs.** Replaced the previous
  `serde_json::Value` builders that produced template context for the
  admin UI with typed page and field context structs at
  `src/admin/context/page/*` and `src/admin/context/field/*`. Each
  page type (auth, collections, dashboard, errors, globals, meta) has
  a typed envelope with `#[serde(skip_serializing_if = …)]` on
  optional fields. The 691-line `context_builder.rs` orchestrator
  shrunk to a thin dispatcher; per-page builders are explicit.
  `BaseFieldData` flattens admin attributes (label, placeholder,
  template, extra, …) so the Handlebars renderer reads them at the
  same depth regardless of field type, and the `RenderFieldHelper`
  reads the new `template` field directly. Schema/doc generation at
  `src/admin/context/page/schema_doc.rs` enumerates context keys per
  page so future renames surface as compile-time errors instead of
  silent template misses. First wave of a broader effort to retire
  `serde_json::Value` blobs from internal interfaces — admin UI is
  done, service layer / hook payloads / event streams still pending.

- **e2e browser test suite resurrected** — the entire `browser_*` test
  modules (139 tests across 18 components) had been silently failing for
  the whole alpha.7 → alpha.8 development cycle, masked by the fact that
  CI doesn't run `--features browser-tests`. Three pre-existing test
  framework bugs and several stale per-test selectors / timings:
  - `tests/e2e/browser.rs::spawn_server` was using `axum::serve(listener,
    router)` instead of `into_make_service_with_connect_info::<SocketAddr>()`
    — the login handler extracts `ConnectInfo` for client-IP rate
    limiting and panicked on every request with `Missing request
    extension: ConnectInfo<SocketAddr>`. Broken since the `trust_proxy`
    work in alpha.7's hardening pass.
  - `tests/e2e/helpers.rs` initialised the test app's `token_provider`
    with a different secret (`"test-secret"`) than `jwt_secret`
    (`"test-jwt-secret"`). Login signed JWTs that auth middleware then
    rejected — every authenticated request 401'd back to `/admin/login`.
  - Test app's session cookies were emitted with `Secure` (because
    `dev_mode = false` by default) but the test server is HTTP — browsers
    silently dropped them. Fixed by setting `dev_mode = true` in the test
    config.
  - chromiumoxide bumped 0.7 → 0.9 for chromium 147 protocol compat.
  - Stale selectors in `browser_tags.rs` (used `.form__tags-input` but
    component renders `.tags__input` in Shadow DOM), `browser_relationship.rs`
    (used `.relationship-search__input` for has-many fields where the
    actual class is `.relationship-search__tags-input`; queried `ref_id`
    column on a join table that uses `related_id`), and `browser_focal_point.rs`
    (queried `<img>` from light DOM but it now lives in the component's
    Shadow DOM, plus the 1×1 PNG fixture needed explicit dimensions for
    `getBoundingClientRect`).
  All 139 e2e tests now pass with `cargo test --test e2e --features
  browser-tests -- --test-threads=1`.

- **CI feature matrix** — `.github/workflows/ci.yml` now runs three
  additional jobs in parallel with the default `check` job:
  `sqlite+postgres` (build + clippy + full test suite), `postgres-only`
  (`--no-default-features --features postgres`, build + clippy — surfaces
  sqlite-isms leaking through the `DbConnection` abstraction), and
  `all-features` (build + clippy with `--all-features` to catch
  feature-interaction compile errors across `s3-storage`, `redis`, and
  `browser-tests` deps). Closes a longstanding gap where the postgres
  backend, in the tree since alpha.6, had no CI coverage.

- **CI: dedicated e2e browser-test job** — new `e2e` job in
  `.github/workflows/ci.yml` installs Chrome via
  `browser-actions/setup-chrome@v1` and runs `cargo test --test e2e
  --features browser-tests`. The 139 resurrected browser tests now
  gate every PR alongside the unit/integration suite; the longstanding
  "compile-only, runs in a separate job" comment in the CI feature
  matrix is now true.

- **Typed admin URL builders** — new `src/admin/handlers/shared/paths.rs`
  with 10 helpers (`paths::collection`, `paths::collection_item`,
  `paths::global_versions_page`, `paths::mfa_with_collection`, etc.).
  Replaces 43 ad-hoc `format!("/admin/...")` strings across 18 handler
  files. Helps with grep-ability and prevents subtle path drift between
  call sites that reference the same route.

- **`FindQuery` builder is now the only construction path** —
  `FindQuery::new()` (a wrapper around `Default::default()` that bypassed
  the builder) has been removed. Inherently-optional fields on
  `FindQueryBuilder` (`order_by`, `limit`, `offset`, `select`,
  `after_cursor`, `before_cursor`, `search`) now take `Option<T>` so
  every call site flows through one builder chain — no more
  `let mut fq = …; if let Some(x) = opt { fq = fq.method(x); }` and
  no more `let mut fq = …; fq.field = …;` patterns. Required fields
  (`filters`, `include_deleted`) stay direct-value. Sweep covered
  19 production sites (gRPC `Find`, MCP `find` tool, Lua hook
  converter, admin items list, populate join + batch dispatch, bulk
  delete/update hooks, admin search, enrich types, bench commands,
  trash CLI, service layer) plus all internal and integration test
  sites. `FindQuery::default()` remains for the truly empty-state
  case (e.g. `&FindQuery::default()` passed to a function that just
  needs the default query); the builder is the path whenever any
  field is set.

- **`ServiceContext` builder: `lua_infra` and `locale_config` take
  `Option<&T>`** — both methods previously took `&T` with the
  caller-side `if let Some(ref x) = parent_x { builder = builder.x(x); }`
  wrapper, repeated 11× across Lua CRUD paths
  (`collection/{create,update,delete,undelete,unpublish}`,
  `bulk/{create_many,update_many,delete_many}`, `versions/restore`,
  `globals/update`). Now `Option<&T>` matches the existing optional-
  attachment shape (`cache`, `event_transport`,
  `invalidation_transport`, `email_ctx`); every caller drops the `if
  let` and folds the call into the builder chain. The `lua_infra`
  body internally short-circuits on `None`. `inner_ctx` in
  `unpublish_document_pool` likewise threads `ctx.locale_config`
  straight through.

- **`crap-cms make component` scaffold uses constructable stylesheets**
  — the generated component skeleton now uses the `css` tagged-template
  helper + `h()` builder pattern that matches every built-in
  component since the CSP hardening pass. Keeps the scaffolded
  output current with the override-template-author guidance.

## [0.1.0-alpha.7] — 2026-04-18

### Added

- **`service::create_many`** -- new service-layer function for bulk
  document creation with transaction chunking, event publishing, and
  cache clearing. Available for Lua hooks, MCP, and direct Rust callers.
  gRPC `CreateMany` RPC to be added in a future release.

### Changed

- **Live event publishing moved to the service layer** -- mutation events
  are now published by the service functions (`create_document`,
  `update_document`, `delete_document`, `undelete_document`,
  `unpublish_document`, `update_global_document`) instead of each
  handler (gRPC, admin, MCP) publishing independently. This eliminates
  the class of bugs where a handler forgets to publish events (e.g.
  `empty_trash` was missing events entirely).
  Events are queued during the transaction via `EventQueue` and flushed
  after commit, so Lua CRUD operations within hooks also produce events
  correctly. Infrastructure (event transport, cache, event queue,
  verification queue) is threaded from the parent service function
  through `RunnerWriteHooks` into the Lua VM via `LuaCrudInfra` in
  `app_data`, making side-effect mutations from hooks first-class
  citizens of the event system.
  **Surfaces that now emit live events:**
  - gRPC API (Create, Update, Delete, Undelete, Unpublish, RestoreVersion)
  - Admin panel (create, update, delete, empty trash, undelete, restore)
  - MCP tools (create, update, delete, undelete, unpublish, restore)
  - Lua CRUD within hooks (create, update, delete, undelete, unpublish, restore_version)
  **Operations that do NOT emit live events (by design):**
  - Migrations (`migrate up`) -- run before the event transport exists
  - `on_init` hooks -- run during startup before subscribers connect
  - CLI commands (`user create`, `import`, etc.) -- no event transport

- **Cache clearing moved to the service layer** -- the populate cache
  (relationship resolution results) is now cleared by every write
  operation at the service level, not just gRPC handlers. Admin,
  MCP, and Lua mutations now correctly invalidate the cache.

- **Bulk operations (`DeleteMany`, `UpdateMany`) moved to the service
  layer** -- transaction chunking, per-doc lifecycle hooks, event
  publishing, cache clearing, and referenced-doc handling are now in
  `service::delete_many` and `service::update_many`. All surfaces
  (gRPC, admin empty_trash, Lua `delete_many`/`update_many`) call
  the same service functions. Pool-based callers get automatic 500-doc
  transaction chunking; Lua callers on an existing connection run
  single-pass.

- **Verification emails moved to the service layer** -- email
  verification for auth collections with `verify_email` enabled is
  now triggered by `create_document` and `create_many` at the service
  level. Verification emails are queued via `VerificationQueue` during
  transactions and sent after commit, so Lua CRUD creates within hooks
  also trigger verification. Previously only gRPC and admin create
  handlers sent verification emails; Lua CRUD, MCP, and bulk creates
  were silently skipping it.

- **Service functions support pool and conn modes** -- all single-op
  service functions (`create_document`, `update_document`,
  `delete_document`, `undelete_document`, `unpublish_document`,
  `update_global_document`, `restore_collection_version`,
  `restore_global_version`) now detect `ctx.pool` vs `ctx.conn` and
  run in the appropriate mode. Pool mode opens its own transaction;
  conn mode runs on the existing connection (Lua CRUD path). Lua CRUD
  now calls the public service functions directly instead of internal
  `_core` variants, ensuring events, cache, and verification are
  handled uniformly.

### Fixed

- **`empty_trash` now emits live events** -- previously, emptying the
  trash via the admin panel deleted documents without publishing any
  mutation events. Subscribe/SSE clients were not notified. Now handled
  automatically by the service-layer event publishing.

- **Verification emails now sent from all create surfaces** -- Lua
  `crap.collections.create()`, MCP create tools, and bulk
  `CreateMany` now send verification emails for auth collections.
  Previously only gRPC and admin panel creates triggered verification.

- **Admin mutations now clear the populate cache** -- previously only
  gRPC handlers cleared the cache. Admin panel writes left stale
  relationship data in the cache.

## [0.1.0-alpha.6] — 2026-04-16

### Added

### Changed

- **Batched `apply_deltas` for ref-count updates** — ref-count
  increments/decrements are now grouped by (collection, delta) and
  applied in a single `UPDATE … WHERE id IN (…)` per group instead of
  one UPDATE per target document. Reduces round-trips from O(targets)
  to O(distinct collection×delta pairs) — typically 1-3 UPDATEs instead
  of 5-8+ for a write touching multiple relationships.

- **Data-driven ref-count for creates (`after_create_from_data`)** —
  on create, outgoing references are now computed from the write data
  in memory instead of reading them back from the database. Eliminates
  5+ SELECT queries per create that were redundantly re-reading
  just-written data.

- **Ref-count skip for non-ref updates** — `persist_update` now checks
  whether the write data touches any relationship fields. When it
  doesn't (e.g. updating only `content`), the entire ref-count dance
  (snapshot before, read after, apply deltas) is skipped — saving
  ~10 queries per non-ref update.

- **Late ref-count lock acquisition** — the `_ref_count` UPDATE
  (which acquires a Postgres row-level lock on the referenced target)
  is now the last operation in `persist_create` and `persist_update`,
  after version snapshots and FTS indexing. This minimizes the time
  the row lock is held under concurrent writes to shared targets
  (e.g. 50 creates all referencing the same author). Combined with
  the other ref-count optimizations above, concurrent create
  throughput improved ~3× and write latency dropped significantly.

- **Batched junction-table inserts** — `set_related_ids` and
  `set_polymorphic_related` now use a single multi-row INSERT instead
  of one INSERT per related ID. Reduces round-trips from O(N) to 1
  per has-many relationship field per write.

- **Chunked bulk operations (DeleteMany, UpdateMany)** — bulk
  mutations now process documents in batches of 500 per transaction
  instead of loading all matches into a single long-lived transaction.
  This removes the previous 10,000-document hard limit, keeps row
  locks short-lived, and prevents timeouts on large bulk deletes
  (previously a DeleteMany of ~14k documents would hold locks for
  minutes and timeout the gRPC client).

- **Default `[database] connection_timeout` raised from 5s to 30s** to
  match `busy_timeout`. The pool-level timeout was firing before
  SQLite's own WAL-writer retry loop had a chance to resolve write
  contention, producing spurious `ServiceError::Transient` errors
  under load (most visible on `find_deep` and bulk-write workloads).
  The new default lets SQLite's busy handler do its job before the
  outer pool gives up. Explicit config overrides still win.

- **Postgres backend: per-connection prepared statement caching** —
  the postgres backend now caches prepared statements per connection,
  mirroring what rusqlite's `prepare_cached` already provides for the
  SQLite backend. Previously, every call through the `DbConnection`
  trait re-parsed and re-planned the SQL on the postgres side — even
  for identical queries repeated thousands of times per second (e.g.,
  `SELECT ... FROM posts WHERE id = $1`). The cache is a per-pooled-
  connection `HashMap<String, Statement>` that persists across pool
  checkouts (no `DISCARD ALL` — recycling method switched from `Clean`
  to a no-op fast recycle). `SET timezone = 'UTC'` is also moved from
  per-checkout to a one-time `post_create` hook, eliminating another
  round-trip per request. Provides a uniform ~2× throughput improvement
  across every postgres workload — reads and writes alike. SQLite
  backend is unchanged.

- **Postgres write path: `SELECT … FOR UPDATE` pre-lock removed from
  ref-count handling.** Previously, every `create` and `update` on
  postgres acquired a per-target `FOR UPDATE` row lock on each
  referenced document before the main INSERT, then again before each
  ref-count UPDATE — adding 1-2 round-trips per referenced doc on the
  write hot path. The protection was redundant: the subsequent
  `UPDATE _ref_count = _ref_count + 1 WHERE id = ?` already takes the
  same row-level write lock implicitly, and the `affected == 0` check
  introduced in alpha.5 already detects a concurrently-deleted target
  and rolls back the enclosing transaction. SQLite was already a
  no-op on this path (its `IMMEDIATE` transactions serialize all
  writers at the DB level); SQLite behavior is unchanged. Multi-server
  no-dangling-reference safety is preserved via `get_ref_count_locked`
  on the delete side + the create side's `affected == 0` rollback.
  **Subtle behavior change**: under a tight create-vs-hard-delete
  race on the same target, the **delete now wins** instead of the
  create — the create rolls back with a clear "cannot reference X:
  target no longer exists" error. Both paths produce a consistent
  database; only which side surfaces the error has shifted.

### Fixed

- **N+1 query on join-field population** — `populate_join_fields` was
  issuing one `find()` per parent document in a batch. A `find()`
  returning 20 parent docs with a `comments` join field meant 20
  follow-up queries. Replaced with a single batched
  `find(on_field IN (…))` plus post-fetch bucketing by the `on_field`
  value. Access-check semantics preserved: `Denied` still yields empty
  arrays for every parent without querying the target collection, and
  `Constrained` filters merge into the batched query just like they
  did in the per-parent path. Eliminates the N+1 pattern entirely,
  yielding order-of-magnitude throughput gains and tail-latency
  reductions for deep-populated queries.

## [0.1.0-alpha.5] — 2026-04-15

### Added

- **`crap-cms update` built-in version manager** — nvm-style CLI for
  managing installed versions of the binary. Subcommands: `check`,
  `list`, `install <version>`, `use <version>`, `uninstall <version>`,
  `where`. Bare `crap-cms update` installs the latest release and
  switches to it. Versions live under `~/.local/share/crap-cms/versions/`;
  the `current` symlink flip is atomic (safe to switch while `serve` is
  running). Release assets are verified against `SHA256SUMS` before
  install. Distro-managed paths (`/usr/`, `/opt/`, `/nix/`) refuse
  self-update with a pointer at the system package manager; `--force`
  overrides. On `crap-cms serve` startup, a one-line notice prints
  when the cached update-check (24h TTL) shows a newer release is
  available — silenceable via `[update] check_on_startup = false` in
  `crap.toml`. Windows self-update (`install`/`use`) is not supported
  in this release — the version store uses symlinks. Windows users
  should download new releases manually; `check`/`list`/`where` still
  work.

- **Official shell installer** at `scripts/install.sh` — auto-detects
  platform (Linux x86_64 / aarch64), downloads the matching asset,
  verifies SHA256, lays out the nvm-style version store under
  `~/.local/share/crap-cms/`, wires up a shim at `~/.local/bin/crap-cms`,
  and prints a PATH hint if needed. Install via
  `curl -fsSL https://raw.githubusercontent.com/dkluhzeb/crap-cms/main/scripts/install.sh | bash`.

- **Top-level `hidden` field flag** — new `hidden = true` on
  `crap.FieldDefinition` strips a field from all read responses (gRPC,
  Lua, MCP, admin JSON, REST) and skips it in the admin form. Writes
  are not stripped — internal hooks/Lua can still write the column.
  This separates the two concerns that `admin.hidden` was overloaded
  to express: `admin.hidden` now controls admin-form rendering only
  (data still returned by the API, matching PayloadCMS's `hidden`
  semantic), while top-level `hidden` is the strict "do not return
  this anywhere" flag. Both flags are independent and composable.

- **`[live] transport = "redis"` for cross-node event fan-out** — new
  config key that pipes live-update mutation events and user-invalidation
  signals through Redis pub/sub instead of the default in-process
  `tokio::sync::broadcast` channel. Required for multi-node deployments
  so subscribers on any node see events published by any other node.
  Reuses `[cache] redis_url` (single source of truth); requires
  `--features redis` at build time. With the default `transport =
  "memory"`, a write on node A still only reaches subscribers connected
  to node A — fine for single-node or sticky-load-balanced setups, not
  for round-robin.

- **Rust typegen proto conversion** — `crap-cms typegen -l rs --proto <module>`
  generates `generated_proto.rs` with `from_document()` impls that extract
  fields directly from `prost_types::Struct` — no JSON intermediate, no
  serde deserialization. Depends only on `prost_types`, not `tonic`.
  Sub-types (array rows) get `from_struct()` methods. Handles all field
  types: text, number, checkbox, relationships, arrays with sub-fields,
  uploads, selects. Layout wrappers (Row, Collapsible, Tabs) are
  transparently promoted.

- **gRPC trash query** — `Find` and `FindByID` requests now accept an
  optional `trash` parameter. When `trash = true`, only soft-deleted
  documents are returned (sorted by `_deleted_at` descending by default).
  Uses `access.trash` permission (falls back to `access.update`) instead
  of `access.read`. Requires `soft_delete = true` on the collection.
  Previously, soft-deleted documents were only accessible through the
  admin UI.

- **Admin access harmonization** — The admin UI now delegates all access
  checks and field stripping to the service layer instead of duplicating
  them. Read-denied fields are completely hidden from edit forms (previously
  they rendered as empty form fields, leaking field existence). Removed
  redundant `strip_denied_fields` from admin handlers. The collection list,
  edit form (collections + globals), and delete confirmation page all go
  through the service layer with proper `ServiceError::AccessDenied`
  handling.

- **Configurable session cookie SameSite attribute** — new `[auth]
  session_cookie_samesite` key in `crap.toml` accepts `"lax"` (default),
  `"strict"`, or `"none"` (reserved; currently falls back to `Lax` with
  a runtime warning). Set to `"strict"` for hardened CSRF protection at
  the cost of breaking cross-site navigation (clicks from emails, external
  links, etc. will require re-login). The CSRF cookie itself remains
  hard-coded to `SameSite=Strict` regardless of this setting.

- **`crap.crypto.constant_time_eq(a, b)`** — new Lua-side helper that
  compares two strings in time independent of where (or whether) they
  differ, backed by the `subtle` crate. Required for verifying HMAC tags,
  signatures, or any secret value — using Lua's `==` operator on HMAC
  strings is timing-attack-vulnerable. The `crap.crypto.hmac_sha256`
  docs now point to this helper as the only correct verification path.

### Changed

- **User-invalidation signals now fire from the service layer** —
  `ServiceContext` carries an optional `invalidation_transport`; when
  set, `service::auth::lock_user` and `service::write::delete_document_core`
  (for hard-delete of auth collections) publish a user-invalidation
  signal so any active live-update streams owned by that user are torn
  down. Wired through admin handlers, gRPC handlers (lock, delete,
  delete_many, upload delete), MCP delete tool, empty-trash, and Lua
  CRUD (`crap.collections.delete` / `delete_many`). Lua VMs receive the
  transport via `LuaInvalidationTransport` app-data set at `HookRunner`
  build time. The previously-duplicated handler-side publishers in
  admin + gRPC handlers have since been removed — the service-layer
  chokepoint is now the single source of invalidation publishes.

- **Cross-request populate cache dedup** — `FindDocumentsInput` and
  `FindByIdInput` gained an optional `singleflight: Option<
  SharedPopulateSingleflight>` field plumbed through
  `PostProcessOpts` into `post_process`. gRPC find / find_by_id
  handlers thread the process-wide `Arc<Singleflight>` from
  `ContentServiceDeps` / `AdminState`. Lua CRUD paths read the shared
  singleflight from `LuaPopulateSingleflight` app-data. Combined with
  the `override_access` guardrail (see Fixed section), this closes
  concurrent requests across the process dedupe populate cache misses,
  while override-access fetches stay isolated. MCP tools hardcode `override_access = true` so the
  guardrail always bypasses their threading — intentionally skipped.

- **Docs + LuaLS annotations for `list_versions` / `restore_version`** —
  both functions are now documented in `docs/src/lua-api/collections.md`
  and typed in `types/crap.lua` (plus `example/types/crap.lua`) with the
  `crap.VersionSummary` shape and their `overrideAccess` opt. See the
  corresponding Fixed entry for the behaviour change.

- **BREAKING: filters on system columns (`_*`) are now rejected** —
  User-supplied `where` filters targeting field paths starting with `_`
  (e.g. `_deleted_at`, `_status`, `_ref_count`, `_password_hash`,
  `_locked`) now error with `InvalidArgument` / `HookError` instead of
  silently ANDing against automatically-injected exclusions or falling
  through. Applies to gRPC, Lua, admin URL query params, and MCP. Use
  the typed request flags (`trash = true`, `draft = true`) to access the
  data those columns represent. Previously, such filters could silently
  produce empty results (for drafts-enabled collections without the
  `draft` flag) or — in Lua bulk ops and gRPC bulk — bypass validation
  entirely. Validation moved into the service layer so all surfaces
  enforce the same rule. The allow-list for service-internal injection
  (`_status = "published"` when filtering to non-drafts, `_deleted_at
  EXISTS` when listing trash) is applied post-validation.

- **BREAKING: `AccessResult::Constrained` filter tables from write
  access hooks now enforce row-level matches** — An access hook for
  `update` / `delete` / `undelete` / `unpublish` that returns a filter
  table (e.g. `return { author_id = ctx.user.id }`) now causes the
  target row to be checked against those filters; the operation is
  denied if the row does not match. Previously the filter was silently
  dropped and the write proceeded unchecked — operators writing the
  natural "users can only modify their own rows" idiom were getting a
  no-op. This restores the intuitive semantic across reads + writes.
  On `create`, filter tables are now rejected with a clear error
  (`create` has no target row to match); use boolean returns with
  explicit `ctx.data` checks instead. On globals (single-row) and jobs
  (trigger-only), filter tables are likewise rejected with an
  operator-facing error. Version `restore` enforces against the parent
  document id.

- **BREAKING: relationship population omits soft-deleted / missing
  targets** — At `depth >= 1`, a has-one relationship whose target is
  soft-deleted or absent now resolves to `null` instead of leaking the
  raw ID string. Has-many relationships drop soft-deleted / absent
  entries from the array. Cycle-protection paths, malformed polymorphic
  refs, and unknown-collection refs still keep the original string.
  Applies to both single-doc and batch population, polymorphic and
  non-polymorphic.

- **Slow / lagged subscribers are dropped** — Live-update
  streams (gRPC Subscribe and admin SSE) now drop a subscriber when a
  per-event send takes longer than `subscriber_send_timeout_ms` (new
  `[live]` key, default `1000`). Subscribers that fall further behind
  than `channel_capacity` are also dropped on their next read; the
  previous behavior of holding lagged subscribers open with warnings
  masked silent event loss. Healthy subscribers are unaffected; dropped
  clients see a closed stream and must reconnect.

- **BREAKING: filter comparison operators are field-type-aware** —
  Comparison operators (`greater_than`, `less_than`, `gt`, `lt`, etc.)
  now bind their values as the field's actual SQL type (`INTEGER` /
  `REAL` / `TEXT`) instead of always `TEXT`. Number fields correctly
  compare numerically (previous lexicographic `"1000" < "50"` ordering
  is gone). Checkbox fields accept `"true"`/`"false"`/`"1"`/`"0"` and
  bind as integer. Date fields stay `TEXT` (ISO strings compare
  lexicographically). Text-only operators (`contains`, `starts_with`,
  `regex`) remain text. Invalid numeric inputs fall back to text with a
  runtime warning rather than panicking.

- **`search_documents` now mirrors `find_documents` draft-inclusion
  semantics** — `SearchDocumentsInput` gained an `include_drafts:
  bool` field. When `false` (default) on a drafts-enabled collection,
  the service injects `_status = "published"` so only published rows
  are returned — matching `find_documents`. The admin relationship
  picker passes `include_drafts = true` so operators can link to
  work-in-progress content. Previously search hard-coded a "permit
  `_status` Constrained filter" flag but never actually injected,
  producing inconsistent behaviour between find and search.

- **Cache stampede fix — singleflight on populate** — relationship
  population deduplicates concurrent cache-miss fetches for the same
  `(collection, id, locale)` key. Previously N concurrent requests
  for the same target each independently ran `find_by_id`; now the
  first arriver runs the query and later arrivers block on a shared
  slot, collapsing N DB hits to 1 under thundering-herd load.
  Dashmap-backed, sync-blocking. See the follow-up plumbing for
  cross-request dedup in the Changed section.

### Fixed

- **Admin upload edit page renders the image preview and focal-point
  selector again.** The admin access harmonization in this release had
  extended the service-layer field stripping to also strip every field
  marked `admin.hidden = true`, conflating "don't render in the admin
  form" with "remove from API output". Upload's auto-injected meta
  fields (`url`, `mime_type`, `filesize`, `width`, `height`,
  `focal_x`, `focal_y`, per-size variants) were marked `admin.hidden`
  to keep them out of the form, so the service stripped them — and the
  admin's upload preview widget (which also reads them via the service
  layer) got nothing to render. The two concerns are now split:
  `admin.hidden` is admin-form rendering only; the new top-level
  `hidden` is the API-stripping flag (see Added). Upload meta fields
  are restored to all API responses, fixing the missing image preview
  and unblocking gRPC/Lua/MCP consumers that need them.

- **[SECURITY] Join field population bypassed target-collection
  read access** — `populate_join_docs` was running raw `query::find`
  on the joined collection, skipping its `access.read` hook. A user
  allowed to read `post` but denied `author` reads could still see
  `author` data by inspecting the post's join field. Populate now
  checks the target collection's read access via a new
  `JoinAccessCheck` trait: `Denied` → empty array; `Constrained(...)`
  → validated and merged into the subquery; `Allowed` → proceeds. The
  guard is wired from `post_process` for every find/find_by_id result.

- **[SECURITY] Shared populate cache + singleflight leak across
  `override_access = true` boundaries** — Lua CRUD paths and MCP tools
  can set `override_access = true` to bypass access hooks. With the
  cross-request singleflight share landed in this release, a bypass
  fetch could write documents into the shared cache that another
  user's request would then read without their own access
  re-evaluation. Added a single-chokepoint guardrail at
  `service::read::post_process`: when `ctx.override_access == true`,
  both the populate cache and the singleflight are forced to `None`
  regardless of what the input carries. Override-access fetches still
  deduplicate within their own call via a fresh per-call singleflight,
  but never write to or read from shared state.

- **[SECURITY] Live-update streams not torn down on lock / hard-delete**
  — When a user was locked or hard-deleted via the service
  layer, their existing gRPC Subscribe and admin SSE streams kept
  receiving events with the original snapshotted access until the
  client disconnected on its own. Both surfaces now publish a
  per-user invalidation that closes affected streams immediately with
  `PermissionDenied`; the client must reconnect with a fresh token.
  Anonymous subscribers are not affected.

- **[SECURITY] Lua `list_versions` / `restore_version` bypassed
  collection access** — both functions hardcoded `override_access =
  true`, silently bypassing the collection's `read` / `update` access
  hooks. Now opt-in via `opts.overrideAccess` with a default of
  `false`, matching every other Lua CRUD method. Lua callers respect
  the configured access rules by default; trusted internal code
  (jobs, migrations) can still opt in explicitly.

- **`admin.access` gate not enforced at login** — Users who failed the
  `admin.access` check could still log in and receive a session cookie,
  only to see a 403 on every subsequent page. The access gate is now
  checked in the login handler before issuing the session. Denied users
  see the 403 immediately at login without a cookie being set.

- **ref_count race: dangling reference after concurrent hard-delete** —
  When a write incremented the reference count on a target document that
  had been hard-deleted concurrently, the increment silently failed
  (0 rows affected) and the caller's transaction committed with a
  dangling reference. Now the increment produces a hard error and the
  caller's transaction rolls back. Decrement-on-missing remains a
  tolerated no-op (the target is already gone; nothing to decrement).

- **Custom auth strategy errors silently swallowed** — If a custom
  authentication strategy hook returned an error (DB outage, bad config,
  Lua panic), the login flow silently fell through to the next strategy
  with no log output. Errors are now logged at `ERROR` level with the
  strategy reference and collection slug, then iteration continues.

- **[SECURITY] Email header injection via CRLF in `crap.email.send`** —
  `subject`, `to`, `from`, `cc`, `bcc`, and `reply_to` values are now
  rejected if they contain `\r`, `\n`, or NUL. Previously, a Lua hook
  that interpolated user-controlled data into `subject` could inject
  arbitrary SMTP headers (e.g., `subject = user .. "\r\nBcc: attacker"`
  silently BCC'd the attacker on every mail). The same validation is
  applied at the queued-email insertion point as defense-in-depth.

- **[SECURITY] HTTP SSRF blocklist no longer leaks internal IPs** —
  When `crap.http.request` is blocked by the SSRF policy, the Lua error
  is now a generic "Target resolves to a blocked address; see server
  logs for details". The resolved IP + blocklist class continue to be
  logged at `warn!` level for operators. Previously the error message
  named the IP, which allowed a caller to enumerate internal topology
  via the error channel.

- **MCP HTTP `api_key` empty behavior clarified** — when `mcp.http =
  true` but `api_key` is empty, the server still starts and registers
  the route, but every request to `POST /mcp` is rejected with 401. The
  previous docs claimed the server would refuse to start — that was
  wrong. The per-request check is still defense-in-depth; operators
  should verify the key is set before enabling HTTP.

- **Lua `crap.collections.delete` ignored `forceHardDelete` on
  soft-delete collections** — the option was parsed but never flipped
  `def.soft_delete = false` before calling the service layer, so
  `forceHardDelete = true` silently soft-deleted rows regardless.
  Fixed to mirror the existing pattern in gRPC single/bulk delete
  and admin empty-trash.

- **Configuration parser silently accepted unknown TOML keys** —
  config structs lacked `#[serde(deny_unknown_fields)]`, so typos
  like `[servr]` or `admin_prot = 3000` passed silently and operators
  would spend hours debugging "why isn't my setting applying". Added
  `deny_unknown_fields` to 20 config structs across `src/config/`;
  startup now fails fast on unrecognised keys with an error that
  names the offending key.

- **Parser integer overflow in filesize / duration / trash-purge
  inputs** — `parse_filesize_string` / `parse_duration_string` /
  `parse_older_than` multiplied without checked arithmetic; absurd
  inputs (e.g. `"10000000GB"`, `"99999999999999999999d"`) silently
  overflowed to small or negative values, changing pool sizes,
  timeouts, or purge windows. All three now use `checked_mul` and
  return a clear error on overflow.

- **Field-definition parsing silently accepted duplicate field
  names** — two fields with the same name at the same nesting level
  produced a single column in the generated DDL (the second
  overwrote the first). Parse-time validation now errors with the
  offending name; the check flattens through transparent layout
  wrappers (`Row`, `Collapsible`, `Tabs`) so a sibling field and a
  nested-in-Row field with the same name also collide.

- **Field-config `get_bool` helper silently defaulted on wrong type** —
  a Lua typo like `required = "yes"` (string) parsed as `false`
  instead of erroring. Now returns `LuaResult<bool>` with a clear
  type-mismatch error naming the key and the offending type.

- **Hook / access references validated at startup, not at first
  call** — misspelled refs like `hooks.article.auto_slug` used to
  surface only when a user triggered the corresponding request.
  Startup now walks every collection + global + field-level
  `hooks.*` / `access.*` string and fails fast with a line-by-line
  report of unresolved refs. Job handlers, auth strategies,
  richtext attribute hooks, and dynamic `crap.hooks.register`
  registrations are intentionally out of scope (they have separate
  resolvers or are runtime-dynamic).

- **`crap-cms user create` accepted malformed email addresses** —
  the CLI wrote whatever string the operator supplied into the auth
  collection, breaking downstream password-reset and email-verify
  flows. Now validates format via the same helper used by the
  `email` field type.

- **Config file world-readable warning** — on startup, if `crap.toml`
  contains a non-empty secret (`auth.secret`, `email.smtp_pass`,
  `upload.s3.secret_key`) AND the file's Unix permissions allow
  world-read or world-write, a `warn!` is emitted recommending
  `chmod 600`. Windows: skipped.

- **Null-byte injection in text / textarea / email fields** — user
  input containing `\0` reached SQLite TEXT storage and broke
  downstream display / log handling. Text, textarea, and email
  coercion paths now reject `\0` with a clear per-field error
  naming the offending field.

- **Locale-suffix field-name collision detection** — a literal field
  named `title__en` defined while `en` is an enabled locale would
  collide with the generated localized column for `title`. Startup
  now walks every field (including nested groups / blocks / tabs)
  against the configured locales and fails fast with a clear error.

- **`crap-cms backup` errored mid-run on read-only output dir** —
  the backup started `VACUUM INTO` then failed on the manifest
  write, leaving a partial backup the operator had to clean up. Now
  probes the output directory with a temp file before any long-
  running work; fails early with a clear message.

- **`SIGTERM` shutdown exit code** — the detached-mode serve process
  called `std::process::exit(0)` unconditionally after cleanup, so
  Kubernetes / systemd saw "success" even when WAL checkpoint or
  pool-get failed. Shutdown cleanup now collects errors and the
  process exits with `1` when any cleanup step failed.

- **Version restore silently dropped unknown snapshot keys** — if a
  collection field was deleted after a version was created,
  restoring that version inserted the snapshot without warning about
  the dropped keys. Now emits a `warn!` per unknown key naming the
  collection, version, and key — silent-drop behavior preserved
  so the restore still succeeds, just with visibility.

- **Retention auto-purge ran on every node without dedup** — with
  multiple scheduler instances (multi-server), the soft-delete
  retention purge fired on each node. Now claims a cron window via
  `try_claim_cron_window` (the same mechanism cron jobs use) so only
  one node runs the purge per window.

- **`_ref_count` could double-increment on has-many with duplicate
  IDs** — `extract_has_many_refs` walked the raw JSON input array
  without deduplication, so `tags = ["a", "a", "b"]` incremented
  target `a`'s ref_count twice before the junction-table UNIQUE
  constraint rejected the second row. Now dedupes the ID list first;
  `collect_has_many_refs` also uses `SELECT DISTINCT` as
  defense-in-depth against any pre-existing dirty junction rows.

- **Localized filter on array sub-field routed to the wrong column** —
  a dot-notation filter like `links.label` where `label` is a
  localized sub-field inside an array field did not route to the
  `_locale`-suffixed column and did not add a `_locale = ?`
  constraint to the EXISTS subquery. A locale-scoped filter under
  `Single("de")` matched documents whose value only appeared in `en`.
  Now threaded through `resolve_filter` → `build_subquery_sql`:
  `ResolvedFilter::Subquery` carries a `locale_constraint` that the
  SQL builder appends when set.

### Documentation

- `crap.auth.user()` now documented in `lua-api/auth.md` with return
  shape, nil conditions, and usage examples.
- `before_broadcast` and `before_render` hook events now documented in
  `hooks/lifecycle-events.md` with fire sites, context shapes, return
  value semantics, and examples.
- Decompression-bomb protection (100-megapixel hard limit for image
  uploads) documented in `uploads/image-processing.md`.
- Filter-operator docs rewritten to reflect the field-type-aware
  coercion landed in this release (previously claimed all values were
  coerced to strings).
- HTTP TLS verification (always on, no opt-out) noted in
  `lua-api/http.md`.
- JSON integer-precision caveat (>2^53 loses precision) + recursion
  depth limits noted in `lua-api/json.md`.
- `crap.config` snapshot-per-VM lifecycle clarified in
  `lua-api/config.md`.
- Custom `richtext.register_node` render functions explicitly
  documented as NOT sanitized — operators must escape interpolated
  user data themselves. Added safe / unsafe pattern examples.
- Plugin load order (collections → globals → jobs → init.lua,
  alphabetical within each) explicitly documented in
  `plugins/overview.md`.
- Field-level access denial is silent (no client-facing error) —
  documented in `access-control/field-level.md`.
- Job retry backoff schedule (exponential, capped at 5 min) documented
  in `jobs/overview.md`.
- **Missing gRPC RPCs documented** — `Validate`, `LockAccount`,
  `UnlockAccount`, `VerifyAccount`, and `UnverifyAccount` are now
  covered in `grpc-api/rpcs.md` with request/response shapes,
  `grpcurl` examples, and access requirements. They were defined in
  `proto/content.proto` and live in the running server but were absent
  from the public reference.
- **`live` metadata-mode hook overhead claim corrected** — The live
  updates overview previously claimed `metadata` mode had "zero hook
  overhead". In reality, `before_broadcast` (and the `live` filter
  function, when configured) still run; only the per-subscriber
  `after_read` hooks and field-level read-access stripping on the
  event payload are skipped. Documentation now reflects this.

- **Plugin load order clarification** — documentation now explicitly
  describes the file load order (`collections/` → `globals/` →
  `jobs/` → `init.lua`, all alphabetical within each kind) and the fact
  that plugin `require()` order in `init.lua` is operator-controlled.

- **Job retry backoff documented** — the exponential backoff formula
  (`min(2^(attempt - 1) * 5, 300)` seconds, capped at 5 minutes) is now
  visible in the docs instead of being a runtime surprise.

- **Multi-node file storage corrected** — `deployment/multi-server.md`
  previously listed shared filesystems (NFS / EFS) as a viable option
  for multi-node file storage. They are **not supported** — `storage =
  "local"` assumes a single writer and the code is not tested against
  networked-filesystem fsync / locking semantics. S3-compatible object
  storage (AWS S3, MinIO, Cloudflare R2, Backblaze B2, etc.) is the
  only supported multi-node option.

- **Multi-node rate limiting promoted to required** — shared Redis
  rate limits were previously framed as "recommended for performance".
  They are now documented as a **security requirement**: without them,
  per-IP login rate limits are per-node counters, and an attacker who
  round-robins across nodes multiplies their allowance by the node
  count (e.g. a 5-attempt limit across 3 nodes becomes 15 attempts).

- **Multi-node live updates rewritten** — `deployment/multi-server.md`
  now documents both `transport = "memory"` (default, single-node or
  sticky-LB) and `transport = "redis"` (cross-node fan-out), with the
  trade-offs for each. Cross-links to `live-updates/overview.md`.

- **Load-balancer stickiness requirements documented** — gRPC Subscribe
  / Admin SSE streams are long-lived and benefit from sticky sessions;
  regular HTTP / gRPC unary calls can round-robin freely. Even with
  `transport = "redis"`, reconnects to a different node lose the
  in-flight subscription context and the client must re-subscribe.

- **PostgreSQL backend visibility** — `database/overview.md` now leads
  with both SQLite and PostgreSQL as first-class backends instead of
  treating PostgreSQL as a footnote. Feature parity (FTS, schema sync,
  migrations, ref_count, soft delete) is called out explicitly.

- **Redis auth / TLS documented** — `internals/cache.md` now describes
  how to encode credentials and TLS into the Redis URL (`redis://user:
  pass@host`, `rediss://` for TLS, ACL user syntax for Redis 6+).
  There is no separate `[cache] password` or `[cache] tls` key.

- **Single-server log path + rotation documented** —
  `deployment/single-server.md` now explains that `--detach` auto-
  enables file logging (since the child has no terminal), gives the
  default log location (`<config_dir>/data/logs/`), rotation policy
  (daily, 30-file retention), and how to read logs (`crap-cms logs`
  or tail the files). Notes `--json` for structured output.

- **Cache stampede known-limitation note** — `internals/cache.md`
  documents the cache-miss coalescing behaviour operators should
  expect now that singleflight is active: cache-miss load on the
  same key under heavy concurrency collapses to a single DB query;
  later arrivers block on a shared slot. Also documents the
  override-access isolation invariant.

## [0.1.0-alpha.4] — 2026-04-11

### Changed

- **BREAKING: `default_deny` now defaults to `true`** — Collections and
  globals without explicit access functions now **deny all operations** by
  default. This is a secure-by-default change. Previously, missing access
  functions allowed all operations (`default_deny = false`). To restore
  the old behavior, set `default_deny = false` in `[access]` in
  `crap.toml`. Every collection and global in production should have
  explicit access rules defined.

- **Invalid locale now returns an error** — API requests (gRPC, Lua CRUD)
  with an invalid locale code now receive `INVALID_ARGUMENT` /
  `RuntimeError` instead of silently falling back to the default locale.
  Valid locale codes are those listed in `[locale] locales` in
  `crap.toml`, plus the special value `"all"`. Passing no locale still
  defaults to the default locale.

### Added

- **MCP locale support** — `find` and `find_by_id` MCP tools now accept
  an optional `locale` parameter for querying locale-specific data,
  matching the gRPC API's locale support.

- **Per-collection ref count backfill** — The `_ref_count` backfill
  migration now tracks which collections have been backfilled
  individually. Adding a new collection to the config no longer requires
  manually resetting the `ref_count_backfilled` flag — the backfill runs
  automatically for newly added collections on the next startup.

- **Event delivery modes** — per-collection `live` setting now supports a
  `mode` field (`"metadata"` or `"full"`) that controls what data events
  carry. `metadata` (default) sends only event metadata (sequence,
  operation, collection, document_id) with zero hook overhead — clients
  re-fetch via `FindByID` if needed. `full` mode sends complete document
  data processed through `after_read` hooks and field-level access
  stripping, matching the exact same data a `Find` call returns. Configure
  per collection: `live = { mode = "full" }`. Global default configurable
  via `[live] default_mode` in `crap.toml`.

- **Event stream access control** — SSE and gRPC Subscribe streams now
  enforce the same access rules as normal read operations:
  - Collection-level access (cached at connection time)
  - Row-level constrained access (in-memory filter evaluation per event,
    using the same constraint filters as `Find` SQL WHERE clauses)
  - Field-level read access stripping (in `full` mode, per subscriber)
  - `after_read` hooks (in `full` mode, per subscriber)

- **SSE events now include `data` field** — SSE mutation events now carry
  document data (respecting the collection's delivery mode and access
  control). Previously SSE sent metadata only. This enables custom admin
  UI themes to use real-time document data without re-fetching.

- **In-memory filter evaluation** — new `matches_constraints()` function
  evaluates `FilterClause` types against `HashMap<String, Value>` data
  in-memory. Supports all filter operators (Equals, NotEquals, Contains,
  Like, GreaterThan/LessThan, In/NotIn, Exists/NotExists, Or groups).
  Used by event streams for row-level access control without DB queries.

- **BREAKING: "Restore" renamed to "Undelete" for trash operations** —
  The operation that un-deletes a soft-deleted document is now called
  "undelete" everywhere to distinguish it from "restore version" (which
  reverts a document to a previous snapshot). Affected APIs:
  - gRPC: `rpc Restore` → `rpc Undelete`, `RestoreRequest` → `UndeleteRequest`,
    `RestoreResponse` → `UndeleteResponse`
  - Lua: `crap.collections.restore()` → `crap.collections.undelete()`
  - Admin URL: `/admin/collections/{slug}/{id}/restore` →
    `/admin/collections/{slug}/{id}/undelete`
  - Version restore operations (`RestoreVersion`, `restore_collection_version`,
    etc.) are unchanged.

- **Service layer unification** — all database operation flows now go through
  a shared service layer (`src/service/`), ensuring consistent access control,
  field-level permissions, validation, hydration, and error handling across
  all 4 API surfaces (admin, gRPC, MCP, Lua hooks). Key additions:
  - `ServiceError` with 12 typed variants replacing string-based error matching
  - `WriteHooks::check_access` / `field_write_denied` / `field_read_denied`
    for unified access control inside service operations
  - `service::auth` module for authentication, password reset, email verification
  - `service::version_ops` for version restore/list
  - `service::document_info` for ref counts and back-references
  - `service::user_settings` for per-user preferences
  - `service::jobs` for job queue/run operations with access control
  - `service::upload` for file upload orchestration
  - Write operations now hydrate + strip read-denied fields before returning

- **ServiceContext API harmonization** — every service function now follows
  `fn operation(ctx: &ServiceContext, input)` with a unified calling
  environment. `ServiceContext` carries connection (pool or direct),
  hooks, user identity, slug, and definition. Eliminates all
  `#[allow(clippy::too_many_arguments)]` from the service layer, all
  `_with_conn` variants, and all loose parameter passing. Dedicated input
  structs (`FindDocumentsInput`, `CountDocumentsInput`, `WriteInput`,
  `QueueJobInput`, etc.) carry operation-specific data.

- **Unified pagination** — all multi-result service functions now return
  `PaginatedResult<T>` with docs, total count, and computed pagination
  metadata (page or cursor mode). Pagination logic is built inside the
  service layer — callers no longer duplicate `PaginationResult`
  construction. Affected: `find_documents`, `search_documents`,
  `list_versions`, `list_job_runs`.

### Fixed

- **BREAKING: `admin.hidden` fields now stripped from API responses** —
  Fields with `admin.hidden = true` are no longer returned in gRPC, MCP,
  or Lua API responses. Previously, `hidden` only affected admin form
  rendering. This aligns with PayloadCMS behavior where hidden fields are
  excluded from all responses. Upload metadata fields (`url`,
  `mime_type`, `width`, `height`, `filesize`, `filename`) that were
  auto-injected as `hidden` are affected — if your frontend relies on
  these in API responses, remove `admin.hidden` from the field definition
  or stop marking them hidden in your upload config.

- **Group subfield access stripping** — read-denied fields inside groups
  (e.g., `address.city` with read access denied) are now correctly
  stripped from API responses after hydration. Previously, group subfields
  stored as `address__city` were stripped before hydration but became
  nested `{"address": {"city": ...}}` after hydration, bypassing the
  strip. `Document::strip_fields()` now handles `__`-separated paths at
  any nesting depth.

- **Missing field stripping** — `undelete_document`, `restore_version`
  (collection and global), `search_documents`, and `find_version_by_id`
  now strip read-denied and hidden fields before returning. Previously
  these functions returned unstripped documents.

- **Version snapshot access control** — `find_version_by_id` now checks
  read access and strips denied fields from the snapshot JSON. Previously
  version snapshots were returned without access checks.

- **Redundant proto-level stripping removed** — gRPC handlers no longer
  perform a second pass of field stripping at the protobuf level. The
  service layer handles all field access control, eliminating redundant
  Lua VM access checks and unnecessary transaction opens per response.

- **Surface parity** — all API surfaces now expose a consistent set of operations:
  - MCP: added `undelete`, `count`, `unpublish`, `list_versions`, `restore_version` tools
  - Lua: added `crap.collections.unpublish()`, `list_versions()`, `restore_version()`,
    `ref_count()`, `validate()` functions
  - gRPC: added `Validate`, `LockAccount`, `UnlockAccount`, `VerifyAccount`,
    `UnverifyAccount` RPCs
  - Lua access API: `crap.auth.user()`, `crap.access.check()`,
    `crap.access.field_read_denied()`, `crap.access.field_write_denied()`
  - Lua `crap.jobs.queue()` now checks job access control

- **Module restructuring** — service layer and all surfaces restructured into
  consistent subdirectory hierarchy matching domain concerns:
  - `service/` — 11 subdirectories: types, hooks, read, write, collection,
    globals, persist, versions, jobs, plus auth, upload, user_settings, document_info
  - `api/service/` renamed to `api/handlers/` to avoid confusion with service layer
  - MCP tools split into `collection/{read,write}/`, `globals/`, `schema/`
  - Lua CRUD split into `collection/{read,write,bulk,versions}/`, `globals/`, `jobs/`
  - Admin `forms.rs` (912 lines) → `forms/`; `query_utils.rs` (518 lines) → `query/`
  - gRPC `convert.rs` (881 lines) → `convert/` with document, data, filters, schema

- **Code quality** — namespaced macro calls replaced with top-level imports
  across service + all surfaces (tracing, anyhow, nanoid). Removed unused
  `rpassword` dependency.

- **Internal code quality refactoring** — large files split into focused
  modules following one-handler-per-file and no-logic-in-mod.rs rules.
  Key restructurings:
  - `admin/handlers/shared.rs` → `shared/` module (access, document,
    locale, pagination, response, versions)
  - `admin/server.rs` → extracted `auth_middleware.rs` and `mcp_handler.rs`
  - `admin/templates/mod.rs` → extracted `registry.rs`
  - `api/service/schema_ops.rs` → split into `globals/`, `schema/`,
    `subscribe.rs`, `collection/versions/`, `jobs/`
  - `api/service/collection/{read,write,bulk}.rs` → split into module
    folders with one handler per file
  - `api/service/auth.rs` → split into per-handler files
  - `api/upload.rs` → split into `upload/` module with shared helpers
  - Extracted shared helpers: `evaluate_condition_results`,
    `extract_doc_status`, `load_version_with_missing_relations`,
    `publish_mutation_event`, `strip_read_denied_proto_fields`
  - Eliminated duplicated access check, event publishing, and field
    stripping patterns across admin and API handlers
  - Added `FindQueryBuilder` for `FindQuery` (9 fields, was using
    manual field assignment)

- **Optional PostgreSQL backend** — Crap CMS now supports PostgreSQL as
  an alternative database backend, available via the `postgres` Cargo
  feature flag. SQLite remains the default and recommended backend for
  most deployments.

  **Why SQLite is the default:** Crap CMS is designed for simplicity.
  SQLite requires zero infrastructure — no database server, no
  connection strings, no Docker, no backups to configure. The entire
  database is a single file you can copy, move, or version-control.
  For the vast majority of CMS deployments (content sites, editorial
  teams, headless API backends), SQLite handles thousands of concurrent
  readers and hundreds of writes per second — more than enough.

  **When to consider PostgreSQL:** Multi-server deployments where
  multiple instances need to share a database, or workloads with 50+
  simultaneous writers (rare for a CMS). PostgreSQL also provides
  better read performance under extreme concurrency (50+ concurrent
  requests) due to MVCC.

  **Build & configure:**
  ```bash
  cargo build --features postgres       # both backends
  cargo build --no-default-features --features postgres  # PG only
  ```
  ```toml
  [database]
  backend = "postgres"
  url = "host=localhost user=crap dbname=crap_cms"
  ```

  The `sqlite` and `postgres` feature flags are independent — both can
  be compiled in and switched at runtime via `crap.toml`. The `r2d2`
  dependency is now optional (only pulled with `sqlite`). PostgreSQL
  uses `deadpool-postgres` with `tokio-postgres` for async-native
  connection pooling.

  Postgres-specific implementation details:
  - Full-text search uses `tsvector`/`tsquery` with GIN indexes
    (SQLite uses FTS5)
  - Timestamps stored as ISO 8601 TEXT (matching SQLite behavior)
  - `SET timezone = 'UTC'` enforced on every connection
  - DDL automatically adjusts `INTEGER` to `BIGINT` via dedicated
    `execute_ddl`/`execute_batch_ddl` methods
  - Connection recycling uses `DISCARD ALL` for clean state
  - `VACUUM INTO` not supported (use `pg_dump` for backups)

- **Storage backend abstraction** — Upload file storage is now pluggable
  via a `StorageBackend` trait with three implementations:

  - **`local`** (default) — Local filesystem, identical to previous
    behavior. Zero config, files in `{config_dir}/uploads/`.
  - **`s3`** (feature-flagged) — S3-compatible storage for multi-server
    deployments. Works with AWS S3, MinIO, Cloudflare R2, Backblaze B2,
    DigitalOcean Spaces. Enable with `--features s3-storage`.
  - **`custom`** — Delegates storage operations to user-provided Lua
    functions via `crap.storage.register()`. For exotic providers
    (Azure Blob, GCS, custom APIs) without adding SDK dependencies.

  The entire upload pipeline (upload, serve, resize, delete, deferred
  image conversion) now goes through the storage trait. File serving
  uses `tower_http::ServeFile` for local storage (Range, ETag,
  conditional GET) and streams from the backend for non-local storage.

  ```toml
  [upload]
  storage = "s3"

  [upload.s3]
  bucket = "my-uploads"
  region = "us-east-1"
  endpoint = "http://minio.example.com:9000"
  access_key = "${AWS_ACCESS_KEY}"
  secret_key = "${AWS_SECRET_KEY}"
  path_style = true
  ```

- **Auth redesign: TokenProvider + PasswordProvider + strategy chain** —
  Authentication infrastructure is now cleanly separated:

  - **`TokenProvider` trait** — JWT token creation/validation.
    Default: `JwtTokenProvider`. Rarely swapped.
  - **`PasswordProvider` trait** — Argon2id password hashing/verification.
    Default: `Argon2PasswordProvider`. Rarely swapped.
  - **Strategy chain** — `local` (email+password) is the built-in
    strategy. Per-collection Lua strategies are tried as fallback
    when local auth fails or is disabled (`disable_local = true`).
    No monolithic "auth provider" — authentication is orchestration
    in handlers, not a trait.

  This design supports OAuth2, Cloudflare Access, Active Directory,
  API keys, and any custom auth via **Lua strategies** — without
  the binary needing provider-specific code.

- **Built-in email MFA** — Auth collections can enable email-based
  multi-factor authentication:

  ```lua
  auth = {
      mfa = "email",  -- sends 6-digit code after password verification
  }
  ```

  After password/strategy authentication succeeds, a 6-digit code is
  emailed to the user. The admin UI shows a code input form; the user
  enters the code to complete login. Codes expire after 5 minutes and
  are single-use.

- **Auth callback route** — New catch-all route
  `GET/POST /admin/auth/callback/{name}` dispatches to Lua hooks,
  enabling OAuth2/OIDC callback flows implemented entirely in Lua:

  ```lua
  -- hooks/auth_callback/google.lua
  function M.google(ctx)
      local code = ctx.headers["_query_code"]
      local tokens = exchange_code(code)
      local userinfo = get_userinfo(tokens.access_token)
      local users = crap.find("users", { where = { email = userinfo.email } })
      if #users > 0 then return users[1] end
      return crap.create("users", { email = userinfo.email })
  end
  ```

- **Multi-server scheduler safety** — Job queue is now safe for
  multi-server deployments:

  - **Cron dedup** — New `_crap_cron_fired` table prevents cron jobs
    from double-firing when multiple servers run the scheduler. Uses
    an atomic upsert to claim each cron window.
  - **Atomic job claiming (Postgres)** — Uses `FOR UPDATE SKIP LOCKED`
    for lock-free atomic claiming. Workers skip rows being claimed by
    other workers. Per-slug concurrency limits are enforced inside the
    query (not in-memory).
  - **Atomic job claiming (SQLite)** — Claim operations now run inside
    an IMMEDIATE transaction, serializing concurrent workers. Per-slug
    concurrency counts are read from the DB inside the transaction.

- **Rate limit backend abstraction** — Login and gRPC rate limiters now
  support pluggable backends via a `RateLimitBackend` trait:

  - **`memory`** (default) — In-process sliding window counters. Same
    behavior as before.
  - **`redis`** (feature-flagged) — Shared rate limits across servers
    using Redis sorted sets. Requires `--features redis`.
  - **`none`** — Rate limiting disabled.

  Multi-server deployments should use `redis` to prevent attackers from
  bypassing rate limits by hitting different servers.

  ```toml
  [auth]
  rate_limit_backend = "redis"
  # rate_limit_redis_url defaults to cache.redis_url if empty
  rate_limit_prefix = "crap:rl:"
  ```

- **Cache backend abstraction** — The cross-request populate cache is now
  pluggable via a `CacheBackend` trait with four implementations:

  - **`memory`** (default) — In-memory DashMap with configurable soft
    entry cap. Good for single-server deployments.
  - **`redis`** (feature-flagged) — Shared Redis cache for multi-server
    deployments. Enable with `--features redis`. Uses key prefixing for
    namespace isolation.
  - **`none`** — No-op cache. Disables cross-request caching entirely.
  - **`custom`** — Lua-delegated cache backend (planned, not yet
    implemented — uses `none` as placeholder).

  **Breaking:** The `depth.populate_cache` and
  `depth.populate_cache_max_age_secs` config options have been replaced
  by a new `[cache]` section. Migration:
  - `populate_cache = true` → `[cache] backend = "memory"`
  - `populate_cache = false` → `[cache] backend = "none"`
  - `populate_cache_max_age_secs = 60` → `max_age_secs = 60`

  ```toml
  [cache]
  backend = "memory"       # "memory", "redis", "none", "custom"
  max_entries = 10000      # soft cap for memory backend
  max_age_secs = 60        # periodic full clear (0 = disabled)
  # redis_url = "redis://127.0.0.1:6379"
  # prefix = "crap:"
  ```

- **Email provider abstraction** — Email sending is now pluggable
  via an `EmailProvider` trait with four implementations:

  - **`smtp`** (default) — SMTP via `lettre`, identical to previous
    behavior. Falls back to `log` provider if `smtp_host` is empty.
  - **`webhook`** — HTTP POST to any URL. Works with SendGrid,
    Mailgun, Resend, or any API that accepts JSON email payloads.
    Configure with `webhook_url` and `webhook_headers`.
  - **`log`** — Logs emails to tracing instead of sending. Useful
    for development and testing.
  - **`custom`** — Delegates to a Lua function registered via
    `crap.email.register({ send = function(opts) ... end })`.

  ```toml
  [email]
  provider = "webhook"
  webhook_url = "https://api.sendgrid.com/v3/mail/send"
  webhook_headers = { Authorization = "Bearer ${SENDGRID_API_KEY}" }
  from_address = "noreply@example.com"
  ```

- **`crap-cms work` standalone worker command** — New top-level command
  that runs a dedicated job worker without HTTP/gRPC servers. Supports
  `--queues` (filter by queue name), `--concurrency` (override max
  concurrent jobs), `--no-cron` (skip cron scheduling), and
  `--detach`/`--stop`/`--restart`/`--status` for background management.
  Enables multi-server deployments where app servers run
  `serve --no-scheduler` and dedicated workers process jobs.

- **Queued email delivery with retries** — New `crap.email.queue(opts)`
  Lua API queues emails as jobs for async delivery with automatic
  retries on failure. Uses the existing job system with exponential
  backoff (5s, 10s, 20s, ..., max 300s). Configurable via
  `queue_retries` (default 3), `queue_name` (default `"email"`), and
  `queue_timeout` (default 30s) in `[email]` config. System email jobs
  (`_system_email`) execute directly in Rust without Lua VM overhead.
  `crap.email.send()` remains available for immediate blocking delivery.

- **Access control bypass in bulk delete and empty trash** — The gRPC
  `DeleteMany` handler and the admin "empty trash" handler created
  `RunnerWriteHooks` without `.with_conn(&tx)`, causing all access
  checks inside `delete_document_core` to short-circuit to `Allowed`.
  Any authenticated user could bulk-delete or permanently purge trashed
  documents regardless of configured permissions. Now both paths pass
  the transaction connection to WriteHooks.

- **Version restore missing access control in service layer** — The
  `restore_collection_version` and `restore_global_version` service
  functions did not check update access. The gRPC handler had its own
  check, but admin and MCP handlers did not, allowing any authenticated
  admin user to restore any version. Access check now lives in the
  service layer, enforced for all callers.

- **Ref count race on Postgres** — Under Postgres's `READ COMMITTED`
  isolation, a concurrent create and delete could race: the delete reads
  `_ref_count = 0` while the create's increment is still in flight,
  allowing deletion of a document that is about to be referenced. Fixed
  by acquiring `SELECT ... FOR UPDATE` row locks on referenced targets
  **before** any writes (INSERT/UPDATE), and on the document's own row
  before checking `_ref_count` in the delete path. This serializes
  concurrent create+delete on the same target row. On SQLite this is a
  no-op — `BEGIN IMMEDIATE` already serializes all write transactions.

- **Potential panics in CLI commands** — Several CLI code paths used
  infallible indexing or `.expect()` that could panic on edge cases:
  `trash.rs` used `HashMap[key]` instead of `.get()` (panics if
  collection removed between validation and access); `work.rs` used
  `.unwrap()` on PID conversion (panics if PID > i32::MAX);
  `user/helpers.rs` used `.expect()` on user selection index. All
  replaced with proper error propagation.

- **Rate limiter mutex poisoning could crash server** — The in-memory
  rate limiter used `.expect()` on `Mutex::lock()`, which panics if the
  mutex is poisoned. Now recovers from poison via `unwrap_or_else`.

- **Broadcast stream lag silently ignored** — SSE and gRPC Subscribe
  streams logged subscriber lag at `warn` level (or not at all for SSE)
  with no actionable guidance. Upgraded to `error` with a message
  recommending `[live] channel_capacity` increase.

- **Timestamp expiry overflow** — JWT token expiry was computed as
  `timestamp as u64 + expiry` without overflow protection. Now uses
  `.max(0) as u64` and `saturating_add()` in all 4 production code
  paths (gRPC login, admin login, MFA pending token, session refresh).

- **Invalid locale silently accepted** — `LocaleContext::from_locale_string`
  returned `None` for both "localization disabled" and "invalid locale
  code", making it impossible for callers to distinguish the two cases.
  Invalid locales now produce an error. Affects all API surfaces (gRPC,
  Lua hooks). Admin UI form submissions gracefully fall back to the
  default locale (locales are validated upstream from cookies).

- **Job pagination offset unbounded** — `ListJobRuns` accepted arbitrary
  `offset` values including negative numbers. Now clamped to `>= 0`.

- **MCP tools missing locale** — MCP `find` and `find_by_id` tools did
  not support the `locale` parameter, unlike their gRPC counterparts.
  Claude (via MCP) could not query locale-specific data.

- **Ref count backfill skipped for new collections** — The one-time
  `_ref_count` backfill was gated by a global flag in `_crap_meta`. If a
  new collection was added after the initial backfill, its documents'
  incoming reference counts were never computed. Now tracked
  per-collection.

- **README field type count** — README stated "14 field types" but the
  actual count is 20 (text, number, textarea, richtext, select, radio,
  checkbox, date, email, json, code, relationship, upload, array, group,
  blocks, row, collapsible, tabs, join).

- **`[live] default_mode` missing from docs** — The `default_mode`
  configuration option was documented in the live-updates overview but
  missing from the `crap-toml.md` configuration reference table. Now
  listed in both the example block and the reference table.

- **Event stream data leak** — gRPC Subscribe previously sent full
  document data without applying field-level read access checks. Events
  now respect the same field access rules as `Find` and `FindByID`.

- **Upload API event data ordering** — upload create/update handlers now
  strip field-level read-denied fields before publishing events, ensuring
  event data never contains fields the publishing user's access would deny.

- **Upload file deletion broken on localized collections** — The
  delete cleanup path used `LocaleConfig::default()` (empty) instead
  of the actual locale config when loading the document for file URL
  extraction. On collections with localized fields, the SELECT query
  referenced bare column names (`caption`) instead of locale-suffixed
  ones (`caption__en`), causing the query to fail. Upload files were
  silently orphaned on deletion. Now uses the correct locale config.

- **Deferred image conversions not cancelled on document delete** —
  When an upload document was deleted, pending image queue entries
  (deferred AVIF/WebP conversions) were not cleaned up. The scheduler
  would attempt to process them, fail because the source was deleted,
  and waste retries. Now cancels pending entries in all delete paths:
  single delete, bulk DeleteMany, Lua delete/delete_many, empty trash,
  and auto-purge.

- **Bulk DeleteMany missing upload file cleanup for image queue** —
  The gRPC `DeleteMany` and Lua `delete_many` did not cancel pending
  image queue entries for deleted documents. Now cleans up alongside
  the existing upload file deletion.

- **Debug logs shown in production** — The default stdout log filter
  for `serve` was `crap_cms=debug,info`, flooding production logs with
  debug output. Now defaults to `crap_cms=info` for production and
  `crap_cms=debug,info` only when `dev_mode = true`. File logging
  retains debug level for diagnostics. Override with `RUST_LOG` env
  var when needed.

- **SHA256 checksums missing from releases** — The release workflow
  now generates a `SHA256SUMS` file and uploads it alongside the
  binaries, enabling the install script to verify downloads
  automatically.

- **gRPC write handlers acquired two pool connections** — The gRPC
  Create, Update, and Delete handlers acquired a connection for
  auth/access checks, dropped it, then the service layer acquired a
  second one. Under high concurrency this caused pool exhaustion and
  5+ second latencies. Now reuses the same connection via
  `_with_conn` service variants, halving pool pressure on writes.

- **SQLite performance defaults too conservative** — Added configurable
  `cache_size` (default 16MB), `mmap_size` (default 256MB),
  `temp_store = MEMORY`, and `wal_autocheckpoint` to `[database]`
  config. Previous defaults used SQLite's 2MB cache with no
  memory-mapping. Pool `max_size` default raised from 32 to 64.

- **Lua VM pool capped at 32 regardless of hardware** — The auto-sized
  `vm_pool_size` default was clamped to `max(cpus, 4)` with a ceiling
  of 32, limiting concurrent hook execution on powerful servers.
  Removed both the floor and ceiling — now auto-sizes to exactly the
  number of CPU cores (fallback: 4 if detection fails). Override with
  `hooks.vm_pool_size` in `crap.toml`.

- **SQLite statements re-parsed on every query** — All `query_all`
  and `query_one` calls used `prepare()` which parses SQL from scratch
  on every invocation. Switched to `prepare_cached()` which reuses
  previously parsed statements. Reduces CPU overhead on every database
  operation, especially for hot paths like `find` and `find_by_id`.

- **Trash pagination navigated away from trash view** — The "Next" and
  "Previous" buttons in the trash list did not preserve `?trash=1` in
  the pagination URLs. Clicking next navigated to the regular (non-
  trash) document list. Now preserves the trash parameter across all
  pagination links.

- **Load test cleanup failed on soft-delete collections** — The gRPC
  load test script's cleanup used `DeleteMany` without
  `forceHardDelete`, so soft-delete collections reported 0 deleted.
  Now uses `forceHardDelete: true` and sums both `deleted` and
  `softDeleted` counts.

- **Example cache not enabled** — The example project now
  enables the relationship populate cache with a 60-second TTL,
  gRPC compression, and optimized depth/polling settings for better
  out-of-the-box demo performance.

- **Unpublish returned stale `_status`** — The unpublish operation
  (both collection and global) returned the document with
  `_status: "published"` instead of `"draft"` in the response. The
  database was updated correctly, but the in-memory document handed to
  after-hooks and returned to the caller still carried the pre-unpublish
  status. Now correctly sets `_status = "draft"` before after-hooks run.

- **Redundant junction table delete** — `set_polymorphic_related()`
  executed the same DELETE twice in the non-locale branch: once via the
  `delete_junction_rows()` helper and once via a manual inline query.
  Removed the redundant manual delete.

- **Flaky subscribe test race condition** — `subscribe_receives_update_event`
  and `subscribe_receives_delete_event` could receive a stale "create" event
  instead of the expected operation because event publishing runs in a
  background `spawn_blocking` task. Added a small delay between document
  creation and subscribing so the background create-event publish completes
  before the subscriber is registered.

- **Keyboard focus indicators suppressed** — `outline: 0` /
  `outline: none` on focused form inputs and the search bar removed
  the keyboard focus indicator, making the admin UI inaccessible for
  keyboard-only users. Added `focus-visible` outline styles so keyboard
  users see a visible focus ring while mouse users still get the clean
  appearance.

## [0.1.0-alpha.3] — 2026-03-30

### Added

- **Soft deletes** — Collections can opt into soft deletes with
  `soft_delete = true`. Deleted documents are moved to trash (`_deleted_at`
  timestamp) instead of being permanently removed. Soft-deleted documents
  are excluded from all reads, counts, and search. The admin UI shows a
  **Trash** tab with restore and permanent-delete buttons, plus an
  **Empty trash** action. Upload files are preserved until hard purge.
  Configurable retention (`soft_delete_retention = "30d"`) auto-purges
  expired documents. Granular permissions: `access.trash` controls
  soft-delete and undelete (falls back to `access.update`), while
  `access.delete` controls permanent deletion. Available in admin UI,
  gRPC (`Delete` with `force_hard_delete`, new `Undelete` RPC), and Lua
  (`crap.collections.delete/undelete` with `forceHardDelete` option).

- **Delete confirmation dialog** — Replaces the old two-step confirmation
  page with a single modal dialog. For soft-delete collections, shows
  "Move to trash" and "Delete permanently" options. For hard-delete
  collections, shows "Delete permanently" only. "Delete permanently"
  and "Empty trash" buttons are hidden when `access.delete` is not
  configured. Upload collections block deletion when other documents
  reference them.

- **Optional timezone support for date fields** — `timezone = true` on a
  date field stores the user's IANA timezone in a companion `_tz` column
  alongside the UTC value. The admin UI shows a timezone dropdown; the
  user enters local time and sees local time on reload (no drift). API
  responses include both `start_date` (UTC) and `start_date_tz` (IANA
  string). Requires `picker_appearance = "dayAndTime"`. Supports localized
  fields, Groups, Rows, Arrays, versioning, and a global
  `[admin] default_timezone` config fallback.

- **Serve lifecycle management** — `crap-cms serve --stop` gracefully stops a
  detached instance (SIGTERM with 10s timeout, then SIGKILL). `--restart` stops
  and re-launches. `--status` shows whether a detached instance is running, with
  PID and uptime on Linux. Stale PID files are automatically cleaned up.

- **File-based logging** — optional `[logging]` config section writes logs to
  rotating files in `data/logs/`. Supports daily, hourly, or no rotation with
  configurable retention (`max_files`). Old log files are pruned on startup.
  Auto-enabled when running with `--detach` (where stdout is unavailable).
  New CLI command: `crap-cms logs` to tail log output (`-f` to follow,
  `-n` for line count), `crap-cms logs clear` to remove old rotated files.

- **Content-Security-Policy header** — configurable `[admin.csp]` section with
  per-directive source lists (`script_src`, `style_src`, `font_src`, etc.).
  Enabled by default with permissive defaults that cover the built-in admin UI.
  Theme developers can extend the lists to allow external CDNs, fonts, and
  analytics scripts. Set `enabled = false` to disable entirely.

- **SSE connection limiting** — `max_sse_connections` in `[live]` (default:
  1000). Returns `503 Service Unavailable` when the limit is reached. `0` =
  unlimited.

- **gRPC Subscribe connection limiting** — `max_subscribe_connections` in
  `[live]` (default: 1000). Returns `UNAVAILABLE` when the limit is reached.
  `0` = unlimited.

- **Admin HTTP request timeout** — `request_timeout` in `[server]` (optional,
  none by default). Returns `408 Request Timeout` when exceeded. SSE streams
  are exempt. Accepts seconds or human-readable (`"30s"`, `"5m"`).

- **gRPC request timeout** — `grpc_timeout` in `[server]` (optional, none by
  default). Returns `DEADLINE_EXCEEDED` when exceeded. Accepts seconds or
  human-readable.

- **Configurable gRPC message size** — `grpc_max_message_size` in `[server]`
  (default: `"16MB"`). Replaces Tonic's 4MB default, which can be exceeded by
  large `Find` responses with deep relationship population. Accepts bytes or
  human-readable (`"16MB"`, `"32MB"`).

- **IP rate limiting** on auth endpoints (login, forgot-password). Configurable
  per-IP limits with automatic cooldown (`max_ip_login_attempts` in `[auth]`).

- **Reset password rate limiting** — per-IP rate limiting on the reset-password
  endpoint (admin and gRPC) to prevent brute-forcing reset tokens.

- **`trust_proxy` config** (`[server]`) — controls whether `X-Forwarded-For` is
  trusted for client IP extraction. Default: `false` (XFF ignored). Enable when
  running behind a reverse proxy so per-IP rate limiting uses the real client IP.

- **H2C support** (HTTP/2 cleartext) for deployment behind reverse proxies.
  New `[server] h2c` config option.

- **Populate cache cap** (`MAX_POPULATE_CACHE_SIZE = 10,000`) prevents unbounded
  memory growth during read-heavy workloads.

- **Hooks on bulk operations** — `before_change`/`after_change` hooks now fire
  per-document for `UpdateMany`, and `before_delete`/`after_delete` for
  `DeleteMany`. Version snapshots are also created per-document. Opt out with
  `hooks = false` in the request.

- **`DeleteMany` soft-delete support** — `DeleteManyRequest` gains a
  `force_hard_delete` field (matching single `Delete`). When the collection
  has `soft_delete` enabled, `DeleteMany` now moves documents to trash by
  default. `DeleteManyResponse` reports both `deleted` (permanently removed)
  and `soft_deleted` (trashed) counts. Permission checks use `access.trash`
  for soft deletes and `access.delete` for hard deletes.

- **Bulk operation safety limit** — `UpdateMany` and `DeleteMany` are now
  capped at 10,000 documents per request to prevent unbounded memory usage.
  Use paginated calls for larger datasets.

- **Startup config validation** — validates port > 0, admin_port != grpc_port,
  `channel_capacity > 0`, `pagination.default_limit > 0`,
  `pagination.default_limit <= max_limit`, `depth >= 0`,
  `default_locale` in `locales` list, MCP HTTP requires `api_key`, and
  warns on questionable settings (e.g., SMTP configured but `public_url`
  missing).

- **Security headers** on all admin responses: `X-Frame-Options: DENY`,
  `X-Content-Type-Options: nosniff`, `Referrer-Policy`,
  `Permissions-Policy` (camera, microphone, geolocation disabled).

- **`crap.json` namespace** — `crap.json.encode()` / `crap.json.decode()` as
  cleaner aliases for `crap.util.json_encode()` / `crap.util.json_decode()`.
  The old `crap.util.json_*` functions continue to work.

- **Lua type definitions** — `types/crap.lua` provides LuaLS-compatible
  `@class`/`@param`/`@return` annotations for the entire `crap.*` API,
  enabling IDE autocompletion and type checking.

- **Reference counting for delete protection** — Every collection table
  now has a `_ref_count` column that tracks how many documents reference
  it. Delete protection is O(1) instead of scanning all collections.
  Covers all relationship types: has-one, has-many, polymorphic, localized,
  array sub-fields, and block sub-fields. Globals that hold outgoing
  references also maintain ref counts on their targets. A one-time
  backfill migration computes initial counts from existing data.

- **Design system harmonization** — Unified button, input, and icon sizing
  across the entire admin UI. All interactive controls now share a consistent
  height scale derived from a single `--base` unit (4px grid). Buttons and
  inputs align at 36px (`--control-lg`), small buttons at 28px (`--control-sm`).
  Icon sizes use a dedicated `--icon-xs/sm/md/lg/xl` scale. All spacing,
  sizing, and layout values use `rem` units via `calc(var(--base) * n)` for
  scalability. The `button--secondary` variant (tinted) fills a previously
  missing gap between primary and ghost buttons.

- **Inline create for relationship fields** — Clicking "Create new" on a
  relationship or upload field now opens a near-fullpage slideout panel
  instead of navigating away. The create form loads in the panel with full
  field support (richtext, code, arrays, blocks). On success, the created
  item is automatically selected in the relationship field. Form context
  is preserved — no more losing unsaved work. Works for both has-one and
  has-many relationships, including polymorphic and upload fields.
  Ctrl+click still opens in a new tab for progressive enhancement.

- **Tag-style chips for has-many relationships** — Has-many relationship
  fields now display selected items as chips inside the search input
  (like a tag input), instead of in a separate row above. Backspace
  removes the last chip. Enter selects the first search result without
  requiring arrow-key navigation first.

- **Shadow DOM web components** — `<crap-block-picker>`, `<crap-tags>`,
  and `<crap-focal-point>` migrated to Shadow DOM with encapsulated
  styles. `<crap-relationship-search>` and `<crap-live-events>` use
  injected scoped styles. ~500 lines of CSS removed from global
  stylesheets and co-located with their components. Dead CSS for
  filter-builder and column-picker (duplicated in the drawer's Shadow
  DOM) removed from global sheets.

- **FOUC prevention** — `:not(:defined)` CSS rules hide Shadow DOM
  components until their JavaScript registers, preventing flash of
  unstyled content.

- **Event-driven component communication** — Removed all global
  singleton patterns (`window.CrapToast`, `getDrawer()`,
  `getConfirmDialog()`, `getCreatePanel()`). Components now
  communicate exclusively via native `CustomEvent` dispatch and
  document-level listeners. Zero cross-component imports, zero
  wrapper functions, zero null checks. Events used:
  `crap:toast` (notifications), `crap:drawer-request` (drawer
  discovery), `crap:confirm-dialog-request` (confirm dialog
  discovery), `crap:create-panel-request` (create panel discovery).

### Fixed

- **Unique check swallowed database errors** — When the uniqueness query
  failed (e.g. database connectivity issue), the error was logged at `warn`
  level but validation silently passed. Duplicate values could be persisted
  if the database was temporarily unavailable during validation. Now produces
  a `validation.unique_check_failed` error.

- **Custom validator errors silently passed** — When a Lua `validate`
  function threw a runtime error, the exception was logged at `warn` level
  but the field silently passed validation. Invalid data could be persisted.
  Now produces a `validation.custom_error` error.

- **`delete_many` silently skipped referenced documents** — When a bulk
  hard delete encountered documents with outstanding references, they were
  silently skipped with only a debug log. The caller received only the
  `deleted` count with no indication that some documents were not removed.
  Both Lua and gRPC `delete_many` now report a `skipped` count alongside
  `deleted`.

- **Has-many validation only reported first invalid value** — Per-element
  validation of has_many text/number fields used `break` after the first
  error, hiding subsequent violations. Users had to fix one value, resubmit,
  and discover the next. Now all invalid values are reported at once.

- **Unique check silently skipped on invalid locale** — When locale
  sanitization failed for a localized unique field, the unique constraint
  check was silently skipped with only a debug-level log. Duplicate values
  could slip through. Now emits a validation error instead.

- **Display conditions failed open on error** — When a display condition
  Lua function threw an error or returned an unexpected type, the field was
  shown as a "safe default". This could expose access-controlled content.
  Now fails closed (hides the field) on error.

- **Upload Bearer token silently fell back to anonymous** — The HTTP upload
  endpoint treated `Authorization: Basic ...` (non-Bearer scheme) as
  anonymous access instead of returning 401. Misconfigured clients were
  silently unauthenticated.

- **Bulk operations silently capped at 10K documents** — `UpdateMany` and
  `DeleteMany` applied a `LIMIT 10000` to the query but did not inform
  the client when results were truncated. Partial mutations occurred with
  no feedback. Now returns `RESOURCE_EXHAUSTED` when the limit is hit.

- **`get_ref_count` returned 0 for missing documents** — The function
  could not distinguish "document has zero references" from "document
  does not exist", which could mask lookup failures in delete protection.
  Now returns `Option<i64>` (`None` for missing documents).

- **Backfill migration silently skipped errors** — `backfill_ref_counts`
  caught query errors at `debug` level and returned `Ok(())`, hiding
  corrupted junction tables. Ref counts could remain incorrect while the
  migration appeared to succeed. Errors are now logged at `warn` level.

- **Display condition evaluated empty field name** — A condition object
  with a missing or empty `"field"` key silently matched against an
  empty-string lookup instead of warning. Now logs a warning and defaults
  to showing the field.

- **Length validation counted bytes instead of characters** — `min_length`
  and `max_length` field validation used `s.len()` (byte count) instead of
  `s.chars().count()` (character count). Multibyte UTF-8 strings were
  overcounted: "café" (4 chars, 5 bytes) would fail `max_length = 4`, and
  CJK text like "你好世界" (4 chars, 12 bytes) would fail `min_length = 5`.

- **Email validation accepted invalid dot patterns** — `is_valid_email_format`
  accepted leading dots (`.user@example.com`), trailing dots
  (`user.@example.com`), consecutive dots (`user..name@example.com`), and
  the same patterns in domain parts. Now rejects all per RFC 5321.

- **Empty Bearer token treated as valid** — `extract_bearer_token("Bearer ")`
  returned `Some("")` instead of `None`, which would pass to JWT validation
  and produce a confusing error. Now filters empty tokens.

- **FTS sync dropped existing index on validation failure** — `sync_fts_table`
  dropped the existing FTS table before validating field names. If validation
  failed (e.g., invalid identifier), the existing working index was destroyed
  with no replacement. Validation now runs before the drop.

- **Lua `delete` silently succeeded on missing documents** — The Lua CRUD
  `crap.collections.delete()` did not check the return value of
  `query::delete`, so deleting a non-existent document appeared to succeed.
  Now returns a "not found" error. `delete_many` now skips already-deleted
  documents gracefully instead of failing.

- **Delete error response leaked internal details** — The admin delete
  handler returned `e.to_string()` in JSON error responses, potentially
  exposing database paths, schema details, or internal error messages.
  Now returns a generic "Failed to delete item" message and logs the full
  error server-side.

- **Stale job error message showed wrong timeout value** — The
  `recover_stale_jobs` error message logged the stale detection threshold
  (2x timeout, min 300s) as `timeout=<threshold>s` instead of the actual
  configured job timeout, misleading operators.

- **Empty trash could delete referenced documents** — The "Empty trash"
  action permanently deleted all soft-deleted documents without checking
  `_ref_count`, which could break referential integrity. Now skips
  documents that are still referenced by other documents, matching the
  behavior of single delete and the gRPC `DeleteMany` endpoint.

- **Lua `delete_many` blocked soft-delete of referenced documents** —
  `crap.collections.delete_many` checked `_ref_count` for both soft and
  hard deletes, blocking soft-deletion of referenced documents. This was
  inconsistent with single `delete()` and the gRPC API, which only check
  ref counts for hard deletes. Soft-deleted documents remain referenceable
  by design.

- **Lua `delete_many` missing `forceHardDelete` option** —
  `crap.collections.delete_many` now supports `{ forceHardDelete = true }`
  to permanently delete documents even when the collection has
  `soft_delete` enabled, matching the existing single `delete()` API.

- **Table rebuild could leave database inconsistent on failure** — The
  SQLite table rebuild (used during `soft_delete` migration) could leave
  the database with an empty new table and orphaned temp table if the
  data copy step failed. Now recovers by restoring the original table.

- **Draft versioned updates skipped field-level after_change hooks** —
  When saving a draft version via Lua `crap.collections.update`, field-
  level `after_change` hooks were not called, though collection-level
  hooks were. Now both run consistently.

- **CSRF token extraction in list-settings.js** — The column settings
  save used `split('=')[1]` to extract the CSRF cookie, which would
  truncate tokens containing `=`. Now uses the same regex pattern as
  all other components.

- **API upload DELETE returned 500 for all errors** — The upload DELETE
  endpoint now returns `404 Not Found` when the document doesn't exist
  and `409 Conflict` when the document is referenced by others, instead
  of `500 Internal Server Error` for every failure.

- **Display condition errors silently showed fields** — When a Lua
  display condition function throws an error or returns an unexpected
  type, the field was shown without any diagnostic. Now logs a warning
  with the function reference and error details.

- **Access constraint unexpected types silently denied** — When an access
  function returns an unexpected Lua type (not boolean or table), the
  request was silently denied without logging. Now logs a warning with
  the function reference and actual type returned.

- **Transaction commit errors silently continued** — Three instances in
  the gRPC field-read-access path logged commit failures with
  `tracing::warn!` but continued execution. Now propagates the error
  properly via `?`.

- **Redundant timezone variable in create/update** — `tz_base` was
  identical to `tz_key` in both `create.rs` and `update.rs` timezone
  companion column handling. Removed the duplicate.

- **`<crap-create-panel>` never instantiated** — The `<crap-create-panel>`
  Web Component was imported and defined but never placed in the DOM,
  making the inline-create feature for relationship and upload fields
  completely non-functional. Added to `templates/layout/base.hbs`.

- **gRPC `get_global_impl` double pool acquisition** — Acquired a
  connection from the pool and then called `ops::get_global()` which
  acquired a second one, risking deadlock on small pools. Now uses
  `query::get_global()` directly on the existing connection.

- **gRPC `update_global_impl` held connection during service call** —
  Held a pool connection while `service::update_global_document()` tried
  to acquire its own, risking deadlock. Now drops the connection first.

- **Lua `update_many` accepted password on auth collections** — The Lua
  `crap.collections.update_many()` did not reject or strip password
  fields on auth collections. Bulk password changes are now explicitly
  rejected with a clear error message.

- **gRPC `restore_version_impl` leaked read-denied fields** — The
  restore-version endpoint returned the full document without stripping
  fields the user is not permitted to read. Now applies the same
  `strip_denied_proto_fields` as all other endpoints.

- **Global unpublish bypassed lifecycle hooks** — Unpublishing a global
  via the admin UI directly called `unpublish_with_snapshot` without
  running before/after change hooks. Now uses a new
  `unpublish_global_document()` that follows the same lifecycle as
  collection unpublish.

- **Lua `update_many` validation missing `soft_delete`, `registry`,
  `draft`** — The `ValidationCtx` in `update_many` was missing
  `soft_delete` (causing false-positive unique constraint violations on
  soft-delete collections), `registry` (skipping richtext node attribute
  validation), and `draft` (enforcing required-field checks on drafts).

- **`locale_config` not passed to `persist_create`/`persist_update`** —
  Reference count operations during create and update used a default
  (empty) `LocaleConfig`, potentially missing locale-specific relationship
  fields. Now forwards the locale config from the write context.

- **Verification email URL hardcoded `http://`** — The email verification
  URL always used `http://` regardless of configuration. Now respects
  `public_url` from server config, matching the forgot-password flow.

- **gRPC `get_global_impl` passed `user: None` to `AfterReadCtx`** —
  After-read hooks saw no authenticated user, breaking user-dependent
  transformations. Now passes the resolved auth user.

- **`send_signal` cast u32 PID to i32 via `as`** — PIDs above
  `i32::MAX` silently wrapped to negative values, which `kill(2)`
  interprets as process groups. Now uses `i32::try_from()` and returns
  an error for out-of-range PIDs.

- **MCP filter operators inconsistent with gRPC API** — MCP used
  `greater_than_equal`/`less_than_equal` while gRPC used
  `greater_than_or_equal`/`less_than_or_equal`. Both forms are now
  accepted. Unrecognized operators now log a warning instead of being
  silently dropped.

- **`me_impl` did not hydrate join table data** — The `/me` endpoint
  returned documents without hydrating array fields, has-many
  relationships, or blocks data. Now calls `hydrate_document`.

- **`list_job_runs_impl` had no upper bound on `limit`** — A client
  could pass an arbitrarily large limit. Now capped at 1000.

- **`empty_trash_action` called `fts_delete` unconditionally** — Did not
  check `supports_fts()` first, which would fail on non-FTS backends.

- **`delete_upload_files` skipped all `*image*` field names** — The
  filter `key.contains("image")` incorrectly skipped fields like
  `hero_image_url`. Changed to exact match on `image_url` only.

- **`ValidationError::to_field_map()` dropped duplicate field errors** —
  Multiple validation errors for the same field were lost due to
  `HashMap::collect()`. Now joins them with `"; "`.

- **Richtext custom node attribute roundtrip** — HTML-escaped attribute
  values (`&#39;`, `&amp;`, etc.) in `<crap-node data-attrs>` were not
  unescaped before JSON parsing, causing deserialization failures.

- **MIME verification bidirectional match** — The upload MIME check
  tested both directions (`detected ∈ claimed` OR `claimed ∈ detected`),
  weakening the security check. Now only verifies `detected ∈ claimed`.

- **`_tz` companion columns not locale-expanded** — When a localized
  Date field had `timezone = true`, `get_expected_column_names` generated
  bare `field_tz` instead of per-locale `field_tz__en`, `field_tz__de`.
  This caused migration drift detection to incorrectly flag columns.

- **Unquoted table names in trash/scheduler SQL** — `find_purge_candidates`
  and `purge_soft_deleted` used unquoted table names, which would fail
  for collection slugs that are SQL reserved words.

- **UTF-8 panic in config duration/filesize parsing** — Multi-byte
  characters (e.g., emoji) in `parse_duration_string` or
  `parse_filesize_string` could cause a panic from invalid byte-offset
  slicing. Now uses char-aware splitting and ASCII validation.

- **Inconsistent duration parsing in scheduler** —
  `parse_retention_seconds` only supported `d`/`h` suffixes. Now also
  supports `m` (minutes) and `s` (seconds) for consistency with
  `parse_duration_string`.

- **`before_broadcast` hooks lost `context` table** — The
  `call_before_broadcast_hook` and `call_registered_before_broadcast`
  functions did not call `read_context_back()`, silently discarding any
  shared state set by hooks on `ctx.context`.

- **`password.hbs` double-nested `form__field` wrapper** — The password
  field template included its own `<div class="form__field">` while the
  parent (`edit_form.hbs`) already provides one, causing CSS layout issues.

- **`_collectFormData` overwrote multi-value form fields** — Both
  `conditions.js` and `validate-form.js` used `data[key] = val` which
  dropped all but the last value for multi-value fields (has-many). Now
  collects duplicate keys into arrays.

- **Lua typegen sub-type name collisions** — Array/Group sub-type class
  names in Lua type generation used only the field name (e.g.,
  `crap.array_row.Items`), colliding when multiple collections had
  identically named fields. Now prefixed with the collection name.

- **EventBus used `Ordering::Relaxed` for sequence counter** — Could
  cause out-of-order sequence numbers across threads. Changed to
  `Ordering::AcqRel`.

- **`back_references` endpoint had no access control** — The endpoint
  returned back-references for any document without checking collection
  read access. Now verifies read permissions.

- **Session guard dialog accumulated event listeners** — The `show()`
  method added click/cancel listeners without removing previous ones.
  Now cleans up the `cancel` handler alongside click handlers.

- **Version list pagination generated `page=0` URLs** — Previous-page
  URLs for version lists used `page - 1` which produced `?page=0` on
  the first page. Now clamps to a minimum of 1.

- **Back-reference self-ref filter compared slug to ID** — The
  self-reference filter compared `owner_slug` (collection name) with
  `target_id` (document ID), making it effectively a no-op. Now
  correctly compares `owner_slug` with `target_collection`.

- **`jobs show` always printed Data field** — Used `if let Some(ref data)
  = Some(...)` which is always true. Changed to `if !run.data.is_empty()`.

- **Claims builder `iat` cast could wrap on pre-epoch clock** — Cast
  `i64` timestamp to `u64` via `as` which wraps negative values. Now
  clamps to 0 first.

- **Relationship search drawer race condition** — The drawer picker
  for relationship fields had no `AbortController`, so rapid searches
  or pagination could resolve out of order. Added abort controller to
  cancel stale fetches.

- **validate-form.js memory leak on reconnect** — Missing `_connected`
  guard meant event listeners could be duplicated if the component was
  disconnected and reconnected by HTMX swaps.

- **sessionStorage errors in private browsing** — `scroll.js` form
  state save/restore now wraps `sessionStorage` calls in try-catch to
  handle private browsing and quota exceeded scenarios gracefully.

- **Back-references button stuck on error text** — After a fetch error
  the "Show details" button displayed "error" permanently. Now restores
  the original button label on retry.

- **Invalid SQL in reference counting** — `MAX(0, expr)` is not
  portable across database backends. Replaced with
  `conn.greatest_expr()` on the `DbConnection` trait (SQLite uses
  `MAX(a, b)`, PostgreSQL would use `GREATEST(a, b)`).

- **Panic in date normalization** — `unwrap()` on
  `date.and_hms_opt()` replaced with proper error propagation via
  `ok_or_else()`.

- **Silent transaction commit errors** — 22 instances of
  `let _ = tx.commit()` across the codebase now log failures via
  `tracing::warn!` instead of silently swallowing errors.

- **Button/input disabled states** — `.button:disabled` now shows
  50% opacity with `not-allowed` cursor. Disabled inputs, selects,
  and textareas show dimmed text, grayed background, and block
  pointer events.

- **Sort on fields inside layout wrappers** — Sorting by a field
  inside a Row, Collapsible, or Tabs wrapper (e.g. `default_sort =
  "-start_date"` where `start_date` is in a Row) caused a 500 error
  ("Invalid sort column"). The sort column validator now recurses into
  layout wrappers to find promoted fields.

- **Upload fields in new block rows not saving** — When adding a new
  block row and selecting an upload/relationship, the value was lost
  on save. The `__INDEX__` placeholder in the `field-name` attribute
  of `<crap-relationship-search>` was not replaced with the actual
  row index, so the hidden input submitted an unparseable field name.
  Fixed by including `[field-name]` in the index replacement
  selectors.

- **Reference counting missing in bulk operations** — `UpdateMany`
  never adjusted ref counts when relationship fields changed, and
  `DeleteMany` never decremented target ref counts before deleting.
  Both could silently corrupt `_ref_count` values, breaking delete
  protection and creating dangling references. Now both operations
  snapshot and adjust ref counts per-document. `DeleteMany` also
  skips documents with `_ref_count > 0` (matching single-delete
  behavior).

- **Version restore broke reference counts** — Restoring a version
  snapshot never adjusted ref counts. If a relationship changed
  between versions, restoring the old version would leave the new
  target's count too high and the old target's count too low. Now
  snapshots outgoing refs before restore and applies the diff after.

- **Empty trash used wrong locale config** — The empty trash handler
  used `LocaleConfig::default()` instead of the site's actual locale
  configuration. Ref count adjustments for multi-locale sites with
  localized relationship fields could read the wrong locale columns.

- **FTS search skipped fields inside layout wrappers** — Fields
  inside Row, Collapsible, or Tabs (which promote children to
  parent-level columns) were not found by the FTS field validator.
  `list_searchable_fields` referencing such fields were silently
  filtered out. Now recurses into layout wrappers for both explicit
  and default FTS field resolution.

- **Upload path traversal when directory missing** — The
  canonicalize-based path check in the upload file serve handler was
  inside an `if let` that silently skipped the check when either path
  couldn't be canonicalized (e.g., directory doesn't exist). Changed
  to `match` — canonicalize failures now return 404.

- **Startup validation for field references** — Collection
  registration now warns when `use_as_title`, `default_sort`, or
  `list_searchable_fields` reference field names that don't exist in
  the collection's field definitions (including fields inside layout
  wrappers). Previously these misconfigurations failed silently at
  runtime.

- **JWT validation errors now logged** — Failed JWT token validation
  (expired, invalid signature, malformed) is now logged at debug
  level instead of being silently swallowed via `.ok()`. Aids
  debugging session issues in production.

- **Array date fields missing timezone columns** — Date sub-fields
  with `timezone = true` inside Array fields did not get the `_tz`
  companion column in the join table (both CREATE and ALTER TABLE
  paths). Main collection tables handled this correctly; array tables
  were missing the logic. Timezone data for array date fields was
  silently lost.

- **Inherited localization missing in join tables** — Arrays, Blocks,
  and has-many Relationships inside a localized Group did not inherit
  the `_locale` column in their join tables. Only directly-localized
  fields got the column. The `sync_join_tables_inner` function now
  propagates `inherited_localized` from parent Groups, matching the
  existing behavior in `collect_column_specs_inner`.

- **Inconsistent SQL identifier quoting** — Table names in SQL format
  strings were inconsistently quoted across the query layer. Some files
  (e.g., `ref_count.rs`) used double-quoted identifiers while most
  others did not. All table name interpolations now use double-quoted
  identifiers (`"table"`) for defense-in-depth consistency.

- **Global tables missing timezone companion columns** — Date fields
  with `timezone = true` in globals did not get the `_tz` companion
  column (both CREATE and ALTER TABLE paths). The column was created
  with the field's own type instead of TEXT, or omitted entirely.
  Collection tables handled this correctly; global migration code was
  missing the `companion_text` check. Timezone data for global date
  fields was silently lost or stored with the wrong type.

- **Global tables missing default values** — Fields with
  `default_value` in globals never had their SQL DEFAULT clause
  applied (both CREATE and ALTER TABLE paths). Collection tables
  handled this correctly; global migration code never called
  `append_default_value`. Checkbox fields also missed their implicit
  `DEFAULT 0`. New rows inserted into global tables got NULL instead
  of the configured default.

- **gRPC RestoreVersion not wrapped in transaction** — The gRPC
  `RestoreVersion` handler performed multiple SQL operations (update
  document, adjust ref counts, set status, create version) on a bare
  connection without a transaction. A failure partway through could
  leave the document in an inconsistent state. The admin UI handler
  was already correctly wrapped. Now both paths use a transaction.

- **Lua CRUD validation missing registry and soft_delete** — The Lua
  API's `crap.collections.create()` and `crap.collections.update()`
  called field validation without the registry (needed for richtext
  custom node attribute validation) and without the `soft_delete` flag
  (needed for unique constraint checks to exclude soft-deleted
  documents). This meant unique fields on soft-delete collections
  could reject values that only exist in soft-deleted rows, and
  richtext custom node validation was silently skipped. Also fixed
  the missing `soft_delete` flag in the bulk API `UpdateMany` and
  admin validation handlers.

- **Path traversal in upload file deletion** (CRITICAL) — The
  canonicalize-based path safety check in `delete_upload_files()` was
  inside an `if let` guard that only triggered when both canonicalize
  calls succeeded AND the path was outside the uploads directory. When
  canonicalize failed (e.g., broken symlink, missing directory), the
  guard didn't fire and the file was deleted without validation. Changed
  to an explicit `match` that skips deletion when canonicalize fails.

- **Division by zero in image resize** (CRITICAL) — `resize_image()`
  divided by `img.height()` and `img.width()` without checking for zero,
  causing a panic on malformed images with zero dimensions. Now returns
  `None` for zero-dimension images, and callers skip the size with a
  warning.

- **Field hook modifications lost in after-change hooks** (CRITICAL) —
  Both `crap.collections.create()` and `crap.collections.update()` in
  the Lua API ran field-level `after_change` hooks that modified
  `after_data`, but then passed `doc.fields.clone()` (the unmodified
  data) to the collection-level `after_change` hook. Field hook
  modifications were silently discarded. Now passes `after_data` to
  the collection-level hook.

- **Unpublish after-change hook received stale data** (HIGH) — The
  `after_change` hook for unpublish operations received the pre-unpublish
  document data with `draft: false`. Now re-reads the document after
  the unpublish and passes the updated state with `draft: true`.

- **DeleteMany deleted upload files for ref-protected documents**
  (HIGH) — `DeleteMany` iterated all queried documents for file cleanup,
  including those skipped due to `_ref_count > 0`. Database records
  survived but their upload files were deleted. Now only deletes files
  for documents that were actually removed from the database.

- **DeleteMany fired BeforeDelete hook for skipped documents** (HIGH) —
  `DeleteMany` ran the `BeforeDelete` hook before checking reference
  counts. Documents with incoming references were skipped (not deleted),
  but the hook had already fired, causing semantic inconsistency. Moved
  the reference count check before the hook.

- **Soft-delete purge deleted files before database records** (HIGH) —
  `purge_collection()` deleted upload files before the corresponding
  database delete. A crash between the two operations left orphaned
  database records pointing to missing files. Reversed the order: DB
  delete first, then file cleanup. A crash now leaves orphaned files
  (harmless) instead of orphaned records (harmful).

- **Zero scheduler intervals caused busy loops** (HIGH) — `JobsConfig`
  allowed `poll_interval`, `cron_interval`, and `heartbeat_interval` to
  be set to 0, causing tokio interval timers to fire continuously and
  starve the event loop. Added startup validation that all three must
  be > 0.

- **DeleteMany ignored `soft_delete` configuration** (HIGH) — The gRPC
  `DeleteMany` always performed hard deletes, bypassing the collection's
  `soft_delete` setting entirely. Documents that should have been moved to
  trash were permanently destroyed. Now respects `soft_delete`: matching
  documents are soft-deleted unless `force_hard_delete` is set. Permission
  checks also now use `access.trash` for soft deletes (matching single
  `Delete` behavior) instead of always requiring `access.delete`.
  `DeleteManyResponse` now reports both `deleted` and `soft_deleted` counts.

- **Field access control skipped Tabs sub-fields** (HIGH) — Field-level
  access control (`access.read`, `access.create`, `access.update`) did
  not recurse into Tabs layout containers. Fields with access restrictions
  inside Tabs were silently exposed to all users. Now correctly recurses
  into `field.tabs[i].fields`. The `deny_all_access_controlled` fail-closed
  fallback (used when the Lua VM pool is exhausted) had the same issue and
  is also fixed to recurse into Group, Row, Collapsible, and Tabs.

- **Richtext/Code editors lost state on array row reorder** (HIGH) —
  `CrapRichtext` and `CrapCode` web components destroyed and re-initialized
  their editor views on every DOM disconnect/reconnect cycle (triggered by
  drag-and-drop reordering). Undo history, cursor position, and unsaved
  content could be lost. Added idempotency guards to `connectedCallback`
  and removed destructive cleanup from `disconnectedCallback`. Also fixed
  `CrapConditions` and `CrapBackRefs` registering duplicate event listeners
  on reconnection.

- **Unquoted SQL table names in migrations** — `CREATE TABLE`, `ALTER TABLE`,
  `DROP TABLE`, `INSERT INTO`, and `RENAME TO` statements in migration code
  did not double-quote table names. Collections with slugs matching SQL
  reserved words (e.g., `order`, `group`, `index`) would fail to create or
  alter. All migration SQL now uses `"table_name"` quoting.

- **Sort by group sub-fields rejected** — `is_valid_sort_column` did not
  recognize group sub-fields (`seo__title`) or fields inside Tabs. Sorting
  by these columns returned "Invalid sort column". Now handles `group__sub`
  naming and recurses into Tabs.

- **Cursor pagination broke with NULL sort values** — Keyset pagination
  used `col > ?` / `col < ?` comparisons which evaluate to NULL in SQL
  when the cursor's sort value is NULL. All remaining rows were silently
  skipped. Now uses `IS NULL` / `IS NOT NULL` conditions for NULL cursors.

- **`field_exists_recursive` skipped Tabs** — Registry startup warnings
  for `use_as_title`, `default_sort`, and `list_searchable_fields` did not
  recurse into Tabs containers, producing false-positive "field not found"
  warnings for valid configurations. Now recurses into `field.tabs`.

- **Empty trash ignored `default_deny` setting** — The empty trash handler
  hard-coded a 403 when no `access.delete` function was configured,
  regardless of the `default_deny` setting. Now uses the same
  `check_access_or_forbid` pattern as other access checks.

- **Validate endpoints leaked internal error details** — Non-validation
  errors from the create/update validate endpoints included full
  `anyhow::Error` strings (potentially containing DB paths, schema
  details) in the HTTP response. Now returns a generic message and logs
  the full error server-side.

- **Evaluate conditions accepted arbitrary Lua function refs** — The
  server-side display condition evaluation endpoint accepted any Lua
  function reference string without validation. Now validates that
  submitted function refs match `admin.condition` values defined in the
  collection's field definitions.

- **Bulk operations had no query limit** — `UpdateMany` and `DeleteMany`
  loaded all matching documents into memory with no safety cap. A broad
  filter on a large collection could cause OOM. Now capped at 10,000
  documents per bulk operation.

- **Draft mode skipped all validation on Array/Blocks sub-fields** —
  Saving as draft skipped not just `required` checks but all validation
  (email format, numeric bounds, option values, custom validators) for
  Array and Blocks sub-fields. Now only skips `required` in draft mode;
  all other constraints are enforced.

- **MCP auth collection schema missing `password` in required** — When
  an auth collection had no other required fields, the `password` field
  was silently omitted from the `required` array in the MCP tool schema.
  LLM clients could create users without passwords.

- **MCP stdio panic lost request ID** — If `handle_message` panicked
  inside `spawn_blocking`, the error response was sent with `id: None`.
  MCP clients could not correlate the error with their request. Now
  preserves the request ID before moving it into the blocking task.

- **CrapTags ignored readonly attribute** — The tag input component
  did not check `data-readonly`, allowing users to add and remove tags
  on locale-locked or readonly fields. Now hides the input and remove
  buttons when readonly.

- **XSS in focal-point component** — `CrapFocalPoint` interpolated the
  image `src` directly into an `innerHTML` template literal, allowing
  attribute injection via crafted `data-src` values. Now sets `src` via
  the DOM property.

- **Delete dialog double-click race condition** — Rapid double-clicking
  the delete button could send duplicate DELETE requests before the first
  response arrived. Added a `submitting` guard.

- **Dirty form guard used wrong HTMX event property** — `CrapDirtyForm`
  and `CrapLiveEvents` accessed `e.detail.verb` on `htmx:beforeRequest`
  events, but HTMX 1.9 provides `e.detail.requestConfig.verb`. The dirty
  flag could be incorrectly cleared on GET navigations. Now checks both
  properties for compatibility.

- **Job retry backoff skipped the 5-second tier** — `backoff_seconds`
  used `2^attempt` but `attempt` is 1-based after claim, so the first
  retry waited 10s instead of 5s. Fixed formula: `2^(attempt-1) * 5`.

- **MCP global read used fragile string matching** — `exec_read_global`
  detected "not found" errors by checking if the error message contained
  "not found" or "no rows". Unrelated errors containing those substrings
  would be silently swallowed. Now inspects the error chain for specific
  causes.

- **Cron expression normalization preserved extra whitespace** —
  `normalize_cron` prepended "0 " to the raw input string, so
  `"0  3  *  *  *"` became `"0 0  3  *  *  *"`. Now normalizes to
  single-spaced output.

- **i18n translations not refreshed on HTMX body swap** — The `t()`
  translation function cached the `#crap-i18n` data island on first
  access and never invalidated. After a locale change via HTMX navigation,
  stale translations persisted until a full page reload. Now invalidates
  the cache on `htmx:afterSettle` body swaps.

- **CSRF cookie decoding inconsistency** — `validate-form.js` and
  `conditions.js` read the CSRF cookie without `decodeURIComponent`,
  while `delete-dialog.js` decoded it. Now all components decode
  consistently.

- **Create panel error used innerHTML** — The error fallback in
  `CrapCreatePanel` used `innerHTML` with the `t('error')` translation
  string, which could render HTML if the translation contained markup.
  Now uses `textContent`.

- **Delete dialog error response double-consumed** — After a failed
  `resp.json()` parse, the catch block called `resp.text()` on the
  already-consumed body. Now reads the body once with `resp.text()` and
  parses via `JSON.parse`.

- **Image queue claim race condition** — `claim_pending_images()` used
  a non-atomic SELECT-then-UPDATE pattern. Concurrent callers could
  SELECT the same pending rows before either marked them as processing,
  leading to duplicate image processing. Now uses optimistic locking:
  each UPDATE includes `AND status = 'pending'` so only one caller
  succeeds per row.

- **Unknown block types silently bypassed validation** — Blocks fields
  with an unrecognized `_block_type` (not matching any defined block
  definition) were silently skipped during validation. Arbitrary data
  could be stored without any field validation. Now produces a
  `validation.unknown_block_type` error.

- **Non-object array/blocks rows silently bypassed validation** —
  Primitive values (strings, numbers, null) in array or blocks fields
  were silently skipped instead of being validated. Now produces a
  `validation.invalid_row_type` error when sub-fields or block
  definitions are defined on the field.

- **`has_many` select malformed JSON silently ignored** — A `has_many`
  select field with invalid JSON (e.g., `"[invalid"`) silently passed
  option validation. Now produces a
  `validation.invalid_multi_select_json` error.

- **Locale sanitization fell back to wrong column for unique check** —
  When a locale string failed `sanitize_locale()`, the unique constraint
  check fell back to the non-localized column name (e.g., `slug` instead
  of `slug__en`), potentially allowing duplicates in the localized
  column. Now skips the unique check entirely on invalid locale.

- **Default value type not validated against field type** — A field
  definition could have a type-mismatched `default_value` (e.g., boolean
  default on a text field, string default on a number field) without any
  error. Documents created without explicit values would get
  type-incompatible defaults. Now validates at parse time: checkbox
  requires boolean, number requires number, text/date/select/etc.
  require string.

- **`ClaimsBuilder.build()` panicked on missing fields** — The JWT
  claims builder used `.expect()` for required `email` and `exp` fields,
  which would panic and crash the server if a code path failed to set
  them. Now returns `Result` with descriptive error messages. All
  callers updated to handle the error gracefully.

- **JSON-to-Lua number conversion silently lost data** — JSON numbers
  outside the i64 and f64 representable range were silently converted
  to Lua `nil`, losing the value without any error. Now returns a
  `RuntimeError` describing the unrepresentable number.

- **CSRF cookie `decodeURIComponent` could throw** — The
  `_getCsrf()` helpers in `conditions.js`, `validate-form.js`, and
  `delete-dialog.js` called `decodeURIComponent()` without a try-catch.
  A malformed cookie value could throw an uncaught exception, breaking
  form submissions and condition evaluation. Now falls back to the raw
  cookie value on decode error.

- **Validation error elements missing `role="alert"`** — Error messages
  injected by `validate-form.js` did not have `role="alert"`, so screen
  readers would not announce validation errors to assistive technology
  users. Now sets `role="alert"` on all injected error elements.

- **Server-side condition evaluation race condition** — The
  `<crap-conditions>` component's debounced server-side evaluation had
  no request cancellation. Rapid form changes could result in multiple
  in-flight requests, with stale responses overwriting newer results.
  Now uses `AbortController` to cancel previous requests before
  issuing a new one.

- **Field-level hooks skipped nested fields** (CRITICAL) — `run_field_hooks_inner`
  and `has_field_hooks_for_event` only iterated top-level fields, never
  recursing into Group, Row, Collapsible, or Tabs containers. Field hooks
  (before_validate, before_change, after_change, after_read) defined on
  sub-fields inside these containers were silently skipped. Now uses
  recursive traversal with proper `group__subfield` prefix accumulation,
  matching the pattern already used by validation.

- **Unpublish before-change hook received `draft=false`** (HIGH) — Both the
  Lua CRUD `handle_unpublish` and the service-layer `unpublish_document`
  built the `beforeChange` hook context with `draft(false)` (or omitted it
  entirely), even though the document is transitioning to draft state. Hooks
  could not distinguish unpublish from a regular update. Now both paths set
  `draft(true)`.

- **`condition_is_truthy` treated `Number(0)` as truthy** — The display
  condition `is_truthy` / `is_falsy` operators treated all numbers
  (including zero) as truthy, inconsistent with standard truthiness
  semantics. `0` and `0.0` are now falsy. Both the Rust backend and
  JavaScript client-side evaluation are fixed.

- **Unknown display condition operators silently showed fields** — A
  condition object with an unrecognized operator (e.g., a typo like
  `"greater_than"` instead of `"equals"`) silently defaulted to showing
  the field. Now logs a warning with the field name.

- **Richtext link modal allowed `javascript:` URLs** — The link insertion
  dialog accepted any URL protocol, including `javascript:`, `data:`, and
  `vbscript:`. The server-side renderer already blocked these at output
  time, but the editor now also validates on input — only `http:`,
  `https:`, `mailto:`, `tel:`, and relative URLs are accepted.

- **Negative LIMIT/OFFSET passed to SQLite** — `FindQuery` accepted
  negative `limit` and `offset` values, which have undefined behavior in
  SQLite. Now clamped to 0 before binding.

- **gRPC auth silently downgraded deleted users to anonymous** — When a
  valid JWT referenced a user that was subsequently deleted, the gRPC
  `resolve_auth_user` returned `Ok(None)` instead of an error, silently
  treating the request as anonymous. Now returns `unauthenticated` error.

- **Bulk `UpdateMany`/`DeleteMany` bypassed per-document access checks** —
  When no access function was configured for a collection, bulk operations
  skipped per-document access checks entirely instead of delegating to the
  default access system. Now always runs access checks regardless of
  whether an explicit access function is configured.

- **Back-references used wrong junction table for Group-nested fields** —
  `back_references.rs` constructed junction table names without the group
  prefix (e.g., `posts_tags` instead of `posts_meta__tags` for a field
  inside a Group), causing delete protection to miss references through
  Group-nested has-many relationships, Arrays, and Blocks.

- **Locale write path ignored inherited Group localization** — When a Group
  had `localized: true`, its sub-fields got locale-suffixed columns in the
  database (via migrations), but the write path (`locale_write_column`)
  only checked each field's own `localized` flag. Data was written to the
  unsuffixed column but read from the locale-suffixed one, causing apparent
  data loss. Now propagates `inherited_localized` through write paths.

- **`_status` column missing from locale-mode queries** — Collections with
  both drafts and localization enabled did not include the `_status` column
  in locale-aware SELECT queries, while the non-locale path included it.
  Downstream code inspecting `_status` would find it absent. Added
  `get_locale_select_columns_full` which includes `_status` when
  `has_drafts` is true.

- **Upload file cleanup skipped on `force_hard_delete`** — When
  `force_hard_delete` was used on a soft-delete upload collection, the
  upload file cleanup was skipped because the condition only checked
  `!def.soft_delete`. Now also cleans up files when `force_hard_delete`
  is true.

- **Lua sandbox allowed native C module loading** — `package.cpath` and
  `package.loadlib` were not removed from the Lua sandbox. A hook author
  who could place a `.so`/`.dll` in the package search path could load
  arbitrary native code. Now clears `package.cpath`, removes
  `package.loadlib`, and removes `string.dump`.

- **`user delete` CLI command bypassed ref_count** — The CLI user delete
  command called `query::delete` directly, bypassing ref count decrements.
  This left stale `_ref_count > 0` values on referenced documents, making
  them undeletable. Now uses a transaction with `before_hard_delete`.

- **gRPC `Me` endpoint checked `_locked` via field value** — The `Me`
  endpoint inspected `doc.fields["_locked"]` instead of using the
  `query::is_locked()` DB query. If `_locked` was stripped by field-level
  access controls, the check would always pass. Now queries the DB
  directly, matching the login endpoint behavior.

- **gRPC `RestoreVersion` used deferred transaction** — `restore_version_impl`
  used `conn.transaction()` instead of `conn.transaction_immediate()`,
  which could cause SQLite `BUSY` errors under concurrent writes. Now
  uses immediate transaction like all other write operations.

- **`sqlite_date_offset_expr` double-negation on negative input** — The
  function always prepended `-` to the seconds value. If a negative value
  was passed (future offset), it would produce `--30 seconds` which SQLite
  cannot parse. Now uses absolute value with explicit sign.

- **Join table names not quoted in SQL** — Array, Block, and Relationship
  join table SQL statements used unquoted table names, which could cause
  subtle errors if table names contained SQL reserved words. Now
  consistently double-quotes all join table names.

- **Non-ASCII `X-Created-Label` header silently failed** — The inline
  create panel's `X-Created-Label` response header failed silently for
  non-ASCII document titles (e.g., accented characters, CJK) because HTTP
  headers only allow visible ASCII. Now percent-encodes the label, and the
  JS side decodes it.

- **Version list pagination accepted `per_page=0`** — The version list
  page (collections and globals) had no lower bound on `per_page`,
  allowing `per_page=0` which produced infinite empty pages. Now uses
  `.clamp(1, max_limit)`.

- **Email verification allowed for locked accounts** — The verify-email
  endpoint marked locked users as verified, inconsistent with the
  reset-password handler which rejects locked accounts. Now blocks
  verification for locked accounts.

- **CSRF token not URL-decoded in `<crap-create-panel>`** — The create
  panel extracted the CSRF cookie value without `decodeURIComponent()`,
  while other components (delete dialog, conditions) properly decoded it.
  Could cause CSRF validation failures. Now uses a shared decode pattern.

- **`<crap-dirty-form>` catch handler cleared dirty flag** — When the
  confirm dialog promise rejected, the `.catch()` handler silently cleared
  `this._dirty`, removing unsaved-changes protection. Now preserves the
  dirty flag on rejection.

- **`<crap-conditions>` stale form reference after HTMX swap** — The
  `_initialized` guard prevented re-initialization after disconnect/
  reconnect, leaving `_debouncedServer` bound to a stale form element.
  Now resets `_initialized` in `disconnectedCallback`.

- **`<crap-list-settings>` used `innerHTML` with translation strings** —
  The add-filter button concatenated `t('add_condition')` into `innerHTML`,
  which could be an XSS vector if translation strings were attacker-
  controlled. Now uses `createElement`/`textContent`.

- **`<crap-sidebar>` Escape handler fired when sidebar closed** — The
  Escape key handler closed the sidebar unconditionally even when already
  closed, potentially interfering with other Escape handlers (modals,
  dialogs). Now only fires when the sidebar is open.

- **Logout route comment said GET/POST** — The `logout_action` handler
  comment incorrectly documented `GET/POST` but the route only accepts
  POST (correct for CSRF protection). Fixed the comment.

- **Polymorphic junction table rebuild dropped foreign key** —
  `rebuild_junction_table_for_polymorphic` (upgrading a has-many
  relationship to polymorphic) recreated the table without the
  `REFERENCES parent(id) ON DELETE CASCADE` constraint on `parent_id`.
  Cascading deletes stopped working for upgraded junction tables, leaving
  orphaned rows when a parent document was deleted.

- **Session version lookup swallowed DB errors** — `resolve_auth_user`
  (API) and `load_auth_user` (admin) used `unwrap_or(0)` on the session
  version query. A transient database failure returned 0, which matched
  tokens with `session_version = 0` (never changed password), bypassing
  session invalidation after a password change. Now propagates the error
  and rejects the token on failure.

- **Ref-count backfill interpolated count into SQL** —
  `increment_ref_count` in `backfill_ref_counts.rs` embedded the count
  value directly in the SQL string via `format!` instead of using a
  parameterized placeholder. Now uses `conn.placeholder(2)`.

- **Scaffold accepted arbitrary field names** — `parse_field_token` did
  not validate field names, allowing special characters (quotes, spaces,
  semicolons) that could produce broken or injectable Lua output in
  generated collection files. Now rejects names that aren't alphanumeric
  plus underscore. Block type names and labels are now escaped for safe
  Lua string embedding.

- **`forceHardDelete` bypassed referential integrity** — The Lua CRUD
  `crap.collections.delete()` with `forceHardDelete = true` skipped the
  `_ref_count` check entirely, allowing hard-deletion of documents still
  referenced by others. This corrupted ref counts on target documents and
  created dangling references. Now always checks ref counts for hard
  deletes regardless of how they are triggered.

- **Array/Blocks sub-field validation incomplete** — Fields inside Array
  and Blocks rows only ran 4 of 9 validation checks (required, date format,
  custom Lua validate, richtext node attrs). Missing checks: `min_length`/
  `max_length`, `min`/`max` numeric bounds, email format, select option
  validation, and has-many element validation. A Text field with
  `max_length = 10` inside an Array accepted values of any length; a Number
  field with `min = 0` accepted negatives; a Select field accepted values
  not in the options list.

- **`has_many` length validation counted bytes** — Per-element `min_length`
  and `max_length` checks on has-many Text fields used `.len()` (byte
  count) instead of `.chars().count()`. Multibyte UTF-8 values (emoji, CJK,
  accented characters) were overcounted, producing false validation errors.

- **Filter validation rejected fields inside layout wrappers** — Array,
  Blocks, and has-many Relationship fields inside Row, Collapsible, or Tabs
  wrappers were not found by `get_valid_filter_paths`, causing API filter
  queries on those fields to be rejected with "Invalid field". The same
  issue existed in `resolve_filter` (SQL generation stage) which also did
  a flat lookup. Both now recurse into layout wrappers.

- **Version snapshot restore lost Group fields inside layout wrappers** —
  `extract_snapshot_data` did not recurse into Row/Collapsible/Tabs wrappers
  nested inside Groups. Restoring a version snapshot silently dropped those
  fields. Refactored to a recursive prefix-based approach matching the
  write path.

- **No server-side password requirement on auth user creation** — Creating
  a user in an auth collection via the admin UI with an empty password
  field succeeded silently, producing an account with no password hash
  that could never log in. The client-side `required` attribute was the
  only protection. Now returns a validation error server-side.

- **Password policy error rendered broken page** — When password policy
  validation failed during create or update, the handler rendered
  `collections/edit_form` with an empty JSON context (`&json!({})`),
  producing a blank page with only the toast error. Now returns a 422
  with only the toast header, so HTMX preserves the form content and the
  user sees the error without losing their input.

- **API `parse_where_json` rejected numeric and boolean shorthand** —
  Filter queries like `{"active": true}` or `{"count": 42}` were rejected
  with "value must be string or operator object". Clients had to use the
  verbose form `{"active": {"equals": "true"}}`. Now accepts numbers and
  booleans as shorthand equals filters, consistent with `value_to_string`
  which already supported them. Also fixed inside `or` groups.

- **`UpdateMany`/`DeleteMany` skipped draft filtering** — Bulk update and
  delete operations did not apply the draft status filter, potentially
  affecting draft documents that should have been excluded. `find` and
  `count` correctly applied this filter. `UpdateMany` now respects the
  `draft` request field; `DeleteMany` defaults to published-only.

- **`UpdateMany`/`DeleteMany` missing mutation events** — Bulk operations
  did not publish mutation events to the event bus, so `Subscribe` stream
  listeners were never notified of bulk changes. Now publishes per-document
  events after commit.

- **CSRF token not URL-decoded in list-settings.js** — The column picker
  save handler did not `decodeURIComponent()` the CSRF cookie value,
  unlike every other CSRF reader in the codebase. Tokens containing
  URL-encoded characters would fail with a CSRF mismatch.

- **Richtext link `rel` attribute lossy on edit** — Editing a link with
  `rel="nofollow noopener noreferrer"` showed the nofollow checkbox as
  unchecked (strict `=== 'nofollow'` comparison), and re-saving stripped
  all other rel tokens. Now uses `.includes('nofollow')` and preserves
  existing tokens.

- **Relationship view link bypassed SPA navigation** — Dynamically setting
  `hx-get` on the relationship field "view" link did not call
  `htmx.process()`, so HTMX never registered the attribute. Clicking the
  link caused a full page reload instead of SPA-style navigation.

- **Slow resize test allocated 1 billion pixels** — The
  `resize_image_cover_extreme_aspect_ratio_no_overflow` test used 1000x1 →
  1x1000 dimensions, causing a 1000000x1000 intermediate allocation that
  took over 60 seconds. Reduced to 10x1 → 1x10 which exercises the same
  ratio math instantly.

- **JSON template helper missing single-quote escape** — The `{{{json ...}}}`
  Handlebars helper only escaped `</` (for `<script>` breakout prevention)
  but not single quotes. When used in single-quoted HTML attributes like
  `data-condition='{{{json condition_json}}}'`, a value containing `'`
  could break out of the attribute. Now escapes `'` to `\u0027` (valid
  JSON unicode escape, decoded transparently by `JSON.parse`). Affects
  display condition attributes on fields, collapsibles, rows, tabs, groups,
  and sidebar sections.

- **Soft-delete purge skipped ref count checks** (CRITICAL) — The
  scheduler's `purge_collection()` hard-deleted expired soft-deleted
  documents without checking `_ref_count`, without calling
  `before_hard_delete()` to decrement outgoing references, and without
  cleaning up FTS entries. This silently broke referential integrity:
  referenced documents were permanently deleted, and target documents
  retained phantom ref counts that blocked their deletion. Now mirrors
  the `empty_trash` logic: checks ref count (skips referenced docs),
  decrements outgoing refs, cleans FTS, then deletes.

- **`delete_document` blocked soft-delete of referenced documents**
  (HIGH) — The single-document `delete_document` service function
  unconditionally checked `_ref_count > 0` before any delete, including
  soft deletes. Every other code path (gRPC `DeleteMany`, Lua `delete`,
  Lua `delete_many`, admin `empty_trash`) correctly only checks ref
  count for hard deletes. Users could not soft-delete (trash) documents
  that were referenced by other documents. Now only checks ref count
  for hard deletes.

- **gRPC field-level write access bypass via `join_data`** (HIGH) —
  The gRPC `Create`, `Update`, and `UpdateGlobal` endpoints stripped
  denied fields from the `data` map but not from `join_data`. Array,
  Blocks, and has-many relationship data for access-controlled fields
  could still be written through the gRPC API. The bulk `UpdateMany`
  endpoint correctly stripped both maps. Now all endpoints strip
  `join_data` as well.

- **Sub-field custom validator errors silently passed** (HIGH) — When a
  Lua `validate` function threw a runtime error inside an Array or
  Blocks sub-field, the error was logged but validation silently passed.
  The top-level `check_custom_validate` correctly failed validation on
  error; the sub-field counterpart did not. Invalid data could be
  persisted through Array/Blocks fields. Now produces a
  `validation.custom_error` error, matching the top-level behavior.

- **`restore_global_version` skipped ref count adjustment** (HIGH) —
  Restoring a version snapshot on a Global never adjusted ref counts.
  If a relationship field changed between versions, restoring the old
  version left the new target's count too high and the old target's
  count too low. The collection `restore_version` handled this
  correctly. Now snapshots outgoing refs before restore and applies
  the diff after.

- **AfterChange hooks missing document `id` in `ctx.data`** — The
  AfterChange hook context for single `create` and `update` operations
  in the Lua CRUD API received `doc.fields` without the document `id`.
  Hooks needing to reference the document (e.g., for follow-up
  operations or notifications) had no way to get it. The bulk
  `update_many` path correctly included `id`. Now all paths include it.

- **Delete confirmation page used wrong access check for soft-delete** —
  The admin delete confirmation page always checked `access.delete`,
  even for collections with `soft_delete` enabled. Users with
  `access.trash` permission (but not `access.delete`) were blocked from
  viewing the confirmation dialog, even though the soft-delete action
  itself would succeed. Now uses `resolve_trash()` for soft-delete
  collections.

- **Upload update loaded old document without locale context** — The
  HTTP upload PATCH endpoint loaded the old document for file cleanup
  with `locale_ctx = None`, while the admin handler equivalent correctly
  passed the locale context. On localized upload collections, this
  could return wrong field values, causing incorrect file cleanup
  (orphaned files or premature deletion).

- **gRPC `FindByID` documentation contradicted behavior** — The
  RPC-level comment stated that `FindByID` returns an empty document
  field when no match is found and that `NOT_FOUND` is not returned.
  The actual implementation returns a `NOT_FOUND` status error, as
  correctly documented in the `FindByIDResponse` message comment.
  Fixed the RPC-level comment to match actual behavior.

- **Undelete action silently redirected on failure** — The admin undelete
  action logged errors but always redirected to the trash page,
  regardless of whether the undelete succeeded. Users had no indication
  of failure. Now returns an HTTP 500 error response on failure.

- **Proto `FieldInfo.type` listed nonexistent field types** — The
  proto comment listed `multiselect`, `point`, and `color` as valid
  field types, none of which exist. Updated to match the actual
  `VALID_FIELD_TYPES` list.

- **Proto `scheduled_by` documentation mismatch** — The
  `GetJobRunResponse.scheduled_by` comment listed `"scheduler"` but
  the code sends `"cron"`. Fixed the comment.

- **Cron schedule test non-deterministic** — `check_cron_schedules_skips_not_due`
  used `chrono::Utc::now()` with a 1-second window, which could
  non-deterministically fire if run at an exact hour boundary. Now uses
  a fixed time at minute :30 to guarantee deterministic behavior.

- **Lua RuntimeError lost anyhow cause chain** — All `RuntimeError`
  conversions in the hooks system used `format!("{}", e)` which only
  printed the top-level error message. Nested causes from `anyhow`
  errors (e.g., SMTP connection errors, DB errors) were silently
  discarded, making job failures and hook errors difficult to diagnose.
  Now uses `format!("{:#}", e)` to print the full cause chain.

- **`jobs list` CLI did not show errors** — The `jobs list` table only
  showed ID, Job, Status, Attempt, and Created. Failed jobs required
  `jobs show <id>` to see the error. Now includes a truncated Error
  column in the list view for at-a-glance diagnosis.

- **Example Lua files missing `overrideAccess`** — The example seed
  migration, jobs (`process_inquiry`, `cleanup_archived`,
  `weekly_report`), hooks (`prevent_last_admin`), and access strategy
  (`api_key_strategy`) did not pass `overrideAccess = true` to CRUD
  calls. After the `overrideAccess` default change to `false`, these
  all failed with "access denied" at runtime. All example Lua files
  now explicitly set `overrideAccess = true`.

- **AfterChange hooks missing `id` in draft-version update** (HIGH) —
  When saving a draft version via Lua `crap.collections.update`, the
  AfterChange hook context did not include the document `id` in
  `ctx.data`. Now includes `id` before running field-level and
  collection-level hooks.

- **AfterChange hooks missing `id` in unpublish** (HIGH) — The
  unpublish code path built the AfterChange context from
  `updated_doc.fields` without inserting the document `id`. Hooks
  could not identify which document was unpublished.

- **`update_many` AfterChange field hooks missing `id`** — The
  `id` was inserted into `after_data` after field-level AfterChange
  hooks had already run, so field hooks saw no `id` while
  collection-level hooks did. Moved the insertion before field hooks
  for consistency with single create/update.

- **Admin login leaked account state** (HIGH) — When a correct
  password was provided for a locked or unverified account, the admin
  login returned distinct error messages (`error_account_locked`,
  `error_verify_email`), confirming password correctness and account
  state to an attacker. The gRPC handler already returned a generic
  error for all cases. Now both return the same generic "invalid
  credentials" response, with the actual reason logged at debug level.

- **XSS in example richtext CTA/mention renders** (HIGH) — The
  example `init.lua` CTA and mention custom richtext node render
  functions interpolated user-controlled attributes (`url`, `text`,
  `style`, `name`) directly into HTML via `string.format` without
  escaping. A CMS author could inject arbitrary HTML/JS into the
  public-facing site. Now escapes all attributes with an
  `html_escape` helper.

- **Upload API delete used wrong access check for soft-delete** —
  The upload DELETE endpoint always checked `access.delete` even when
  the collection has `soft_delete` enabled. Now uses
  `resolve_trash()` for soft-delete collections, matching the gRPC
  and admin handlers.

- **Non-constant-time HMAC comparison in example** — The example
  `api_key_strategy.lua` compared HMAC signatures with `~=` (standard
  string comparison), enabling timing attacks. Now uses a double-HMAC
  pattern for constant-time comparison.

- **gRPC `UpdateMany` missing draft flag in validation** — The
  `ValidationCtx` for bulk updates did not pass the `draft` flag from
  the request. Draft bulk updates incorrectly enforced required-field
  validation, causing them to fail when required fields were omitted
  (which is allowed in draft mode).

- **Upload API leaked read-denied fields** — The upload POST and PATCH
  endpoints returned the full document in the response without
  stripping fields the user lacks read access to. Now applies
  `check_field_read_access` and removes denied fields before
  responding.

- **Lua `crap.globals.get`/`update` had no access control** (HIGH) —
  The Lua globals API bypassed all access control — no
  `overrideAccess` option, no collection-level checks, no field-level
  stripping. Any hook code could read and write all global data
  regardless of user permissions. Now supports `overrideAccess` option
  (default `false`), enforces collection-level read/update access, and
  strips field-level read/write-denied fields.

- **Lua `crap.globals.update` skipped validation** (HIGH) — Data
  written via `crap.globals.update()` bypassed all field validation
  (required, unique, length, numeric bounds, custom validators). Invalid
  data could be persisted directly. Now runs `validate_fields_inner`
  before writing.

- **Lua `crap.collections.create`/`update` leaked read-denied fields**
  — The returned document from Lua CRUD create and update operations
  included fields the user lacks read access to. The `find` and
  `find_by_id` functions correctly stripped these. Now all return paths
  (create, draft update, non-draft update) strip read-denied fields
  when `overrideAccess = false`.

- **`empty_trash` skipped lifecycle hooks** — The admin "Empty trash"
  action permanently deleted documents without running `BeforeDelete`
  or `AfterDelete` hooks. Hooks that perform cleanup side effects
  (cascade deletes, audit logging, external sync) were silently
  skipped. Now runs both hooks per document, matching the behavior of
  single delete and `DeleteMany`.

- **Windows build broken by Unix-only signal code** — `send_signal`,
  `is_process_running`, `stop`, `restart`, `status`, and
  `check_existing_pid` were not gated behind `#[cfg(unix)]`. The
  Windows CI build failed with unresolved `SIGKILL`/`SIGTERM` errors.
  All Unix-only functions and their call sites are now properly gated.
  On Windows, `--stop`/`--restart`/`--status` return a clear
  "not supported on this platform" error.

- **Upload file cleanup silently swallowed DB errors** (HIGH): When
  deleting an upload-collection document, the pre-delete query to load
  file paths used `.ok().flatten()`, silently discarding database errors.
  If the query failed, upload files were never cleaned up — leaking disk
  space permanently. Now logs a warning on failure.

- **Globals used hardcoded default LocaleConfig** (MEDIUM): The global
  update path used `LocaleConfig::default()` for reference counting
  instead of the actual configured locale. This could cause incorrect
  ref count snapshots in projects with non-default locale settings. Now
  extracts the locale config from the input's locale context.

- **Dashboard exposed collection metadata without access checks** (MEDIUM):
  The admin dashboard showed document counts and last-updated timestamps
  for all collections and globals regardless of the user's read access.
  Now skips collections/globals the user cannot read.

- **Sidebar navigation ignored access control** (MEDIUM): The sidebar nav
  listed all collections and globals regardless of the user's read access.
  Added `filter_nav_by_access()` to all admin page handlers. The collection
  list page (`/admin/collections`) also now filters by read access.

- **Multipart form field parse failure produced silent empty string**
  (MEDIUM): If a form field failed to parse (e.g., network interruption),
  the error was logged but the field was silently set to an empty string.
  Optional fields would lose data without any user feedback. Now propagates
  the error as a proper form validation failure.

- **Fragile `unwrap()` after `is_some()` guard in validation** (MEDIUM):
  `validate_scalar_field` checked `ctx.locale_ctx.is_some()` then called
  `.unwrap()` on a separate line. Refactored to `if let Some(lctx)` for
  safety against future refactors.

- **Unsafe UTF-8 byte slicing in image status display** (MEDIUM): Image
  queue status used `&e.id[..n]` byte slicing for display truncation,
  which panics if the offset falls within a multi-byte character. Changed
  to `chars().take(n).collect()`.

- **Regex compiled on every config env-var substitution call** (LOW):
  `substitute_env_vars` compiled a new `Regex` on each invocation. Moved
  to a `LazyLock` static for one-time compilation.

- **`from_utf8_lossy` silently replaced invalid UTF-8 in SQLite results**
  (LOW): SQLite text column values were converted with `from_utf8_lossy`,
  silently replacing invalid bytes with the replacement character. Now logs
  a warning when invalid UTF-8 is encountered before falling back to lossy
  conversion.

- **JSON API responses missing `charset=utf-8`**: Upload API `json_error`
  and `json_ok` helpers set `Content-Type: application/json` without
  charset, which could cause encoding issues with older clients. Now
  includes `charset=utf-8`.

- **MCP HTTP errors returned plain text instead of JSON-RPC**: Auth
  failures (missing/invalid API key) and body-too-large errors on the
  MCP HTTP endpoint returned plain text responses. MCP clients expecting
  JSON-RPC 2.0 format couldn't parse these errors. Now returns proper
  `JsonRpcResponse::error` with appropriate error codes.

- **Empty IN/NOT IN filter generated invalid SQL**: `FilterOp::In(vec![])`
  produced `field IN ()` which is invalid SQL. Empty IN now returns `FALSE`
  (`0 = 1`) and empty NOT IN returns `TRUE` (`1 = 1`).

- **Image resize integer overflow on extreme aspect ratios**: Resize
  dimension calculation used unchecked `f64 → u32` cast that could wrap
  on extreme aspect ratios. Now clamped to `u32::MAX`.

- **SVG CSP strengthened**: Added `default-src 'none'` alongside the
  existing `sandbox` directive for defense-in-depth on SVG uploads.

- **Group filter normalization missed layout wrappers**: Filtering on a
  Group field nested inside Row/Tabs/Collapsible failed because
  `normalize_field_name` only checked top-level fields. Now recursively
  searches through transparent layout wrappers.

- **Job retry with no backoff**: Failed jobs were immediately re-queued
  as `pending` with no delay, causing tight retry loops. Now uses
  exponential backoff (`min(2^attempt * 5, 300)` seconds) via a
  `retry_after` column.

- **Populate cache not locale-aware — cross-locale data leakage**: The
  relationship populate cache keyed on `(collection, id)` without locale.
  Two requests for the same document in different locales could return
  cached data from the wrong locale. Cache key now includes locale.

- **JWT secret loss on failed write**: If the auto-generated JWT secret
  could not be persisted to disk (permissions, full disk), the server
  started with an ephemeral secret. On restart, a new secret was
  generated, invalidating all sessions. Now fails to start instead.

- **Config validation gaps**: Added checks for `smtp_port = 0` when SMTP
  host is configured, `request_timeout = 0` / `grpc_timeout = 0` (use
  `None` to disable), and `grpc_rate_limit_window = 0` when rate limiting
  is enabled.

- **Cron `skip_if_running` TOCTOU race**: The check for running jobs and
  the insert of a new job were not atomic. Two scheduler instances could
  both see count=0 and both insert. Now wrapped in
  `transaction_immediate()`.

- **Join field populate with negative depth**: Join field population
  passed `depth - 1` without guarding `depth > 0`, allowing negative
  depth values. Now skipped when depth is exhausted.

- **Hardcoded English strings in UI components**: Drawer close button
  aria-label, confirm dialog fallback text, and toast colors now use
  `t()` translations and CSS custom properties respectively.

- **Card header text overflow**: Long card titles broke flex layout.
  Added `text-overflow: ellipsis` and overflow containment.

- **Cursor encoding error silently dropped**: `cursor.encode().ok()` discarded
  serialization errors, causing pagination to silently break. Now logs the
  error before returning `None`.

- **MCP resources returned empty JSON on serialization failure**: Three
  `unwrap_or_default()` calls in MCP resource endpoints silently produced
  empty strings when schema serialization failed. Now logs the error.

- **Richtext link dialog null dereferences**: Four `querySelector()` calls in
  the link modal's `applyLink()` function accessed `.value`/`.checked`
  without null checks, causing crashes if modal DOM was malformed.

- **Filter builder null dereferences**: `list-settings.js` filter builder
  accessed `.value` on `querySelector()` results without null checks.

- **Stale warning buttons used `onclick` property assignment**:
  `live-events.js` used `btn.onclick =` instead of `addEventListener()`,
  overwriting any existing click handlers.

- **Cursor pagination broken on numeric fields**: Cursor sort values were always
  bound as `TEXT`, so numeric columns compared lexicographically (`"9" > "10"`).
  Number values now bind as `INTEGER`/`REAL` and `NULL` cursors bind as SQL `NULL`
  instead of empty string.

- **Silent "unknown" block type on missing `_block_type`**: Block rows without a
  `_block_type` key silently defaulted to `"unknown"`, masking form parsing bugs
  and persisting unrenderable blocks. Now returns an error.

- **Version snapshot corruption silently lost**: Malformed JSON in version
  snapshots was swallowed via `unwrap_or(Null)`, permanently losing the snapshot
  data with no error. Now propagates the parse error.

- **Double-space labels for group sub-fields**: `to_title_case("seo__title")`
  produced `"Seo  Title"` (double space). Now filters empty segments from
  consecutive underscores, producing `"Seo Title"`.

- **`after_read` hook errors silently swallowed**: Hook failures were logged at
  WARN and the unmodified document was returned, serving stale data with no
  visible indication. Elevated to `error!` with full error chain.

- **Hook non-table return silently ignored**: If a Lua hook returned a string,
  number, or boolean instead of a table (common mistake), the original context
  was used with no feedback. Non-nil non-table returns now log a warning.

- **Form field read errors silently became empty strings**: Multipart form field
  read failures (e.g., truncated uploads) were hidden by `unwrap_or_default()`.
  Now logs the error before falling back.

- **Field name `__` collision with group naming**: Field names containing double
  underscores (e.g., `seo__title`) are now rejected during schema parsing, since
  `__` is reserved as the group field separator in column names.

- **Theme picker crash in restricted storage contexts**: `localStorage` access in
  the theme picker could throw in embedded iframes or with storage policies
  disabled. Wrapped in try/catch.

- **Dirty form / list settings listeners lost on DOM reconnect**: The
  `<crap-dirty-form>` and `<crap-list-settings>` components cleaned up
  document/window listeners in `disconnectedCallback` but did not reset their
  initialization guard, so re-insertion into the DOM left them inert. Guard is
  now reset on disconnect.

- **Page metadata stomping**: `with_pagination` no longer overwrites the `page`
  context object (title, type, title_name) with the pagination page number.

- **Admin socket address extraction**: The non-H2C admin server was not using
  `into_make_service_with_connect_info::<SocketAddr>()`, so `ConnectInfo`
  extraction failed at runtime — broke `trust_proxy` and per-IP rate limiting.

- **Relationship link URLs**: The join field template appended `/edit` to
  relationship item URLs (e.g. `/admin/collections/tags/123/edit` instead of
  `/admin/collections/tags/123`), causing 404s when clicking linked items.

- **Relationship search label association**: The `<crap-relationship-search>`
  input was missing an `id` attribute, breaking `<label for="...">` matching.

- **Relationship search null-safety**: `JSON.parse(getAttribute('selected'))`
  could return `null` instead of an array, causing a TypeError when iterating.

- **Join field label element**: The join field template used a `<label>` without
  a `for` attribute — changed to `<span class="form__label">` for correct
  semantics.

- **Richtext `<crap-node>` tag parsing**: The parser searched for `</crap-node>`
  before `/>`, so a self-closing tag before a full closing tag consumed too
  much content. Rewritten to find whichever closing pattern comes first.

- **Richtext node attr validation in nested fields**: Richtext fields inside
  array or blocks containers did not have their custom node attributes
  validated. Added recursive field walking to find all richtext fields.

- **Richtext node attr `required` skipped for drafts**: Required validation on
  custom node attributes fired even for drafts, blocking users from saving
  incomplete work.

- **Form validation double-submit**: The `<crap-validate-form>` component's
  `_runValidation()` could fire concurrently on rapid double-click. Added a
  guard flag to prevent concurrent validation requests.

- **Verification token expiry silent failure**: `find_by_verification_token`
  silently defaulted expiry to 0 on data corruption, making all tokens appear
  expired. Now uses proper error propagation (consistent with reset tokens).

- **`DeleteMany` file deletion before commit**: Upload files were deleted from
  disk before the DB transaction committed. If the commit failed, documents
  would reference missing files. Files are now deleted after successful commit.

- **Heading level not lower-bounded**: A ProseMirror document with
  `"level": 0` produced invalid `<h0>`. Now clamped to 1-6.

- **Job retry stale heartbeat**: `fail_job` with retry did not clear
  `heartbeat_at`, leaving a stale timestamp from the failed run.

- **`__INDEX__` partial replacement in array templates**: `replace()` only
  replaced the first `__INDEX__` occurrence per attribute. Changed to
  `replaceAll()` so nested templates work correctly.

- **Duplicate IDs in nested array templates**: When adding a parent array row,
  `_replaceIndexInNestedTemplates` replaced **all** `__INDEX__` placeholders —
  including those belonging to child array levels — corrupting nested templates
  so every child row cloned from them got identical hardcoded IDs. Rewritten to
  use targeted replacement based on the parent fieldset's `data-field-name`,
  replacing only the parent-level `__INDEX__` while preserving child-level
  placeholders. Also added nested template reindexing in `_reindexRows` so child
  templates reflect the correct parent index after drag-reorder.

- **Nested array actions fired twice (event bubbling)**: Click events on nested
  `crap-array-field` actions (add/remove/move/duplicate) bubbled up to the
  parent `crap-array-field`, which also handled them — doubling the effect
  (e.g., adding 2 sub-items instead of 1). Added ownership check so each
  component only handles actions belonging to its own level.

- **Nested drag-and-drop events bubbled to parent**: `_onDragStart`,
  `_onDragOver`, and `_onDrop` had no ownership checks. Dragging a nested
  array's row caused both parent and child components to handle the drag,
  potentially moving rows to the wrong container or corrupting indices. Added
  ownership checks for drag handles and container elements.

- **`_getDragAfterElement` selected nested rows**: The drop position calculation
  used `querySelectorAll('.form__array-row:not(...)')` which matched ALL
  descendant rows including those in nested arrays. Changed to
  `:scope > .form__array-row` to only consider direct children.

- **Nested `crap:request-add-block` event fired twice**: The
  `crap:request-add-block` custom event from `crap-block-picker` bubbled to
  parent `crap-array-field` components, causing duplicate block row additions.
  Added ownership check on the event target.

- **Listener accumulation on nested component reconnect**: Row move operations
  (`insertBefore`) triggered `disconnectedCallback`→`connectedCallback` on
  nested `crap-array-field` elements. Since `disconnectedCallback` reset
  `_connected` without removing listeners, each reconnect added duplicate
  handlers via fresh `bind()` calls. Stopped resetting `_connected` so listeners
  survive disconnect/reconnect cycles without accumulation.

- **Duplicated row label watcher skipped**: `_duplicateRow` cloned the row
  including `data-label-init="1"`, causing `_setupRowLabelWatcher` to bail out
  on the clone. Typing in the duplicate's label field never updated the row
  title. Now clears `data-label-init` before setting up the watcher.

- **`_setupBlockRowLabelWatcher` was exact duplicate**: Identical to
  `_setupRowLabelWatcher`. Removed and consolidated all callers to use the
  single method.

- **`getConfirmDialog()` null crash**: `dirty-form.js` called `.prompt()` on
  null when no `<crap-confirm-dialog>` exists. Added null guard with safe
  fallback.

- **Password max_length error message**: Said "characters" but checked bytes.
  Fixed to say "bytes" (intentional for Argon2 DoS prevention).

- **Richtext modals inaccessible**: Link and node edit modals were plain
  `<div>` overlays without focus trapping, Escape handling, or ARIA roles.
  Converted to native `<dialog>` elements with `aria-labelledby`.

- **Relationship search dropdown invisible to screen readers**: Added
  `role="combobox"`, `aria-expanded`, `role="listbox"`, and `role="option"`.

- **Hardcoded English in UI components**: Replaced hardcoded "Cancel",
  "Confirm", "OK", "Search..." strings with `t()` translations in confirm,
  richtext, and relationship-search components.

- **`channel_capacity = 0` startup panic**: Setting `live.channel_capacity = 0`
  in `crap.toml` caused a tokio panic at startup (`broadcast::channel` requires
  capacity > 0). Now caught by config validation with a clear error message.

- **Missing config validation for pagination limits**: `pagination.default_limit`
  and `pagination.max_limit` accepted zero or negative values. Negative
  `default_limit` passed through to SQL `LIMIT`, causing undefined behavior.
  Now validated: both must be > 0, and `default_limit <= max_limit`.

- **`default_locale` not validated against `locales` list**: Setting
  `default_locale = "en"` with `locales = ["de", "fr"]` was silently accepted,
  causing the default locale to have no storage columns. Now errors at startup.

- **Negative depth config accepted**: `depth.default_depth` and `max_depth`
  accepted negative values. Now validated: both must be >= 0.

- **SSE reconnection created duplicate `EventSource`**: If the SSE connection
  dropped and the component was reconnected during the 5-second retry window,
  both the timer callback and `connectedCallback` created new connections.
  Reconnect timer is now tracked and cleared on disconnect.

- **Array field index collision after row removal**: Removing a row and adding
  a new one could produce duplicate indices because `_afterRowChange()` did
  not call `_reindexRows()`. Indices are now resequenced on every row change.

- **Array checkbox/label association broken on new rows**: `_replaceIndexInSubtree`
  did not replace `__INDEX__` in `label[for]` attributes, so newly added array
  rows had non-functional checkbox labels. `_reindexRows` also did not update
  `id` or `label[for]` attributes, breaking label association after drag-reorder.
  Both methods now update all relevant attributes.

- **Web Component event listener accumulation**: Multiple components lacked
  `_connected` guards or reset their guard flag in `disconnectedCallback`,
  causing duplicate event listeners on DOM reconnect (HTMX swaps, drag
  reorder). Affected: `CrapArrayField`, `CrapConfirm`, `CrapTags`,
  `CrapDirtyForm`, `CrapCollapsible`, `CrapBlockPicker`, `CrapTabs`,
  `CrapFocalPoint`, `CrapListSettings`, `CrapUploadPreview`,
  `CrapRelationshipSearch`, and all picker components (`CrapThemePicker`,
  `CrapLocalePicker`, `CrapUiLocalePicker`). Symptoms ranged from
  double-toggling collapsible groups, duplicate block additions, drawer
  opening multiple times, to confirmed form submissions being blocked.
  Added `_connected` guards to all components; stopped resetting the flag
  in `disconnectedCallback`.

- **Relationship search state loss on reconnect**: `CrapRelationshipSearch`
  reset `_initialized` in `disconnectedCallback`, causing a full DOM rebuild
  (`innerHTML = ''`) on reconnect that destroyed selected items and search
  state.

- **Focal point `0` treated as center**: `parseFloat(...) || 0.5` in
  `CrapFocalPoint` treated a legitimate focal-point coordinate of `0` as
  falsy, defaulting it to `0.5` (center). Changed to explicit `Number.isNaN`
  check.

- **Dirty form re-queried form reference in disconnect**:
  `CrapDirtyForm.disconnectedCallback` called `this.querySelector('#edit-form')`
  to remove listeners. If the form element was detached before the wrapper,
  the query could miss it, leaking `input`/`change` listeners. Now stores the
  form reference during `connectedCallback`.

- **Tab keyboard navigation**: `CrapTabs` did not implement WAI-ARIA keyboard
  navigation. Added ArrowLeft/Right, Home/End key handling with proper
  `tabindex` management.

- **Relationship search stale dropdown**: A pending `fetch` from `doSearch()`
  could resolve after `closeDropdown()`, reopening the dropdown. Now
  increments the generation counter on close to invalidate in-flight searches.

- **Block row label watcher duplicate listeners**: `_setupBlockRowLabelWatcher`
  lacked the `labelInit` guard present in `_setupRowLabelWatcher`, allowing
  duplicate `input` listeners on reconnection.

- **Auth page cache-busting**: Login, forgot-password, and reset-password
  pages linked to `/static/styles.css` without the `?v={{crap.build_hash}}`
  cache-busting parameter used by other pages.

- **Missing favicon on standalone pages**: Forgot-password, reset-password,
  auth-required, and admin-denied pages were missing the
  `<link rel="icon">` tag, causing 404s for `/favicon.ico`.

- **gRPC reflection docs misleading**: Documentation implied reflection was
  always available. Clarified that `grpc_reflection = true` must be set.

- **Reset token expiry docs hardcoded**: gRPC docs said tokens expire
  "after 1 hour" instead of referencing the configurable `reset_token_expiry`.

- **`sanitize_locale` empty string invariant**: Added `debug_assert!` to catch
  pathological input (all non-alphanumeric characters) that produces an empty
  locale identifier. Panics in debug builds; documents the invariant.

- **`append_default_value` type mismatch warnings**: Now logs `tracing::warn!`
  when a default value type obviously mismatches the field type (e.g., string
  default on a Number field, bool default on a Text field).

- **Removed dead `FieldHooks::is_empty()`**: Unused `#[allow(dead_code)]`
  method — individual Vec fields are checked directly at all call sites.

### Changed

- **`overrideAccess` default changed to `false`** (BREAKING) — All Lua
  CRUD functions (`find`, `find_by_id`, `create`, `update`, `delete`,
  `count`, `update_many`, `delete_many`, `undelete`) now enforce access
  control by default. Previously they bypassed access checks unless
  explicitly set to `false`. This follows the principle of least
  privilege — hooks that need unrestricted access must explicitly opt in
  with `overrideAccess = true`. Collections without access functions are
  unaffected (no restriction configured = allowed).

- **Responsive breakpoint raised to 1024px** — The mobile layout
  (hamburger sidebar, stacked edit layout, static headers) now activates
  at 1024px instead of 768px/900px. Two-sidebar layouts (nav + edit
  sidebar) were too cramped on tablets and small laptops.

- **Sticky subheader simplified** — Removed duplicate `ResizeObserver`
  (was in both `sticky-header.js` and `list-settings.js`), eliminated
  the `--list-header-height` CSS variable (redundant with
  `--sticky-header-bottom`), and removed direct inline style
  manipulation fallback on the edit sidebar. The sticky subheader now
  breaks out of `.main` padding with negative horizontal margins for
  edge-to-edge coverage, fixing content bleed visible during scroll.
  On mobile, headers revert to static document flow — no sticky
  positioning, no overlap issues.

- **Consistent chip styling** — Relationship chips and tag input chips
  now use the same visual style: primary-tinted background, medium font
  weight, rounded corners, and a remove button with red hover state.

- **Hardcoded colors replaced with CSS variables** — Bare `#fff` and
  `white` values in CSS and web components replaced with
  `var(--text-on-primary)` or `var(--bg-elevated)` for proper theme
  support.

- **Button disabled state** — `.button:disabled` now shows 50% opacity
  with `not-allowed` cursor. Input fields (`input:disabled`,
  `select:disabled`, `textarea:disabled`) show dimmed text, grayed
  background, and block pointer events.

- **Missing i18n keys** — Seven JavaScript translation keys
  (`search_to_add`, `search`, `are_you_sure`, `ok`, `documents`,
  `error`, `no_details`) were used in web components but missing from
  the `#crap-i18n` data island. Now included. Added `error` and
  `no_details` keys to en/de translation files.

- **Email template colors** — Password reset and email verification
  templates updated from `#2563eb` to `#1677ff` to match the system
  primary color.

- **Delete protection expanded to all collections** — Previously only
  upload/media collections were protected from deletion when referenced.
  Now all collections are protected: attempting to delete a document with
  `_ref_count > 0` is blocked. Bulk `delete_many` silently skips
  referenced documents instead of failing.

- **Delete confirmation page uses lazy-loaded details** — The delete
  confirmation page now shows a fast "Referenced by N document(s)"
  summary from the `_ref_count` column. A "Show details" button
  lazy-loads the full back-reference list (which collections/fields
  reference the document) via a new
  `GET /admin/collections/{slug}/{id}/back-references` endpoint.

- **Richtext node attrs now use the field system** — `register_node` attrs are now
  defined with `crap.fields.*` factory functions instead of the old `{ name, type }`
  table syntax. Supports all scalar field types (`text`, `number`, `textarea`, `select`,
  `radio`, `checkbox`, `date`, `email`, `json`, `code`). Complex types are rejected at
  registration time. Node edit modals now support `placeholder`, `description`, radio
  groups, date pickers, email inputs, and monospace editors for code/json fields.

- **Full field feature support for richtext node attrs:**
  - Admin display hints: `hidden`, `readonly`, `width`, `step`, `rows`, `language`,
    `min`/`max`, `min_length`/`max_length`, `min_date`/`max_date`, `picker_appearance`
  - Server-side validation: `required`, `validate`, length/numeric/date bounds, email
    format, option validity — errors reference node location (e.g. `content[cta#0].url`)
  - `before_validate` hooks for normalizing attr values before validation
  - Registration-time warnings for features that have no effect on node attrs
    (`unique`, `index`, `localized`, `access`, `before_change`, `after_change`,
    `after_read`, `has_many`, `mcp`, `admin.condition`)

- **Scaffold `dev_mode`** defaults to `false` (was `true`). New projects start
  secure by default.

- **Admin templates**: Pagination variables now live exclusively under the
  `pagination` object (e.g. `pagination.prev_url` instead of `prev_url`).
  Templates using the `{{> components/pagination}}` partial work automatically.
  Custom templates that referenced top-level pagination keys (`page`, `per_page`,
  `total`, `total_pages`, `has_prev`, `has_next`, `prev_url`, `next_url`,
  `has_pagination`) must update to use the `pagination.*` prefix. The
  `has_pagination` key has been removed — use `{{#if pagination.has_prev}}`
  / `{{#if pagination.has_next}}` directly. The `pagination` object is always
  present when `with_pagination` is called, even on single-page results.

- **MCP `find` response**: Pagination metadata is now nested under a
  `"pagination"` key instead of being flat in the response object. The response
  shape is now `{ "docs": [...], "pagination": { "totalDocs": ..., ... } }`.

- **Admin templates**: The `items` context key for collection list pages is now
  `docs`, matching the naming used by MCP and gRPC. Update custom templates:
  `{{#if items}}` → `{{#if docs}}`, `{{#each items}}` → `{{#each docs}}`.

- **Upload cleanup guard**: `process_upload` now returns an RAII `CleanupGuard`
  that the caller must `.commit()` after their DB transaction succeeds. Prevents
  orphaned files when the DB write fails after files are already on disk.

- **CORS `max_age_seconds`** renamed to **`max_age`** for consistency with other
  duration fields. Accepts integer seconds or human-readable (`"1h"`, `"30m"`).

- **Scaffold CORS config** — `crap init` now outputs `max_age` instead of the
  old `max_age_seconds` in the commented CORS section.

### Security

- **Lua sandbox escape via `load()` / `loadstring()`** (CRITICAL): The
  Lua sandbox removed `loadfile` and `dofile` but not `load()` or
  `loadstring()`. A malicious hook could compile and execute arbitrary
  code with `load("os.execute('...')")()`, fully bypassing the sandbox.
  Now removes `load`, `loadstring`, `loadfile`, and `dofile`. Regression
  tests added for all four globals and a bypass attempt.

- **XSS via `javascript:` protocol in richtext links** (CRITICAL): Link
  marks in ProseMirror content rendered `href` attributes without URL
  protocol validation. A `javascript:alert('xss')` href executed
  arbitrary code when clicked. Now only allowlisted protocols (`http`,
  `https`, `mailto`, `tel`, `ftp`, relative paths) are rendered; all
  others are replaced with `#`.

- **Unescaped node type in `<crap-node>` tags** (HIGH): Custom node
  `data-type` attribute used `html_escape` (no quote escaping) instead
  of `html_escape_attr`. A crafted node type with quotes could break
  HTML attribute parsing. Fixed in both renderer and validation handler.

- **Session refresh allowed deleted users** (HIGH): The session refresh
  endpoint checked lock status and session version but never verified the
  user document still exists. A deleted user's session could be
  refreshed indefinitely. Now checks user existence first.

- **Locked accounts could reset passwords** (MEDIUM): The password reset
  flow did not check account lock status. A locked user could reset
  their password and regain access. Now rejects reset attempts for
  locked accounts.

- **gRPC reset password used wrong rate limiter** (MEDIUM): The gRPC
  password reset endpoint used `ip_login_limiter` instead of
  `ip_forgot_password_limiter`, allowing rate limit pool pollution
  between login and reset operations.

- **Date string slicing panic on multi-byte UTF-8** (MEDIUM): Date field
  value slicing used `&val[..10]` which panics if the byte offset falls
  within a multi-byte character. Changed to `.get(..10).unwrap_or(val)`.

- **String slicing panics on multi-byte UTF-8** (HIGH): Eight locations
  across the codebase used `find()` + byte-offset slicing (`&s[..pos]`)
  which panics when the offset falls within a multi-byte character.
  Affected: polymorphic ref parsing (3 sites), form bracket parsing,
  CLI key=value parsing, template path splitting, richtext attribute
  extraction, and timestamp normalization. All converted to `split_once`
  or guarded with `is_char_boundary`.

- **gRPC Subscribe connection limit TOCTOU race** (MEDIUM): The
  `fetch_add` + check pattern allowed concurrent requests to exceed the
  configured `max_subscribe_connections`. Replaced with a
  `compare_exchange_weak` CAS loop matching the SSE implementation.

- **`url_decode` garbled multi-byte UTF-8** (HIGH): Percent-encoded multi-byte
  sequences (e.g. `%C3%A9` for `é`, CJK, emoji) were decoded byte-by-byte as
  individual `char`s, producing mojibake. Malformed `%XX` sequences silently
  dropped characters. Rewritten to collect decoded bytes into `Vec<u8>` then
  convert via `String::from_utf8_lossy`; malformed sequences are now preserved
  literally.

- **NaN/Infinity accepted in number fields** (HIGH): Submitting `"NaN"`,
  `"inf"`, or `"-inf"` as a number field value parsed successfully and stored
  non-finite floats in the database. Added `is_finite()` check — non-finite
  values now coerce to `NULL`.

- **Rate limiter bypass via unparseable XFF** (HIGH): When `trust_proxy = true`
  and `X-Forwarded-For` contained a non-IP string, `client_ip()` used the raw
  garbage string as the rate limiter key. Attackers could vary this per-request
  to get unique rate limit buckets. Unparseable XFF now falls back to the TCP
  socket address.

- **SSE connection limit TOCTOU race** (HIGH): The SSE connection counter used
  `fetch_add` + check + `fetch_sub`, allowing a race where concurrent requests
  could exceed the configured `max_sse_connections`. Replaced with a
  `compare_exchange_weak` loop for atomic slot acquisition.

- **JSON template helper `</script>` breakout** (MEDIUM): The `{{{json ...}}}`
  Handlebars helper did not escape `</` in serialized values. A value containing
  `</script>` could break out of a `<script>` block in the admin UI. Now
  replaces `</` with `<\/` after serialization.

- **Pagination offset overflow** (MEDIUM): Extreme `page` values (near
  `i64::MAX`) caused integer overflow in `(page - 1) * limit`. Changed to
  `saturating_mul` to prevent panics.

- **Content-Security-Policy** (NEW): Admin UI now sends a CSP header by default
  with restrictive `default-src`, `frame-ancestors 'none'`, `form-action 'self'`,
  and `base-uri 'self'`. Inline scripts/styles are allowed via `'unsafe-inline'`
  (required for theme bootstrap, CSRF injection, and Shadow DOM components).

- **X-Forwarded-For bypass** (HIGH): `client_ip()` no longer trusts XFF by
  default. Without `trust_proxy = true`, the TCP socket address is used,
  preventing attackers from spoofing IPs to bypass per-IP rate limits.

- **Shared rate limiters** (MEDIUM): Admin and gRPC servers now share the same
  `LoginRateLimiter` instances, preventing attackers from doubling their attempt
  budget by targeting both servers.

- **Richtext node attr XSS** (HIGH): Custom node attribute values were rendered
  unescaped into `innerHTML` in the richtext editor modal and inline node
  display. Values containing `<`, `>`, `"`, `'`, or `&` could break the DOM
  or enable stored XSS. All interpolated values are now HTML-escaped. The
  server-side `before_validate` hook output is also escaped when
  reconstructing `<crap-node>` tags.

- **SSRF DNS rebinding closed** (HIGH): `crap.http.request()` now resolves DNS
  once, validates against the SSRF policy, and pins the validated IP via
  `reqwest::ClientBuilder::resolve()`. No second DNS lookup occurs at connect
  time — eliminates the TOCTOU DNS rebinding gap that existed with ureq.
  Redirects are individually resolved, validated, and pinned before following.

- **Migration concurrency** — `sync_all` now uses `transaction_immediate()` to
  serialize concurrent DDL operations via SQLite's write lock + `busy_timeout`,
  preventing schema corruption from concurrent startups.

- **Version uniqueness constraint** — UNIQUE index on `(_parent, _version)` in
  versions tables prevents duplicate version numbers from race conditions.

- **SSRF IPv6-mapped IPv4 bypass** (HIGH): `is_private_ip()` did not check
  IPv6-mapped IPv4 addresses (`::ffff:127.0.0.1`, `::ffff:10.0.0.1`, etc.).
  These bypassed the SSRF filter entirely. Now extracts the inner v4 address
  via `to_ipv4_mapped()` and re-checks it.

- **Field access fail-open on VM pool exhaustion** (HIGH): `check_field_read_access`
  and `check_field_write_access` returned empty denied lists (= allow all) when
  the Lua VM pool failed to acquire. Changed to fail-closed — all
  access-controlled fields are denied when the pool is unavailable.

- **Rate limiter IPv6 bypass** (MEDIUM): With `trust_proxy = true`, the raw
  `X-Forwarded-For` string was used as the rate limiter key. Different IPv6
  representations of the same address (e.g., `2001:db8::1` vs
  `2001:0db8:0:0:0:0:0:1`) got separate buckets. Now parsed as `IpAddr` and
  re-serialized to canonical form.

- **Logout CSRF** (LOW): The `/admin/logout` endpoint accepted GET requests,
  allowing forced logout via `<img src="/admin/logout">`. Now POST-only.

- **Upload serving path traversal hardening** (MEDIUM): The upload file
  serving endpoint relied solely on string-based `..`/`/`/`\` checks.
  Added canonicalization verification (`starts_with` on the canonical
  uploads directory) as defense-in-depth against symlink or encoding-based
  traversal vectors.

- **Upload file deletion path traversal hardening** (LOW): `delete_upload_files`
  joined document-stored URLs to the config directory without verifying the
  resolved path stayed within the uploads directory. A corrupted database
  record could cause arbitrary file deletion. Now canonicalizes and verifies
  the path stays within the uploads directory.

- **Lua package path injection** (MEDIUM): `setup_package_paths` interpolated
  the config directory path into a Lua code string without escaping. A
  directory name containing `"` or `\` could inject arbitrary Lua code.
  Replaced string interpolation with direct Lua API calls (`Table::set`).

- **PRAGMA table name validation** (LOW): `sqlite_get_table_columns` and
  `sqlite_get_table_column_types` interpolated table names into `PRAGMA
  table_info()` without validation. Added alphanumeric + underscore
  validation before PRAGMA execution.

- **MCP `safe_config_path` non-existent parent bypass** (LOW): When
  writing a file with a non-existent parent directory, `safe_config_path`
  skipped the canonicalization check entirely. Now walks up the parent
  chain to find the nearest existing ancestor and verifies it stays within
  the config directory.

- **Sensitive form Debug redaction** (LOW): `LoginForm` and `ResetPasswordForm`
  now redact passwords and tokens in their `Debug` output, preventing
  accidental exposure in logs.

- **UNIQUE constraint error leaks schema** (MEDIUM): gRPC error messages for
  unique constraint violations included internal table names (e.g.,
  `UNIQUE constraint failed: users.email`). Now sanitized to show only the
  column name.

- **MCP HTTP unauthenticated access** (HIGH): When `mcp.http = true` and
  `api_key` was empty, the MCP HTTP endpoint accepted unauthenticated requests
  with full CRUD access (MCP bypasses all access control). The server now
  requires an API key when MCP HTTP is enabled (config validation error at
  startup). The HTTP handler also rejects requests as a defense-in-depth guard.

- **MCP `exclude_collections` bypass** (MEDIUM): `exclude_collections` and
  `include_collections` only filtered the `tools/list` response — an attacker
  who knew a collection slug could call `find_<slug>` directly via
  `tools/call`. Collection filters are now enforced at execution time.

- **Lua `update_many` skipped validation and hooks** (HIGH): The Lua
  `crap.collections.update_many()` function only ran `BeforeChange` hooks and
  discarded their return value. It skipped `BeforeValidate` hooks, field
  validation (`required`, `unique`, custom `validate`), and field-level
  `before_change`/`after_change` hooks. Now runs the full write lifecycle
  matching the single `update` and gRPC `UpdateMany` paths.

- **Lua `update_many` field write access bypass** (MEDIUM): When called with
  `overrideAccess = false`, field-level write access checks were not applied.
  Now strips denied fields before the DB write.

- **IP rate limiter not cleared on successful login** (MEDIUM): The per-IP
  rate limiter was never cleared on successful login (only the per-email
  limiter was). Users behind shared IPs (NAT, VPN) could eventually get
  locked out despite successful logins. Both limiters are now cleared on
  success (admin and gRPC).

- **Lua `delete`/`delete_many` orphaned upload files** (MEDIUM): Deleting
  upload-collection documents via Lua hooks left files on disk. Now cleans up
  upload files after successful deletion, matching the gRPC path.

- **`sanitize_locale` empty string passes in release builds** (HIGH):
  `sanitize_locale` used `debug_assert!` which only fires in debug builds.
  An all-special-character locale string silently produced `""` in release,
  which gets interpolated into SQL as an empty identifier. Now returns
  `Result<String>` with a proper error, propagated through all callers.

- **Non-existent locale silently accepted**: `LocaleContext::from_locale_string`
  accepted any locale code without checking it exists in the config's locale
  list. Requesting a non-existent locale (e.g. `"fr"` when only `"en"` and
  `"de"` are configured) silently created a `Single("fr")` context. Now
  returns `None` for unknown locale codes.

- **Lua table conversion stack overflow** (HIGH): `lua_to_json` and
  `json_to_lua` recursed into nested tables with no depth limit. A deeply
  nested structure (65+ levels) caused stack overflow. Now capped at 64
  levels with a clear error.

- **Mixed-key Lua tables silently lost string keys** (HIGH): A Lua table
  with both integer and string keys (e.g., `{1, 2, name="test"}`) was
  treated as a JSON array, silently dropping string keys. Now detected
  and serialized as a JSON object preserving all keys.

- **Version table index name collision** (HIGH): Version table indexes
  used names like `idx_{slug}_parent_latest` that could collide with
  field-level indexes on fields named `parent_latest`. Namespaced to
  `idx__ver_{slug}_*`.

- **Polymorphic relationship upgrade left stale PRIMARY KEY** (HIGH):
  Upgrading a junction table from non-polymorphic to polymorphic added
  the `related_collection` column but didn't update the PRIMARY KEY
  constraint. Now rebuilds the table with the correct composite PK.

- **Silent NaN/Infinity and number overflow in gRPC conversion** (MEDIUM):
  Non-finite floats silently became `null` and overflowing numbers
  silently became `0.0` in protobuf conversion. Now logs warnings.

- **Event publishing error silently swallowed** (MEDIUM): Collection
  definition lookup failure during event publishing was discarded with
  `.ok()`. Now logs a warning.

- **Sessions not invalidated on password change** (HIGH): After a password
  reset, existing JWTs remained valid until expiry. Added a
  `_session_version` counter to auth tables that increments on password
  change. The version is embedded in JWT claims and checked on every
  authenticated request — stale tokens are rejected immediately.

### Internal

- Unified pagination output into `PaginationResult` struct + builder in
  `db::query`. All 4 entry points (gRPC, MCP, Admin, Lua) use a single
  computation path with thin format-specific adapters.

- Unified pagination input validation via `PaginationCtx`, reducing
  `validate_find_pagination` call signatures from 7 parameters to 4.

- Removed `pagination_builder.rs` (gRPC) and `find_pagination_input_builder.rs`
  (Lua) — consolidated into `db::query::pagination_result`.

- Removed 4 duplicated `resolve_sort()` implementations (now 1).

- Extracted CSRF and auth middleware from monolithic `server.rs`.

- Split oversized modules into focused submodules: auth handlers, field context
  enrichment, document types, hook context, MCP tool dispatch, scaffold hooks.

- Harmonized test macros and module imports across codebase.

- Extracted `get_text`/`get_opt_text` helpers in image queue code, replacing
  repeated match-and-clone blocks.

- Replaced `ureq` with `reqwest` (blocking + rustls-tls) for the Lua HTTP
  client. Enables DNS pinning via `ClientBuilder::resolve()` and reuses
  existing rustls/hyper/tokio transitive deps.
