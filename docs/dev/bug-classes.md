# Bug-Class Ledger

The registry of every recurring bug **class** found across the audit
programs (module sweeps, chokepoint rounds 1–10, future-break audit,
cross-module harmonization, the 2026-09 pre-tag audits), each with its
structural **guard** — the machinery that makes recurrence impossible or
loud. Mined from the CHANGELOG (Unreleased + alpha.9, ~2,300 entries)
and the sweep records, 2026-09-05.

**Why this file exists:** classifying the 2026-09 audit cycle showed
that of ~13 findings only one was a novel logic bug; the rest were
instances of classes already seen elsewhere. Fixing bugs doesn't stop
audits from finding more — killing classes does. Every class that got a
structural guard (the wire model, `config_doc_parity`, the
`field_children` classifier, the gen-* gates) has never reappeared.

## Triage rule

Every new finding — audit result, user report, review comment — is
triaged against this ledger **before** it is fixed:

1. **Instance of a GUARDED class** → the guard failed. Fix the guard
   (extend the pin/test/chokepoint), then the bug. A guard that let an
   instance through is itself class D4.
2. **Instance of a PARTIAL or UNGUARDED class** → fix the bug AND
   upgrade the class's guard. The class's row moves toward GUARDED.
3. **No matching class** → genuinely new information. Add a row, pick a
   guard from the toolbox, note the founding instance.

Fix discipline is unchanged: regression test first, then fix, then
CHANGELOG. New: the CHANGELOG entry (or commit message) names the class
ID it belongs to when one applies.

**Convergence metric:** the count of UNGUARDED/PARTIAL rows (drive
down), and the rate of genuinely-new rows per audit round (should trend
to zero). Audits stop when two consecutive full rounds produce only
guarded-class findings.

## Guard toolbox (strongest first)

1. **Direct wiring** — the consumer reads the truth constant; no copy
   exists (`make hook` positions return `ACCESS_KEYS` itself).
2. **Generation + `--check` gate** — `gen-proto`, `gen-lua-types`,
   `gen-wire-doc`, `gen-doc-tables`, `gen-template-doc`.
3. **Parity pin test** — curated artifact compared to a code inventory
   (`config_doc_parity.rs`, `wire_parity.rs`).
4. **Partition test** — A must equal B ∪ C (`GLOBAL_ACCESS_KEYS` ∪
   reject-list = `ACCESS_KEYS`).
5. **Compile-forced completeness** — exhaustive match / struct
   destructuring at the mechanism (`field_children`, `FILTER_OP_SPECS`
   consistency test).
6. **Structural meta-test** — a test that scans code shape
   (`surface_parity.rs` routing guard,
   `auth_revoking_handlers_request_invalidation`, `ACCESS_TOUCHPOINTS`
   staleness test).
7. **Chokepoint** — one function owns the concern; callers can't
   re-implement it (`decode_where_map`, `inherit_write_infra`,
   `queue_job`, `run_pool_write`).
8. **Boot/load-time validation** — misconfig fails startup with the
   source named (`validate_hook_references`, strict schema keys).
9. **Convention + review lens** — weakest; an idiom the sweeps enforce
   by hand (`inspect_err`, fail-closed `?`). A row resting only on this
   is PARTIAL at best.

Status legend: **GUARDED** (structural guard, tools 1–8) ·
**PARTIAL** (chokepoint or per-site tests exist, but a new site can
still miss it) · **UNGUARDED** (only fixed instances; convention at
most).

---

## S — Input strictness

| ID | Class | Guard | Status |
|----|-------|-------|--------|
| S1 | Unknown/typo'd key silently dropped | `deny_unknown_keys` (Lua), `#[serde(deny_unknown_fields)]` (~50 sites), `OpWire::lua_option_keys` | GUARDED |
| S2 | Present-but-wrong-typed value leniently coerced or defaulted | strict getters + `core::parse_bool`/`parse_truthy`; no meta-test — a new lenient getter can ship | PARTIAL |
| S3 | Two encodings of one value, only one handled (has_many array vs JSON string) | `decode_element_list`, `coerce/parse_has_many_scalar`, `ColumnSpec::ddl_type` | GUARDED |
| S4 | Malformed value silently accepted/coerced (number→NULL, unchecked date) | per-site strict checks (`check_date_field`, `number_violation`); convention beyond that | PARTIAL |
| S5 | Identifier grammar/collision missed (reserved names, 63-byte, slug prefixes, quoting) | `validate_slug`, `RESERVED_FIELD_NAMES`, `TZ_SUFFIX`/`LANG_SUFFIX`, `quote_ident` + DQS off, `identifier_check.rs`, `reject_reserved_tool_prefix`, `SYSTEM_JOB_SLUGS` | GUARDED |
| S6 | Config that can never fire — or is accepted then silently downgraded (`samesite="none"`→Lax, placeholder backends) | `reject_global_only_access_keys`, `warn_access_keys_without_features`, `status --check` 24-rule audit + startup nudge | PARTIAL |
| S7 | Unknown enum-ish string silently defaulted — incl. inside a dependency (`Region::FromStr` is infallible) | strict `FieldType::parse` + `FieldType::ALL`, typed config enums; boundary adapters (`create_s3_storage` bails on implicit `Region::Custom`) | GUARDED |
| S8 | Loose truthiness at a security gate (`_locked = "1"`); falsy-zero swallowing a real value (`\|\| 0.5`, `Number(0)` falsy) | typed coercion set at the evaluator; explicit `Number.isNaN`; Rust+JS evaluators fixed in lockstep | PARTIAL |
| S9 | Duplicate keys/elements silently collapsed or double-applied (HashMap form parsing truncated `<select multiple>`, dup field names → one column, dup has-many IDs double-incremented refs) | `Vec<(String,String)>` ingress, parse-time dup-name error (wrapper-flattened), dedup + `SELECT DISTINCT` | PARTIAL |
| S10 | Untrusted string used as filesystem/template path (traversal) | `validate_template_name` (15 vectors), storage `validate_key`, scaffold `validate_template_slug`, custom-page rules — 4 converging validators, no single chokepoint/meta-test | PARTIAL |
| S11 | Degenerate or cross-field-inconsistent config accepted at load, detonates at runtime (`channel_capacity = 0` panic, zero intervals busy-loop, `default_locale` ∉ `locales`, `default_limit > max_limit`) | per-key startup validation; no completeness rule for new numeric knobs | PARTIAL |

## F — Fail-closed security

| ID | Class | Guard | Status |
|----|-------|-------|--------|
| F1 | Fail-open on infrastructure error at any security/validation/existence predicate (`unwrap_or(false)`, `S3 exists()` swallowing 403→false, unique-check passing on DB error, conditions showing on error) | fail-closed idiom + per-site tests (`is_locked_propagates_query_error`), `deny_all_access_controlled`, `is_not_found_error` classifier; no lint | PARTIAL |
| F2 | Vacuous, dropped, or loose predicate silently widens (empty or-group, stripped `_status` filter, `contains("image")`, bidirectional MIME match) — or dup predicates silently narrow to empty | `decode_where_map` hard-errors; never-silently-widen rule; stripped predicates re-injected via typed lanes (`status_filter`), never dropped | GUARDED |
| F3 | Read-shaped operation with no access check | fixed sites + `ACCESS_TOUCHPOINTS` staleness test; a brand-new read op can still ship ungated | PARTIAL |
| F4 | Access evaluated with wrong/empty principal or context | `op::run`/`run_blocking` assemble principal + context once for all surfaces | GUARDED |
| F5 | Enforcement skipped on an embedded/derived view (populate targets, live events, versions) | shared `post_process` strip order; populate cache stores raw docs, access applied per request | PARTIAL |
| F6 | Filter-table access result collapsed to "allowed" at a boolean gate, or silently dropped on a write op | boolean-gate contract (deny + log); write ops enforce or reject filter tables (create rejects; restore checks parent id) | PARTIAL |
| F7 | Rate limiter keyed on un-normalized or attacker-supplied identity / shared keyspace/instance / non-atomic check | `normalize_email`, canonical `IpAddr`, XFF only from `trusted_proxies` (boot-validated), per-purpose keyspaces, atomic `check_and_block`, `rate_limit_backend = "redis"` for multi-node | GUARDED |
| F8 | Token purpose-confusion or entropy drift | `Claims.token_use` partition, single `validate_token`, single `generate_security_token` | GUARDED |
| F9 | Privilege revocation leaves a twin credential or live stream valid | `_session_version` bump chokepoint + structural test `auth_revoking_handlers_request_invalidation` | GUARDED |
| F10 | Cross-request/pooled state shares user- or tx-scoped data (populate cache, singleflight, pooled-connection session state after no-op recycle) | raw-doc cache + per-request strip + `:pub` namespace; `post_process` forces cache/singleflight off under `override_access`; per-conn state only via `post_create` | GUARDED |
| F11 | Check/strip ordering leaks data or budget (probe before auth, hook before strip) | per-site tests; convention | PARTIAL |
| F12 | Fail-open under backpressure — lag/close swallowed or warned-instead-of-dropped on revocation and live-event buses | fail-closed drop (revocation), drop-lagged-subscriber (live); convention | PARTIAL |
| F13 | Undecidable credential downgraded to anonymous instead of rejected | `Resolution::Invalid(Unaccepted)` variant in the one evaluator | GUARDED |
| F14 | Untrusted value interpolated into an interpreter/protocol sink without the sink's escaper (SQL values, Lua source, HTML text vs attr, JSON-in-`<script>`/attr, email CRLF headers, NUL bytes) | per-sink: `conn.placeholder`, Lua `Table::set`, `html_escape` vs `html_escape_attr`, `{{{json}}}` dual-escape, `validate_no_crlf` (+ queue-side defense), NUL rejection — no sink inventory pin | PARTIAL |
| F15 | Untrusted content interpreted as markup/code (39 `innerHTML` writes, HTML-payload uploads served as text/html, SVG entity expansion) | `h()` DOM builder (one annotated parse site left), MIME/extension cross-check + SVG `<!DOCTYPE>`/`<!ENTITY>` rejection at upload, nonce CSP without `unsafe-inline` | GUARDED |
| F16 | Sandbox capability denylist incomplete — removing A and B but not sibling C (`load`/`loadstring` after `loadfile`/`dofile`; `package.cpath`/`loadlib`/`string.dump`) | regression tests over the four globals + bypass attempt; needs an enumerated-allowlist pin over surviving globals | PARTIAL |
| F17 | Sensitive/internal detail escapes via a secondary channel — error bodies, `Debug`, logs, response shape, timing (SSRF error naming the IP, `Transient` detail echoed, bare-`String` secrets, account-state oracle, non-constant-time compares) | redacting newtypes (`JwtSecret`, `S3SecretKey`, …), scrubbed responders, `subtle::ConstantTimeEq`/`crap.crypto.constant_time_eq`; no partition test "every secret field is a redacting newtype" | PARTIAL |

## P — Surface parity & chokepoints

| ID | Class | Guard | Status |
|----|-------|-------|--------|
| P1 | Per-surface copies of one operation/grammar drift — incl. a second execution engine (`matches_constraints` in-memory evaluator) and duplicate parsers of one syntax | wire model + `wire_parity.rs` + one `decode_where_map`; the in-memory engine has no equivalence pin against SQL | GUARDED |
| P2 | Sibling missing a fix its twins got — sibling axes: API surface, entity kind (globals vs collections: `_tz` columns, defaults, ref counts), delete-path set, browser-side restatement of server rules | `wire_parity`, `surface_behavior_parity` (gRPC↔Lua); entity-kind parity rests on shared DDL/column-spec chokepoints; admin/MCP + client not pinned | PARTIAL |
| P3 | Capability/policy gate lives in a per-surface codec instead of the service op | op bodies own gates; `surface_parity.rs` routing guard blocks reaching past the service layer | GUARDED |
| P4 | Limit/depth/offset clamp missing on one surface | `apply_pagination_limits`, `clamp_depth`, `floor_optional_limit`, `PaginationCtx::resolve_limit`; frozen read-surface invariant | GUARDED |
| P5 | A path bypasses service invariants — CLI raw SQL, an admin handler calling a deep helper, or re-admitted stored data skipping validation | `cli_commands_write_only_through_reviewed_paths` (surface_parity): write primitives in `src/commands` confined to a reviewed, staleness-checked allowlist documenting the invariants each site hand-maintains; `WriteHooks::validate_fields` on restore. Working the guard found + fixed two live instances: CLI `user create` skipped `ref_count::after_create`+`fts_upsert`, `user delete` skipped `fts_delete` | GUARDED |
| P6 | Literal/predicate/selector re-spelled at N sites drifts — incl. template partials, JS selector/attr lists (`__INDEX__` set), cookie regexes, i18n/theme literals bypassing `t()`/CSS vars | named consts + per-literal pins, shared partials (`partials/field.hbs`), `static/components/util/*`; discovery is manual | PARTIAL |
| P7 | Context/param bundle rebuilt by hand, silently dropping a field | `inherit_write_infra`, `ServiceContextBuilder::infra`; inputs carry only per-call data | GUARDED |
| P8 | One concept spelled differently per surface (op names, casing, result keys) | `FilterOp::op_name`/`scalar_from_name`, snake_case decision, wire model option keys | GUARDED |
| P9 | Backend/platform-specific assumption breaks the sibling target (SQLite-isms on PG, Unix-isms on Windows) | `DbConnection` trait (`ddl_type`, `quote_ident`, `now_expr`, `greatest_expr`, `supports_fts()`); CI runs `sqlite+postgres` and `postgres-only` build+clippy+suite jobs; live-server PG smoke and Windows remain manual | PARTIAL |
| P10 | Create, alter, and table-rebuild paths provision differently (rebuild dropped FK/PK constraints) | `collect_system_columns` chokepoint; rebuild must preserve constraints — no pin | PARTIAL |
| P11 | Process-local state assumed cluster-global (per-node rate limits multiply the attacker budget, memory event transport, unsynced purge tickers) | `rate_limit_backend`/`[live] transport` redis backends, `try_claim_cron_window` + `_crap_cron_fired`, `FOR UPDATE SKIP LOCKED`; no inventory pin over "state that must be shared when nodes > 1" | PARTIAL |

## M — Mechanism coverage

| ID | Class | Guard | Status |
|----|-------|-------|--------|
| M1 | Tree walker doesn't descend a nested composite / layout wrapper | `core::walk::field_children` classifier, **exhaustive match in every walker** — new composite = compile error | GUARDED |
| M2 | Nested instance gets degraded handling vs top level (validation, normalization, hydration) | shared helpers per case (`check_date_field`, `canonical_json_array`); no meta-guard | PARTIAL |
| M3 | Status/lifecycle view filter missing on one read path (draft/trash/published, soft-deleted populate targets leaking raw IDs) | `resolve_draft` family, `published_only` + `JoinAccessCheck` in populate, frozen access-model contract | PARTIAL |
| M4 | Locale/variant companion column missed (`_tz`, `_lang`, `__locale`) — also blinds checkers into false orphan warnings | suffix consts, locale scope resolved like migration DDL, `ServiceContext` `locale_config` attachment; per-site fixes | PARTIAL |
| M5 | Hook/callback context missing a field it needs | typed context structs (single source for Lua shape); inner ctx is a deliberate superset | GUARDED |
| M6 | Unbounded recursion/size/connections on user-influenced input | hook-depth guard, `max_nesting_depth`, `lua_to_json` 64-level cap, HTTP/download/body/message size caps, SSE+Subscribe connection caps (CAS), pixel-per-byte ratio cap, bulk batch caps | PARTIAL |
| M7 | Component written but never wired in — typegen renderer not in `BLOCK_RENDERS`, web component defined but never placed in the DOM | `tests/wiring_completeness.rs`: every `render_*` fn must be referenced beyond its definition; every defined `crap-*` element must be placed in a template or another JS file; both with positive controls | GUARDED |
| M8 | Startup validation blind to a statically-known ref kind | `validate_hook_references` now covers collection/global/field hooks+access, conditions, jobs, auth methods, routes, admin gate; no meta-pin that every `HookRef`-typed field is visited | PARTIAL |
| M9 | Hazardous idiom at every call site, only symptom site fixed (deferred tx upgrade) | one-time audit (`transaction_immediate`); no lint | PARTIAL |
| M10 | Test/dev harness diverges from production wiring, hiding a surface | fixed instances (e2e Handlebars runner, `served_url` pair); convention | PARTIAL |
| M11 | Coverage stops at the in-process seam; transport layer untested | wire-level gRPC e2e (all 31 RPCs), browser e2e for admin | GUARDED |
| M12 | Client component lifecycle non-idempotence — connect/disconnect accumulates listeners or destroys state (14 components; SSE dup `EventSource`; editors losing state on row reorder) | `_connected` guards (19 components); the guard-flag/DOM-lifetime pairing is reasoned per component; browser e2e | PARTIAL |
| M13 | Nested component instance captures its descendants' events/DOM (double-fired bubbling events, drag selecting nested rows, `__INDEX__` replacing child placeholders) | event-target ownership checks, `:scope >` selectors; per-site | PARTIAL |
| M14 | Browser/platform semantics re-implemented by hand instead of delegated (FormData without submitter, multipart-vs-urlencoded, textarea LF rule, htmx shadow-root discovery) | delegate-to-platform principle (declarative htmx, native submission, `formnovalidate`); per-site | PARTIAL |
| M15 | Init-phase-only API silently half-applies when called at runtime (six registration APIs landing in one VM / bypassing migration) | `InitPhase` app-data marker + per-API rejection tests; needs completeness pin over registry-mutating Lua APIs | GUARDED |
| M16 | Accessibility/semantic contract missing on injected or custom UI (modals without `<dialog>`, dropdowns invisible to screen readers, missing `role="alert"`, broken label/for) | native `<dialog>`, WAI-ARIA roles per component; convention | PARTIAL |

## D — Drift (artifacts & meta)

| ID | Class | Guard | Status |
|----|-------|-------|--------|
| D1 | Two artifacts describe one truth and drift | per-pair: gen-* gates, `config_doc_parity` (docs + init template), `FILTER_OP_SPECS`, scaffold wiring, `TZ_SUFFIX` consts, `is_system_column` | GUARDED per known pair — full inventory sweep is Phase 2 |
| D2 | Docs/scaffold/example asserts a mechanism that doesn't exist, moved, needs the repo — or examples silently break on a default flip (`overrideAccess`) | fixed instances (git-mv recipe, grep-at-HEAD, Scenario 2); example boots in smokes but nothing executes example flows | UNGUARDED |
| D3 | Dead limb documented/advertised (field no code reads, phantom feature) | wire model kills the API side (schemas render from model); docs side manual | PARTIAL |
| D4 | **A guard that silently stopped guarding** — vacuous matcher, never-read heartbeat, always-0 exit, `debug_assert!`-only invariant, a suite CI never ran (139 browser tests, a full cycle) | positive controls on the structural scanners (`invalidation_scan_fires_on_synthetic_violation`, `cli_write_scan_fires_…`, `render_scan_fires_…`), `invalidation_write_ops_vocabulary_is_live`, allowlist staleness tests ×2, `ci_workflow_still_runs_every_gate` pin; gen gates self-check by diffing committed files | PARTIAL |
| D5 | Type model expresses a constraint the runtime ignores | e2e evaluator pins (`grpc_methods_evaluator`); per-feature | PARTIAL |
| D6 | Hand-edited generated artifact lost on regeneration | `--check` gates fail on hand edits; generated files carry AUTO-GENERATED headers | GUARDED |
| D7 | Stale comment/test conceals a real gap | process rule only (trace-full-path before concluding) | UNGUARDED |
| D8 | Round-trip drops — or accretes — data it didn't model (plugin round-trip, textarea whitespace accretion, EXIF orientation lost on re-encode) | strict parsing; accretion round-trip test; `core/upload/exif.rs` with per-orientation tests | PARTIAL |
| D9 | Frozen contract reshaped without upgrade path — or a migration gate too coarse (global backfill flag skipping later-added collections) | `frozen-contracts.md` + review; `gen-proto --check`; `_crap_meta` **versioned** gates; `#[serde(default)]` decode-compat for legacy tokens | PARTIAL |
| D10 | Dependency-boundary drift — upstream default/option/behavior relied on instead of pinned or adapted (`Validation::default()`, htmx 1.x keys silently dropped, infallible `Region::FromStr`, default flips on major bumps) | pinned constructors, explicit adapters at the boundary, `cargo audit` CI gate + `.cargo/audit.toml`, vendored bundles pinned by SHA-384 | PARTIAL |
| D11 | User overlay/override silently stops applying after an upstream move | source-version drift headers + `templates status`/`diff`/`layout` + `crap-cms status` customization summary | GUARDED |

## L — Logic, ordering, resources

| ID | Class | Guard | Status |
|----|-------|-------|--------|
| L1 | Hook result computed then discarded / hook on transparent wrapper never runs | wrapper-hook = hard parse error; per-path tests | PARTIAL |
| L2 | Pipeline stage ordering (hydration after hooks, snapshot before stamp) | hydration-before-`after_change` on every write path; per-path tests | PARTIAL |
| L3 | Side effect published before commit | queue-then-flush in `run_pool_write`; **frozen guarantee**: events only for committed writes | GUARDED |
| L4 | Non-atomic multi-step mutation leaves partial state — incl. non-transactional resources (FTS drop before validate, backup failing mid-run) | `transaction_immediate` envelopes; files deleted post-commit; upload `CleanupGuard` RAII (`.commit()` after DB tx); probe-before-destroy; DB-first-then-files ordering | PARTIAL |
| L5 | Concurrency: lost update / double execution / stale-response overwrite — server (CAS, cron windows) and client (in-flight search races, double-submit, stale cursor outliving its result set) | CAS terminal writes + `_crap_cron_fired`/`SKIP LOCKED`, single-flighted tickers, `AbortController`/generation counters/submit guards client-side | PARTIAL |
| L6 | Off-by-one at window/expiry boundary | fixed sites (`<=` cron window, MFA expiry aligned); convention | PARTIAL |
| L7 | Flag threaded through the API but ignored at the write edge | wire model kills advertised-but-ignored keys; behavior parity partial | PARTIAL |
| L8 | Infrastructure error surfaced as semantic result (404/INTERNAL confusion) | `ServiceError::classify`/typed variants; shared `json_*` responders; typed status mapping is a frozen contract | GUARDED |
| L9 | Error silently swallowed into absent/default (`let _`, `.ok()`) — and its sibling: a statement that succeeded but affected 0 rows, unchecked | `inspect_err` convention; fallible cache types; `affected == 0` checks; no lint | PARTIAL |
| L10 | Read path type-blind, returns raw storage form | write-edge canonicalization + shared converters; per-type tests | PARTIAL |
| L11 | N+1 / hot path pays for machinery it doesn't need | batched hydrate/populate; loadtest is the detector (manual) | PARTIAL |
| L12 | Resource lifetime/pool discipline violated (UAF slot, two-conn deadlock, blocking on async) | RAII guards (`TxSlot`), one-conn rule, `spawn_blocking` extraction rule; no lint | PARTIAL |
| L13 | Formatter/codegen mutates or mis-tokenizes its own input | **proptests**: idempotency + content-preservation; verbatim byte-ranges; golden compile tests (`generated_rust_parses`, kitchen-sink goldens) | GUARDED |
| L14 | Panic — or silent wrong answer — from untrusted input (byte-slicing UTF-8, byte-counted `min_length`, garbled `url_decode`, JS handlers aborted by `querySelector().value`/`JSON.parse(null)`/throwing `localStorage`) | char-safe helpers, `.chars().count()`, try/catch at client entry points; convention | PARTIAL |
| L15 | Absent optional collapses to a hard default instead of inheriting — incl. a UI empty state defaulting into a data-narrowing filter | `effective_max_attempts`-style resolution points; empty filter drawer renders zero rows | PARTIAL |
| L16 | Numeric overflow / lossy cast silently changes meaning (unchecked `*` on parsed sizes, `as` narrowing a PID to a process group, f64→u32 image casts) | `checked_mul`, `saturating_add`/`saturating_mul`, `i32::try_from`, `is_finite()`; lint candidate: ban narrowing `as` on input-derived values | PARTIAL |
| L17 | Partial outcome reported as complete success (skipped referenced docs uncounted, silent 10K bulk cap, first-error-only validation) | `skipped` counts, `RESOURCE_EXHAUSTED` on cap, full error lists; no pin that every clamped op signals truncation | PARTIAL |
| L18 | Ambiguous sentinel conflates two outcomes (`0` = "no refs" and "no document"; `None` = "disabled" and "invalid") | `Option`/enum return types at the fixed sites; convention | PARTIAL |

---

## Priority queue (what Phase 2/3 should guard next)

Remaining UNGUARDED: **D2** — smoke-execute documented CLI recipes and
example flows (`cli_binary_ops` style); **D7** — no structural fix,
folded into the review lens list below. (M7, P5 guarded and D4 hardened
in round 2026-09-05 (3).)

High-value PARTIAL hardening (new since the full-CHANGELOG pass):
**F14** sink inventory (one escaping-policy table over SQL/Lua/HTML/
email/JSON sinks) · **F17** partition test "every secret-typed config
field is a redacting newtype" · **F16** sandbox allowlist pin over
surviving Lua globals · **S11** completeness rule for numeric config
validation · **P11** inventory of node-local state vs the multi-node
contract · **L16** lint banning narrowing `as` on input-derived values
· **L17** truncation-signal pin over clamped bulk ops. Plus the earlier
queue: **P2** (admin/MCP behavioral parity), **M8** (meta-pin over
`HookRef`-typed fields), **M15** (InitPhase completeness pin).

## Appendix 1 — audit lens sets (reuse verbatim)

Per-module sweep lenses: logic/correctness · edge cases & boundaries ·
concurrency/transactions · error handling & silent failures · security ·
**future-break/freeze (mandatory)**. Meta-rules: agents overclaim — every
claim personally verified; regression test first; report the matrix
before fixing; divergence beats agreement.

Cross-cutting (harmonization) lenses: read-surface invariants (A) ·
fail-closed error handling (B) · strict input & structural parity (C).
Chokepoint-round lenses: cross-surface enforcement, ingress
normalization, SQL construction, cache keys, locale columns, error
classification, context construction, DDL/migration construction,
default-fallback resolution, serialization.

Freeze-lens categories: lenient/coerced input · defaults whose change
alters behavior · storage/serialization shapes · identifier/length
limits · naming/API surface · enum floors · on-disk/wire formats.
Tiering: fix-now / decide+fix / document-as-frozen.

## Appendix 2 — do-not-rechase (standing refutations)

Recorded so future audits don't re-litigate. Full detail in the sweep
memories; the load-bearing ones:

- `with_lua_db` TxContext panic-UAF claim — **refuted twice**; the
  leaked slot does not survive into a reused pooled VM (and the slot is
  now RAII anyway).
- `from_locale_string(None, …)` cannot `Err` — the admin
  `unwrap_or(None)` sites are dead-handling, not bare-column bugs.
  (Unknown *Some(bad)* locales were a separate, fixed issue.)
- `load_authenticated_user`'s `.ok()?` sites are fail-**closed**.
- Global version-table "double-wrapping" — consistent on read+write,
  verified by migration test.
- fmt best-effort unbalanced-nesting handling and `depth.saturating_sub`
  are intended (templates legitimately split tags across `{{#if}}`).
- Jobs reserved-tool-prefix (`many_`/`by_id_`) is inert — job slugs
  never build tool names.
- Config fail-safe operator-self-harm items (CSP header-value injection
  via own config, unvalidated host, negative `cache_size`) —
  deliberately not "fixed".
- SQLite-vs-PG timestamp/`transaction_immediate` differences don't bite
  within one deployment — by design.
- BIGINT widths / TEXT timestamps on PG: keeping is stable; only future
  narrowing would break. Don't narrow.
- 8-vs-9 `HookEvent` count: global-only `before_render` is intentional.
- The `walk_defs`/`DescentPolicy` mega-walker idea — rejected as
  dishonest abstraction; `field_children` is the answer.
- Over-abstraction declined by agreement: 4 CRUD surfaces, 3 document
  serializers, per-surface password extraction, 3 `FieldType` mappers.

## Maintenance

- Rows are **append-only**; a class is never deleted, only upgraded to
  GUARDED (with its anchor named).
- When a guard is extended, update the row's anchor list.
- New audit rounds add a dated one-line note here recording: findings
  count, how many fell into guarded classes, how many new rows.
- This file is dev documentation (not in the mdbook); the user-facing
  stability story lives in `docs/src/internals/frozen-contracts.md`.

**Round log:**
- 2026-09-05 — ledger founded from CHANGELOG (Unreleased + alpha.9) +
  sweep memories. 66 classes: 24 GUARDED, 37 PARTIAL, 5 UNGUARDED.
- 2026-09-05 (2) — full-CHANGELOG diff pass (alpha.8 → alpha.1, three
  diff-miners against the founded ledger). +18 classes (84 total; the
  client-side families M12–M16, injection/disclosure F14–F17, topology
  P11, S9–S11, D10–D11, L16–L18), ~25 rows widened, ~30 guard anchors
  added (all verified in-code), 1 stale row corrected: P9 falsely
  claimed "no PG CI" — CI has had `sqlite+postgres` + `postgres-only`
  jobs since alpha.8 (a live D4 instance inside the ledger itself).
  84 classes: 27 GUARDED, 52 PARTIAL, 5 UNGUARDED.
- 2026-09-05 (3) — Phase 2/3 round 1: guarded the top of the UNGUARDED
  queue. M7 → GUARDED (`tests/wiring_completeness.rs`), P5 → GUARDED
  (CLI write-primitive scan + reviewed allowlist; found and fixed two
  live P5 instances in the CLI user paths, regression test
  `cli_user_paths_maintain_ref_counts_and_fts`, fail-before proven),
  D4 → PARTIAL (positive controls + vocabulary liveness + CI-gate pin).
  84 classes: 29 GUARDED, 53 PARTIAL, 2 UNGUARDED (D2, D7).
