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

**Chokepoint check before every concrete fix.** Before writing a fix, ask
whether the bug is one copy of some logic disagreeing with another — an
encode, decode, shaping or lookup that the canonical path already does. If
so, the fix is to route every path through one chokepoint (the existing one,
or a new one every copy migrates to) plus a guard test that fails when a copy
drifts — never to patch the copy to imitate the other. The fix plan names the
chokepoint each fix lands in. Patching copies is what kept the Round 15
focused reviews finding new instances of the same cluster.

Fix discipline is unchanged: regression test first, then fix, then
CHANGELOG. The commit message names the class ID it belongs to when one
applies; CHANGELOG entries stay self-contained and never cite class IDs.

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
| S11 | Degenerate or cross-field-inconsistent config accepted at load, detonates at runtime | per-key startup validation in `config/validate.rs` + completeness pin `every_numeric_knob_is_validated_or_exempt` (pattern-matched numeric keys must be validated or carry a reviewed exemption with its reason) | GUARDED |

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
| F14 | Untrusted value interpolated into an interpreter/protocol sink without the sink's escaper | `tests/sink_escaping.rs`: the reviewed sink→escaper inventory (14 rows over 12 anchors: HTML text/attr, JSON-in-markup, SQL idents, email CRLF, Lua source, fs paths, DOM `h()`) with a row floor, per-anchor liveness pins on the real escaping calls + CRLF/NUL behavior pin + positive control | GUARDED |
| F15 | Untrusted content interpreted as markup/code (39 `innerHTML` writes, HTML-payload uploads served as text/html, SVG entity expansion) | `h()` DOM builder (one annotated parse site left), MIME/extension cross-check + SVG `<!DOCTYPE>`/`<!ENTITY>` rejection at upload, nonce CSP without `unsafe-inline` | GUARDED |
| F16 | Sandbox capability denylist incomplete — removing A and B but not sibling C (`load` after `loadfile`; **`io.popen` after `os.execute`** — found live by building this guard) | `sandbox_globals_match_reviewed_allowlist` pins the complete surviving global + `os`/`io`/`string` capability sets; per-capability regression tests; sandbox contract recorded in frozen-contracts.md | GUARDED |
| F17 | Sensitive/internal detail escapes via a secondary channel — error bodies, `Debug`, logs, serialization, timing | redacting newtypes (`JwtSecret`, `S3SecretKey`, `SmtpPassword`, `McpApiKey`, + new `RedisUrl`, `WebhookHeaders` — the partition test found all three missing ones on its first run), sentinel partition test `tests/secret_redaction.rs` over Debug AND Serialize, scrubbed responders, constant-time compares | GUARDED |
| F18 | Cross-subsystem namespace collision in a shared external store — two subsystems write one Redis under overlapping key prefixes, so one's bulk operation (a wildcard cache clear) destroys the other's security state (every rate-limit lockout) | cache keys confined to `{prefix}cache:` (`core::cache::keys`, the one key builder), startup refusal of an overlapping `auth.rate_limit_prefix` on the same Redis (`validate_redis_namespaces`), tests `a_cache_clear_cannot_reach_rate_limit_keys_under_default_prefixes` + `validate_rejects_a_rate_limit_prefix_inside_the_cache_namespace` | GUARDED |

## P — Surface parity & chokepoints

| ID | Class | Guard | Status |
|----|-------|-------|--------|
| P1 | Per-surface copies of one operation/grammar drift — incl. a second execution engine (`matches_constraints` in-memory evaluator) and duplicate parsers of one syntax | wire model + `wire_parity.rs` + one `decode_where_map`; the in-memory engine has no equivalence pin against SQL | GUARDED |
| P2 | Sibling missing a fix its twins got — sibling axes: API surface, entity kind, delete-path set, browser-side restatement of server rules | `wire_parity` (fields), `surface_behavior_parity` Phase 2 (gRPC ↔ Lua ↔ **MCP through the real JSON-RPC dispatch**: totals, filters, validation, uniqueness); admin covered by browser e2e + the routing guard pinning it to the same op bodies; entity-kind parity rests on shared DDL/column-spec chokepoints | GUARDED |
| P3 | Capability/policy gate lives in a per-surface codec instead of the service op | op bodies own gates; `surface_parity.rs` routing guard blocks reaching past the service layer | GUARDED |
| P4 | Limit/depth/offset clamp missing on one surface | `apply_pagination_limits`, `clamp_depth`, `floor_optional_limit`, `PaginationCtx::resolve_limit`; frozen read-surface invariant | GUARDED |
| P5 | A path bypasses service invariants — CLI raw SQL, an admin handler calling a deep helper, or re-admitted stored data skipping validation | `cli_commands_write_only_through_reviewed_paths` (surface_parity): write primitives in `src/commands` — credential writes included, in qualified and bare-imported form — confined to a reviewed, staleness-checked allowlist documenting the invariants each site hand-maintains; `WriteHooks::validate_fields` on restore. Working the guard found + fixed two live instances: CLI `user create` skipped `ref_count::after_create`+`fts_upsert`, `user delete` skipped `fts_delete` | GUARDED |
| P6 | Literal/predicate/selector re-spelled at N sites drifts — incl. template partials, JS selector/attr lists (`__INDEX__` set), cookie regexes, i18n/theme literals bypassing `t()`/CSS vars | named consts + per-literal pins, shared partials (`partials/field.hbs`), `static/components/util/*`; discovery is manual | PARTIAL |
| P7 | Context/param bundle rebuilt by hand, silently dropping a field | `inherit_write_infra`, `ServiceContextBuilder::infra`; inputs carry only per-call data | GUARDED |
| P8 | One concept spelled differently per surface (op names, casing, result keys) | `FilterOp::op_name`/`scalar_from_name`, snake_case decision, wire model option keys | GUARDED |
| P9 | Backend/platform-specific assumption breaks the sibling target (SQLite-isms on PG, Unix-isms on Windows) | `DbConnection` trait (`ddl_type`, `quote_ident`, `now_expr`, `greatest_expr`, `supports_fts()`); CI runs `sqlite+postgres` and `postgres-only` build+clippy+suite jobs; live-server PG smoke and Windows remain manual | PARTIAL |
| P10 | Create, alter, and table-rebuild paths provision differently (rebuild dropped FK/PK constraints) | `collect_system_columns` chokepoint; rebuild must preserve constraints — no pin | PARTIAL |
| P11 | Process-local state assumed cluster-global | redis backends + cron dedup + `SKIP LOCKED`; `deployment/multi-server.md` is the operator checklist, now pinned by `multi_server_doc_covers_every_node_local_subsystem` (8 subsystems incl. the newly documented per-node MCP session labels); new node-local state = add mechanism + doc row + pin entry | GUARDED |
| P12 | Copies of the value ↔ column path (snapshot build, draft save, draft read, version restore, per-read row decode, per-reader locale choice) disagree with the published path | chokepoints: `column_value` / `stored_document_values` (encode, array rows included), `decode_row` / `decode_value` (decode, array rows included), `LocaleContext::access_locale` / `read_locale` (locale choice); round-trip pin `tests/locale_value_matrix.rs` — a fresh draft reads as the published document in every locale mode, a restore reproduces every locale, lists read typed at the top level, in groups and in array rows | GUARDED |

## M — Mechanism coverage

| ID | Class | Guard | Status |
|----|-------|-------|--------|
| M1 | Tree walker doesn't descend a nested composite / layout wrapper | `core::walk::field_children` classifier (exhaustive over `FieldType`) is the sole composite-dispatch source — every field-tree walker routes through it, so a new composite is a compile error; **source-scan pin `tests/field_tree_dispatch.rs`** inventories every production `match …field_type` per file with dispatch counts (24 files / 27 sites, test modules stripped) and fails on a new hand-rolled dispatch outside the reviewed allowlist | GUARDED |
| M2 | Nested instance gets degraded handling vs top level (validation, normalization, hydration) | shared helpers per case (`check_date_field`, `canonical_json_array`); no meta-guard | PARTIAL |
| M3 | Status/lifecycle view filter missing on one read path (draft/trash/published, soft-deleted populate targets leaking raw IDs) | `resolve_draft` family, `published_only` + `JoinAccessCheck` in populate, frozen access-model contract | PARTIAL |
| M4 | Locale/variant companion column missed (`_tz`, `_lang`, `__locale`) — also blinds checkers into false orphan warnings | suffix consts, locale scope resolved like migration DDL, `ServiceContext` `locale_config` attachment; per-site fixes | PARTIAL |
| M5 | Hook/callback context missing a field it needs | typed context structs (single source for Lua shape); inner ctx is a deliberate superset | GUARDED |
| M6 | Unbounded recursion/size/connections on user-influenced input | hook-depth guard, `max_nesting_depth`, `lua_to_json` 64-level cap, HTTP/download/body/message size caps, SSE+Subscribe connection caps (CAS), pixel-per-byte ratio cap, bulk batch caps | PARTIAL |
| M7 | Component written but never wired in — typegen renderer not in `BLOCK_RENDERS`, web component defined but never placed in the DOM | `tests/wiring_completeness.rs`: every `render_*` fn must be referenced beyond its definition; every defined `crap-*` element must be placed in a template or another JS file; both with positive controls | GUARDED |
| M8 | Startup validation blind to a statically-known ref kind | `validate_hook_references` covers every ref kind; `Hooks`, `Access`, `FieldHooks` exhaustively destructured and the `AuthMethod` match wildcard-free inside the validator — a new hook slot/access key/auth variant fails to COMPILE until the validator learns it. JobDefinition/config-level refs stay review-time | GUARDED |
| M9 | Hazardous idiom at every call site, only symptom site fixed (deferred tx upgrade) | one-time audit (`transaction_immediate`); no lint | PARTIAL |
| M10 | Test/dev harness diverges from production wiring, hiding a surface | fixed instances (e2e Handlebars runner, `served_url` pair); convention | PARTIAL |
| M11 | Coverage stops at the in-process seam; transport layer untested | wire-level gRPC e2e (all 31 RPCs), browser e2e for admin | GUARDED |
| M12 | Client component lifecycle non-idempotence — connect/disconnect accumulates listeners or destroys state (14 components; SSE dup `EventSource`; editors losing state on row reorder) | `_connected` guards (19 components); the guard-flag/DOM-lifetime pairing is reasoned per component; browser e2e | PARTIAL |
| M13 | Nested component instance captures its descendants' events/DOM (double-fired bubbling events, drag selecting nested rows, `__INDEX__` replacing child placeholders) | event-target ownership checks, `:scope >` selectors; per-site | PARTIAL |
| M14 | Browser/platform semantics re-implemented by hand instead of delegated (FormData without submitter, multipart-vs-urlencoded, textarea LF rule, htmx shadow-root discovery) | delegate-to-platform principle (declarative htmx, native submission, `formnovalidate`); per-site | PARTIAL |
| M15 | Init-phase-only API silently half-applies when called at runtime | `InitPhase` marker + per-API rejection tests + completeness pin `every_registering_lua_api_is_init_phase_guarded` — building the pin found `crap.hooks.register`/`remove` UNGUARDED (runtime registration landed in one pooled VM, intermittent); both now rejected outside init | GUARDED |
| M16 | Accessibility/semantic contract missing on injected or custom UI (modals without `<dialog>`, dropdowns invisible to screen readers, missing `role="alert"`, broken label/for) | native `<dialog>`, WAI-ARIA roles per component; convention | PARTIAL |

## D — Drift (artifacts & meta)

| ID | Class | Guard | Status |
|----|-------|-------|--------|
| D1 | Two artifacts describe one truth and drift | per-pair: gen-* gates, `config_doc_parity` (docs + init template), `FILTER_OP_SPECS`, scaffold wiring, `TZ_SUFFIX` consts, `is_system_column` | GUARDED per known pair — full inventory sweep is Phase 2 |
| D2 | Docs/scaffold/example asserts a mechanism that doesn't exist, moved, needs the repo | `tests/docs_cli_smoke.rs`: every documented `crap-cms` invocation validated against the live CLI tree (fence-aware, with positive control), and the load-bearing template workflows (scenario 08 loop, scenario 02 extract targets, clean-layout answer) executed against a scaffolded config dir | GUARDED |
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
| L12 | Resource lifetime/pool discipline violated (UAF slot, two-conn deadlock, blocking on async) | RAII guards (`TxSlot`, `InfraRestore`), one-conn rule, `spawn_blocking`/`block_in_place` via shared `on_blocking_section` (now on the REST upload handlers' auth+gate prologue, `serve` auth, and both SSE/Subscribe pumps' batched Lua transform); `run_pool_write` drops its write conn before post-commit flushes. No known blocking-in-async site remains; a lint would make it GUARDED | PARTIAL |
| L13 | Formatter/codegen mutates or mis-tokenizes its own input | **proptests**: idempotency + content-preservation; verbatim byte-ranges; golden compile tests (`generated_rust_parses`, kitchen-sink goldens) | GUARDED |
| L14 | Panic — or silent wrong answer — from untrusted input (byte-slicing UTF-8, byte-counted `min_length`, garbled `url_decode`, JS handlers aborted by `querySelector().value`/`JSON.parse(null)`/throwing `localStorage`) | char-safe helpers, `.chars().count()`, try/catch at client entry points; convention | PARTIAL |
| L15 | Absent optional collapses to a hard default instead of inheriting — incl. a UI empty state defaulting into a data-narrowing filter | `effective_max_attempts`-style resolution points; empty filter drawer renders zero rows | PARTIAL |
| L16 | Numeric overflow / lossy cast silently changes meaning | narrowing-`as` lints (`cast_possible_truncation`/`_wrap`/`_sign_loss`) are ALREADY denied in production code (clippy pedantic at warn + CI `-D warnings`; tests opt out explicitly); overflow on parsed input guarded per-site (`checked_mul`, `saturating_*`, `try_from`, `is_finite`) — that half stays convention | PARTIAL |
| L17 | Partial outcome reported as complete success | `skipped` counts (positive case now pinned: `delete_many_reports_referenced_documents_as_skipped`), `LimitExceeded` on the bulk cap (pinned in `bulk_ops.rs`), full error lists | GUARDED |
| L18 | Ambiguous sentinel conflates two outcomes (`0` = "no refs" and "no document"; `None` = "disabled" and "invalid") | `Option`/enum return types at the fixed sites; convention | PARTIAL |

---

## Priority queue (what Phase 2/3 should guard next)

**Array/blocks write-preservation — DONE 2026-09-07** (nested-instance
degraded handling + write round-trip dropping unmodeled data). Array &
blocks writes were a destructive delete-and-reinsert with no stable
per-row identity, so a write-denied or absent sub-field on an existing
row was lost — the scalar path preserves it (column-preserving `UPDATE`),
the composite path did not. Fixed: the junction-row `id` round-trips
end-to-end and the rebuild is replaced with a diff-based,
column-preserving writer (arrays: UPDATE only present columns; blocks:
shallow-merge stored `data`). Details in
[`array-row-identity.md`](array-row-identity.md). The guard is the
regression suite (9 DB-layer + a founding end-to-end Lua test + an admin
enrichment test) plus the shared write-preservation contract now in
`frozen-contracts.md`. Follow-ups (refinements, not data-loss): version-
snapshot id on restore, gRPC/MCP wire tests, admin e2e.

Remaining UNGUARDED: **D7** only — no structural fix exists for stale
comments; folded into the review lens list below. Everything else from
the founding queues is guarded (M7, P5, D2) or hardened (D4).
All priority-queue items are guarded. The remaining PARTIAL rows are
the healthy chokepoint-backed kind that harden opportunistically when
their code is touched; none currently warrants a dedicated project.
Next program step: fresh-eyes convergence audits (two clean rounds),
then the tag gates.

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
- `ALTER TABLE ADD COLUMN` for `created_at`/`updated_at` intentionally omits
  the `DEFAULT now` that `CREATE` uses — SQLite forbids a non-constant default
  on an added column. Every write binds the timestamps explicitly, so the
  absent server default never surfaces. Not a create-vs-alter bug.
- Sort/filter never silently falls back to a default on invalid input.
  The admin list handler 400s an unknown/unsortable `sort`, an unknown
  `_status` value, and a drafts-only status filter on a no-drafts
  collection; the service `resolve_sort` `bail!`s on a sort column that
  is not a real column (`_rank` excepted, itself gated by
  `validate_query_fields`). The trash view prefers an explicit user sort
  over the trash default. No surface masks a bad sort/filter as a
  default-sorted or unfiltered result.
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
- Populate access/draft/trash gating, embedded-doc field strip,
  override-access cache isolation, polymorphic allowlist on write, depth
  caps, has-many order, back-reference gating — verified CLEAN (R12).
- Custom routes: reserved prefixes, method allowlist, CSRF double-submit,
  rate-limit keying behind `trust_proxy`, body limit both layers — CLEAN
  (R12). FTS query injection (both backends) and JSON-path injection in
  dot filters — CLEAN (R12).
- Client: `json` helper `</` escaping, CSP nonce on every inline script,
  richtext link protocol allowlist, SVG served as attachment + sandbox,
  server minting a fresh id for a duplicated row id — CLEAN (R12).

- Day-only date fields with a timezone rendering the previous day in list
  views — **unreachable** (R14): `parse_date_config` forces `timezone = false`
  unless `picker_appearance = "dayAndTime"`, and a timezone field's list cell
  renders as `dayAndTime`. The equality-filter corollary is the same
  unreachable configuration.
- A collection and a global sharing a slug mixing up their `access.admin`
  gates — **unreachable** (R15): the shared slug is rejected at definition time
  and again at startup, so the two gates can never be looked up under one name.

## Appendix 3 — verified CLEAN (skip in the next lens; re-check only when the code moves)

Each convergence lens ends with a CLEAN list. Re-verifying those areas the
next round is where most of a round's reading goes, so they are recorded
here per round; a lens prompt carries the instruction to skip them unless the
files changed since. Entries are dropped when the area is touched.

- **R26 (2026-09-24)**
  - *Jobs:* system-tx scope/flush, in-tx migration records, retention
    batching/resume/claim, `PurgeEvents` settle, claim, backoff docs,
    `ctx.job` fields, `ScheduledBy`, unique-key index, jobs CLI write pool,
    email send vs queue.
  - *Read access:* version find/list strip, `auth me`, admin label enrich,
    relationship search endpoint, unique-validation echo, describe (names
    only), restore gap report.
  - *Filter paths:* probe vs resolver, has-many list coverage, polymorphic
    has-many elements, localized junctions, MCP/gRPC docs.
  - *Uploads:* body-limit layering, derived-column strip on the trusted path,
    retention purge file deletion, hard-delete file ownership (row +
    snapshots).

- **R25 (2026-09-24)**
  - *Live events:* Subscribe and SSE share gate, access map and coalescing;
    delete/undelete/purge delivery verified at runtime on PG.
  - *Where decoding:* one `decode_where_map`/`decode_where_json` on every
    surface; has-many sort rejection mapped to invalid-argument everywhere.
  - *Label locale:* only `resolve_current` in production; the task-local is
    entered in the middleware and carried across blocking threads.
  - *Export/import & snapshots:* per-locale columns and join rows, canonical
    list shapes.
  - *Client:* SSE envelope matches `sse_payload.rs`; `h()`/toast sinks
    escape; CSRF on every htmx request and native submit.

- **R24 (2026-09-23)**
  - *Postgres (live):* the example project syncs, seeds and serves on PG16 —
    login, every collection list/edit, globals, dashboard, custom pages,
    sort/search/trash/filter; CLI status/user list/jobs list/export/bench.
  - *Live events:* no subscriber-facing encoder can emit the gate snapshot or
    unstripped data; per-subscriber strip order; commit-gated publishing on
    every delete path; toast coalescing escapes text.
  - *List view:* cells escape every field kind; column prefs keyed per user;
    trash/restore buttons gated by `perms`; pagination bounds.
  - *Typegen:* golden diffs are real in all six outputs; `_status`/`_deleted_at`
    emitted exactly with drafts/soft delete; array row ids only where relational.

- **R23 (2026-09-23)**
  - *Jobs catalogue/health:* gRPC/MCP/CLI/Lua list through one
    `service::jobs` chokepoint; access fails closed and hides denied jobs;
    one `stale_threshold_secs` for reclaim and healthcheck.
  - *Custom-route CSRF:* `admin::csrf` shared with the global middleware
    (header first, urlencoded `_csrf` second, constant-time, empty cookie
    never matches); dispatch order unchanged.
  - *Example job-manager plugin:* every Lua API call matches its signature;
    mutations go through access-gated chokepoints; no triple-stash.
  - *Validation:* `unique` (fails closed, self/soft-delete exclusion,
    localized columns), custom `validate` return shapes, layout-wrapper
    recursion and error paths, length/numeric/email checks, the row walker's
    error paths, `crap.validation_error` nonce.
  - *CLI:* dispatch/instance-lock/config-dir resolution, `serve`/`work` flags
    and PID semantics, non-TTY prompts fail fast, `export`/`import`
    atomicity, `make` overwrite refusal, `templates extract` exit code,
    `status` exit codes, the reviewed-write allowlist in `surface_parity.rs`.
  - *Admin extensions:* custom page route auth and slug validation, page
    access fail-closed and nav/route agreement, `template_data` soft failure,
    slot helper (cycle guard, raw-by-design output), overlay registration.

- **R22 (2026-09-21)**
  - *Generated descriptions:* the wire-model → proto → MCP-schema → Lua-option
    chain agrees on every one of the 17 messages once `Int32` and proto3
    presence are expressible (pinned by two new `tests/wire_parity.rs` cases);
    the client type printers' relationship/select/polymorphic/localized
    handling; the `read_shape_fields` boundary — every read-describing surface
    uses it and no write, validation, migration or column-name path does.
  - *Readonly cascade:* every `SubFieldOpts` / `ChildEnrichOpts` / `EnrichCtx`
    construction in production threads `ancestor_readonly`; no copy of
    `field.admin.readonly || locale_locked` survives outside the shared
    helper; the locale lock is never inferred from readonly or vice versa.
  - *Templates:* the `../readonly` depth at all four row-header call sites in
    `fields/array.hbs` and `fields/blocks.hbs`, including the two differently
    nested new-row `<template>` blocks.
  - *Export/import:* round-trip fidelity re-checked; the only finding was a
    documentation gap (export bypasses `access` and read hooks by design).

- **R18 (2026-09-19, commit b86a3f9e)**
  - *Atomicity:* pool write envelope (`service/orchestrate.rs`) release-
    before-effects on both paths; scoped Lua tx per-tx queues handed up only
    on commit; upload `CleanupGuard` key coverage and in-tx dropped-key diff;
    S3/local storage put/get/delete status checks, temp+fsync+rename; image-
    convert job idempotent re-encode + source-URL guard; job queries CAS on
    `(running, attempt)`; scheduler non-clean outcomes all leave `running`;
    bulk runs `max_attempts = 1`; delete paths resolve files in-tx, delete
    post-commit; `create_version_and_prune` lock→insert→prune; migrations
    one IMMEDIATE tx under the advisory lock, gates stamped with their work,
    idempotent re-run, FK window restored or boot fails; verification email
    minted on the account's tx; login limiters fail closed; backup/restore
    staging; VM pool RAII; serve shutdown drains.
  - *Config/ops:* duration/filesize parsing edge cases; numeric-knob
    validation pin; ports, trust_proxy, CORS, MCP key, Redis-implies-secret,
    rate-limit prefix overlap, VM pool clamp; locale validation (except the
    case-insensitive collision fixed this round); env substitution skips
    comments; secret file locking/mode/staging; alpha.9→alpha.10 migration
    idempotency and backend split; guide Actions 27/31/33/34/46/49 checked
    for substance; backup/restore/migrate fresh/db cleanup/console gates and
    locks; secret-bearing newtypes redact over Debug+Serialize, no log line
    prints a secret; instance lock semantics for serve/work/mcp/CLI.
  - *Round-trip:* one encode/decode for columns (`encode.rs`); checkbox
    truthiness one rule; empty string → NULL uniform; number spellings and
    non-finite rejection; has-many scalar canonical JSON at every ingress;
    date grammar parity validator↔normalizer, tz companion, DST rejection;
    text/email canonical form at every depth; relationship id shapes;
    select unlisted tolerance; upload `sizes` folded once; gRPC and Lua
    codecs round-trip (documented non-identities pinned); companions in
    nested rows.
  - *Lua API:* sandbox removals and chunk names; limits armed on every entry
    point (coroutine hole fixed this round); strict unknown-key rejection on
    every option table; hook-return interpretation per docs for every hook
    kind; access-rule verdicts and constraint grammar; CRUD availability/tx
    model, hook-depth guard; init-phase gating; docs↔code parity for the
    namespace pages listed in the R18 report.
  - *Admin UI:* CSRF coverage of every mutating route, double-submit
    constant-time; auth chain and admin gates fail closed; session cookie
    flags, refresh checks, no fixation; MFA pending token lifetimes, TOTP
    URI encoding; XFF only from trusted proxies; static assets (no traversal,
    ETag, `no-store` on HTML); dev mode scope; every triple-stash escaped,
    JS `h()` builder; i18n keys resolve (JS-list drift fixed this round);
    form error re-render preserves values, never echoes password; list
    pagination/sort/filter validation (`_status` fixed this round); search
    endpoint access + locale; dashboard per-collection cost; htmx partial
    contract for GET navigation; multipart lifecycle.

- **R19 (2026-09-20, commit 1aea0304)**
  - *API surface:* bearer/API-key extraction parity gRPC↔REST; auth before
    existence disclosure on account actions; pagination and depth clamping;
    locale/draft downgrade semantics; `where` parsing (proptest); error →
    Status/HTTP mapping leak-free; Subscribe lifecycle (RAII limit,
    fail-closed lag/revocation, drain); REST upload auth-before-body; login/
    MFA/reset rate-limit atomicity and non-enumeration; TriggerJob delay and
    concealment; bulk queue actor/auth and `bulk_max_documents` parity; gRPC
    limiter TCP-peer keying; proto↔wire parity (gated); FieldValue codec.
  - *Hooks execution model:* write/read/broadcast ordering per
    `lifecycle-events.md`; field-vs-collection-vs-registered ordering;
    `ctx.document` per-pass snapshot; strip-before-hooks by design;
    `after_change` sees the persisted row; `after_read` fail-open; hook-depth
    guard on every CRUD entry; TxContext/read-only gating; exhaustive
    destructuring of collection `Hooks`/`Access`.
  - *MCP:* HTTP auth-before-session; sessions audit-only; `execute_crud_tool`
    re-checks include/exclude + `access.mcp` at call time with one
    `UnknownTool` answer; batch handling; job/config tool tiering symmetric
    at list and call; error scrubbing on every `exec_*`; `where` grammar
    shared; config-file secret redaction structural; client-name sanitised.
  - *Versions × locale × soft delete:* restore on a trashed document refused;
    ref-count diff on restore (collections and globals); pruning never drops
    the sole published snapshot; locale added/removed after snapshots; draft
    read fallback published→null; trash keeps the pending draft, undelete
    restores it; hard delete/purge one chokepoint (CLI, scheduler, bulk);
    globals cannot own upload files; `list_versions` access + pagination;
    globals reject `required_locales` as unknown.
  - *Read path:* populate cache key and write-through invalidation; trash/
    draft/access gating on populate targets incl. nested containers and
    joins; FTS sanitisation both backends; every `FilterOp` both backends;
    keyset NULL/duplicate stability; count/find parity; SQL↔in-memory filter
    agreement.

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
- 2026-09-05 (4) — Phase 2/3 round 2: D2 → GUARDED
  (`tests/docs_cli_smoke.rs`; its first run caught 3 scan-calibration
  cases), F17 → GUARDED (`tests/secret_redaction.rs`; first run found
  3 REAL leaks — both redis-URL passwords and webhook Authorization
  values readable via Debug/logs/serialize — fixed with `RedisUrl` +
  `WebhookHeaders` newtypes), F16 → GUARDED (allowlist pin; building it
  found **`io.popen` live in the hook sandbox** — process execution,
  `os.execute`'s sibling — removed, contract frozen). 84 classes:
  32 GUARDED, 51 PARTIAL, 1 UNGUARDED (D7).
- 2026-09-05 (5) — Phase 2/3 round 3 (PARTIAL queue): M15 → GUARDED —
  the completeness pin found **`crap.hooks.register`/`remove` live at
  runtime** (landed in one pooled VM, intermittent across requests);
  both now init-phase-gated with regression tests. M8 → GUARDED —
  `Hooks`/`Access`/`FieldHooks` exhaustively destructured and the
  `AuthMethod` match wildcard-free inside `validate_hook_references`,
  so new ref slots fail to compile at the validator. L16 verified:
  narrowing-cast lints already denied via pedantic + `-D warnings`
  (row corrected — it understated existing machinery, a mini-D4).
  84 classes: 34 GUARDED, 49 PARTIAL, 1 UNGUARDED (D7).
- 2026-09-05 (6) — Phase 2/3 round 4 (PARTIAL queue): F14 → GUARDED
  (sink→escaper inventory, `tests/sink_escaping.rs`), S11 → GUARDED
  (numeric-knob completeness pin with reviewed exemptions), L17 →
  GUARDED (positive skip-signal pin), P11 → GUARDED (multi-server doc
  as pinned operator checklist + MCP session-label stickiness row
  added). Fallout from M15 cleaned: 3 more runtime register/remove
  tests converted/unit-covered. 84 classes: 38 GUARDED, 45 PARTIAL,
  1 UNGUARDED (D7). Priority queues exhausted — remaining large item:
  P2 behavioral-parity harness.
- 2026-09-05 (7) — P2 → GUARDED: `surface_behavior_parity` Phase 2
  folds MCP in through the public `McpServer::handle_message` JSON-RPC
  entry (tool routing + argument parsing + result envelope all in the
  loop); totals, filters, validation rejection, and unique enforcement
  now pinned identical across gRPC/Lua/MCP. Admin stays with browser
  e2e + the routing guard, documented in the row. FINAL queue state:
  84 classes — 39 GUARDED, 44 PARTIAL, 1 UNGUARDED (D7). The guard
  program is complete; convergence audits are next.
- 2026-09-05 (10) — **Deferred-item cleanup + convergence-bar
  correction.** Fixed everything rounds 1–2 had recorded-but-deferred:
  the four L12 blocking-in-async sites (REST upload create/update/delete
  prologues + `serve` auth on the blocking pool; both live-event pumps
  batch their per-event Lua strip/`after_read` into one blocking hop
  before async forwarding), the `checkbox_columns` migration gate made
  **per-slug** (was a global flag that would skip a later-added table —
  D9 granularity), and a frozen-contract note on the create-only
  `_versions_{slug}` path. **Corrected the premature "converged" call:**
  two rounds finding ~20 real bugs each is not convergence. The criterion
  is a genuinely quiet round; more audit rounds are needed before the tag
  gates. Tag-gate work (loadtest, release-target cross-builds) is
  DEFERRED until a round comes back clean.
- 2026-09-05 (9) — **CONVERGENCE ROUND 2** (4 adversarial lenses:
  concurrency/ordering, wire-option honesty, freeze/upgrade gates,
  resource/lifetime). **~18 findings — every one an instance of an
  existing class; 0 new classes.** Fixed this round: F2 MCP wipe
  (non-object `where` → match-everything on `delete_many`; hard-errors
  now, parity test), queued-bulk visibility HIGH (stripped payload
  couldn't decode → run invisible to its queuer; `BulkRunIdentity`
  projection + failure-path strip), C1 `crap.transaction` rolled-back
  events published (fresh per-tx event/verification queues, regression
  test), C2 conn-mode delete files-before-commit (post-commit
  `FileCleanupQueue` on both flush points, regression test), C3 version
  restore skipped FTS re-sync, C4 user-settings lost update (IMMEDIATE
  tx), C5 Postgres job-claim outside a tx (per-slug caps only advisory
  across nodes; unified tx path), R4 `run_pool_write` held its write
  conn across the deferred-effect flush (two-conn deadlock shape),
  search endpoint L12 (`block_in_place`). Documented: TOTP/MCP-session/
  unique-delay/ranked-search contracts to add to frozen-contracts;
  checkbox + `_versions_` migration-gate granularity notes; upload/SSE
  L12 instances tracked on the L12 row. Migration gates + decode-compat
  (cursor, snapshot, `_totp_*`, `result_json`) all proved sound.
  **NOTE (corrected 2026-09-05 (10)): NOT yet converged.** Rounds 1 and 2
  each found ~20 real, fixable bugs — several serious. "Every finding is
  an instance of an existing class" is too weak a bar once the ledger has
  84 rows (almost anything matches some class). Real convergence = a
  round that comes back genuinely quiet. Rounds 1–2 also LEFT deferred
  items (the L12 upload/SSE instances, migration-gate granularity) — not
  legitimate for a "converged" verdict.
- 2026-09-05 (8) — **CONVERGENCE ROUND 1** (4 adversarial lenses:
  client-side, sink call-sites, disclosure, newest-feature fail-open).
  **21 findings — every one an instance of an existing class; 0 new
  classes.** Highest-severity: F17 guard failure (`database.url`
  Postgres password readable via MCP `crap://config` + `crap.config` —
  the partition fixture never used a PG URL), `read_config_file`
  redaction 3 keys behind the newtype set, absolute hook paths in
  client error text, MCP raw internal error text, the non-RAII
  `LuaCrudInfra` restore, and 5 nested-component M13s (2 HIGH: nested
  tabs blank out, nested groups can't collapse). 4 guard failures
  fixed AS guards (partition fixture + Display channel, redaction sync
  pin, CLI-scan vocabulary + liveness pin — which immediately exposed
  the pre-existing vacuous `query::undelete(` entry and a previously
  invisible `trash restore` write site — and the sink-inventory scope).
  All 21 fixed same-round with regression tests. Also cleared-and-
  recorded: SQL binding everywhere, all triple-stashes judged, email
  funneling, no process sinks, TOTP/signed-URL/MCP-session/queued-bulk
  verdicts CLEAN, no live secret-log sites. **Convergence: 1 of 2
  consecutive all-guarded rounds achieved.**
- 2026-09-05 (9) — **CONVERGENCE ROUND 2** (fresh lens set). 18
  findings — again all instances of existing classes, 0 new. This
  prompted a **criterion correction**: "two consecutive rounds whose
  findings all map to an existing class" is NOT a valid convergence
  bar. With 84 rows almost any real finding matches *some* class, so
  that test can be satisfied while the code is still materially buggy.
  **Real convergence = a round that comes back genuinely quiet** (few
  or no substantive findings, none HIGH). Rounds 1–2 were NOT
  convergence — they were high-yield audit rounds. The "1 of 2
  achieved" line above is retracted.
- 2026-09-05 (10) — **CONVERGENCE ROUND 3** (6 substantive findings,
  down from 21 / 18). The access-control lens came back CLEAN (every
  read/draft/trash/versions gate held). Findings: 1 pool/route infra
  (P2/F10 — non-RAII / duplicated pool-CRUD infra construction), 1
  ref-count HIGH (M4 — localized has-one: the compute walker read
  `col__en`/`col__de` but single-locale write data is bare-keyed, so
  zero refs were counted → delete-protection bypass; the existing test
  only drove the read path, a guard gap), and 4 render findings (2
  HIGH: nested display-conditions inert; list column sortable-drift
  400). All fixed with regression tests. Still 3 HIGH this round →
  **NOT converged; round 4 required.**
- 2026-09-06 (11) — **classifier convergences** (structural, not
  audit rounds): three walker families that each carried a `_ => {}` /
  `_ => Leaf` wildcard were routed through the shared
  `core::walk::field_children` classifier (or its `FieldContext`
  analogue), which was made **exhaustive over `FieldType`** and
  `pub(crate)` — a new `FieldType`/`FieldContext` variant is now a
  compile error at the one classifier instead of silently
  leaf-classified everywhere. Converged: (a) ref-count compute + read
  walkers (`db/query/ref_count`), which fixed the round-3 M4 localized
  has-one miss in the compute path; (b) the `FieldContext`
  display-condition + error-count walkers via new
  `child_field_slices()` / `non_repeating_children_mut()`; (c) the
  field-access denial-collect + data-aware strip walkers
  (`hooks/lifecycle/access/field/walk.rs`). M1 stays GUARDED, now with
  three more walker families provably behind the single classifier.
- 2026-09-06 (12) — **M1 guard upgrade + full walker convergence.**
  Audited every `match …field_type` composite dispatch in the tree (5
  read-only lens agents + hand verification). Routed ~19 hand-rolled
  composite-descent walkers through the shared `field_children`
  classifier (killing their `_ => {}` wildcards; behaviour-preserving),
  spanning join/hydrate (read×4, save, group), read (missing_relations,
  sort, back_references scan), populate (×2), filter (where_clause,
  resolve/blocks, resolve/lookup, validation prefix-roots),
  ref_count/api, validation sub_fields, admin condition-refs, typegen
  sub-types, mcp object-schema, versions (save_draft, restore),
  snapshot, and CLI import. Building the inventory surfaced THREE
  descent sites no lens agent had been pointed at (versions
  save_draft/restore, CLI import) — direct evidence the periodic-audit
  approach leaks. Added the source-scan pin `tests/field_tree_dispatch.rs`
  (every production `match …field_type` is the sanctioned classifier, a
  leaf re-dispatch under it, or a reviewed value-mapper) so a new
  hand-rolled dispatch fails CI. Also fixed a real bug found en route:
  `apply_default_timezone` never descended `blocks`, so a `timezone`
  Date inside a Blocks field never inherited the config default
  (regression test added). M1 PARTIAL→GUARDED. Not a convergence audit
  round — round 4 still pending.
- 2026-09-06 (13) — **CONVERGENCE ROUND 4** (5 fresh lenses:
  tx-failure consistency, locale/i18n, unbounded recursion/limits,
  upload/storage lifecycle, numeric/serialization). 6 actionable
  findings, all personally verified by full-path trace and fixed
  test-first: **3 HIGH** — (1) gRPC ingestion converter recursed
  unbounded on attacker data, a pre-auth stack-overflow DoS (the
  `max_nesting_depth` guard existed on the Lua path but not gRPC —
  S/P guard-parity gap); (2) `update_upload` deleted the live
  published file on a draft-save-with-new-file (L — file-vs-committed
  state); (3) numeric-sort keyset pagination errored on Postgres at
  whole-number boundaries (`AdaptiveInt` missing FLOAT8 — D
  SQLite/PG drift, mirror of the checkbox INT2 fix). **2 MED** —
  restore dropped a localized date's `_tz` companion (M coverage
  gap); a whole Number in an array serialized as `5.0` not `5` (M/D
  read-path divergence). **1 LOW-MED** — `update_upload`
  `.ok().flatten()` swallowed a DB error and leaked old files (F1
  fail-open, delete path was hardened but not update). The systemic
  machinery audited CLEAN: tx commit-ordering, ref-count deltas
  under failure, access control, upload CleanupGuard + backend
  parity, populate depth/cycle detection, signed URLs, the whole
  numeric coerce/has-many-scalar path. **Round 4 did NOT converge**
  (3 HIGH) — the finding rate is falling (R1 21 / R2 18 / R3 6 / R4
  6, but severity concentrated) and every finding was an existing
  class, 0 new classes. Round 5 required. Also noted (not fixed):
  bulk default 0, conn-mode invalidation asymmetry, doubly-nested
  group filter locale, localized-array required sub-fields
  (already-known). Deferred residual: prost/tonic decode of the
  self-referential message is a separate recursion the converter
  guard doesn't cover — message size is bounded (grpc_max_msg) but
  not depth; flagged for a decode-level mitigation decision.
- 2026-09-06 (14) — **CONVERGENCE ROUND 5** (5 fresh lenses:
  auth/session/token, concurrency/TOCTOU, jobs/scheduler,
  query/filter/FTS, cache/events). Auth came back **fully CLEAN**
  (session-version invalidation, JWT alg-pinning, atomic email-keyed
  rate limiting, single-use MFA/TOTP with replay guards, reset tokens,
  OAuth scoping — all fail-closed). 6 actionable findings, all
  personally verified and fixed test-first: **2 HIGH** — (1) ref-count
  TOCTOU: concurrent updates to one doc snapshot outgoing refs unlocked
  → double-applied delta → delete-protection bypass / phantom ref
  (PG-only, L/F; fixed with a `lock_row` FOR-UPDATE before the
  snapshot, no-op on SQLite); (2) keyset pagination NULL-order drift:
  `ORDER BY` lacked `NULLS FIRST/LAST`, PG default opposite SQLite →
  dup/dropped rows (PG-only, D; fixed by pinning the clause). **2 MED**
  — nested-JSON dot-path filter (`->>` vs `#>>`) and nested-JSON Number
  compare (text vs float8) both PG-only (D). **1 MED** — gRPC Subscribe
  op-filter running after burst coalescing dropped a requested event
  (M/L). **1 LOW** — image-job cleanup LIKE over-delete via unescaped
  `_` in a nanoid id (S; fixed + `contains` `\` gap). Plus a
  harmonization: gRPC/admin MFA completion now share one fail-closed
  `reload_authenticated_user`. **The headline: 4 of 6 were PG-only, all
  from SQL builders written to SQLite semantics — the D class (SQLite/PG
  drift) was systematically UNGUARDED because there was NO behavioral
  Postgres testing anywhere** (no CI service, no PG-connecting test;
  the `--features postgres` CI jobs only compile). Seeded the fix: a
  dual-backend harness (`db/pg_test.rs`, env-gated on
  `TEST_DATABASE_URL`) — the four PG bugs were found AND fixed red-green
  against a real Postgres with it. D-class stays PARTIAL (harness
  exists, coverage is still thin — 4 targeted tests, not the full
  suite ported). **Round 5 did NOT converge** (2 HIGH) — but a
  security-critical fresh lens (auth) came back clean, like round 3's
  access model. Round 6 pending; the biggest lever now is broadening PG
  behavioral coverage + adding a CI Postgres service.
- 2026-09-06 (15) — **CONVERGENCE ROUND 6** (4 fresh lenses:
  error-disclosure, input-strictness, migration-drift, hook-loading).
  **6 findings, 0 HIGH** — the severity ceiling fell again (R4 3 HIGH →
  R5 2 HIGH → R6 0 HIGH), all instances of already-founded classes, no
  new class. Fixed test-first: **(1) Sec/F** — a hook `error()` leaked the
  server's ABSOLUTE filesystem path: hook files resolved via `require`
  were named by Lua's stock searcher (the absolute `package.path` entry),
  and that chunk name travels verbatim to the client as a `HookError`. A
  Rust `require` searcher now stamps the config-relative `chunk_name`,
  harmonizing `hooks/` with the `collections/`/`globals/`/`jobs/`/init.lua
  paths that were already relative (installed in both VM builders).
  **(2) Sec/P** — MCP job tools and gRPC `cancel_run` returned unscrubbed
  `Internal`/`Transient` text (raw backend/driver vocabulary); switched to
  `into_anyhow_scrubbed`/`Status::from`, with a source-scan guard pinning
  the whole `src/mcp/tools` tree. **(3) D+M** — the GLOBAL alter path
  lacked the scalar has-many numeric→TEXT reconcile the COLLECTION path
  had, so an upgraded Global with a `has_many` Number/Text field was
  unsavable on Postgres; extracted ONE shared `reconcile_scalar_list_column`
  and wired both paths, proven red-green with a real-PG test. **(4) S** —
  present-but-wrong-typed values on Email / length-constrained / scalar
  has-many fields were coerced not rejected; each now rejects to match
  Number's existing rule. **Round 6 did NOT meet the stop criterion**
  (that needs TWO consecutive quiet rounds) but it is the first 0-HIGH
  round — a quiet-ish round. Round 7 pending; the biggest lever remains
  broadening PG behavioral coverage on the new harness + landing the
  drafted CI Postgres service job.
- 2026-09-08 (22) — **CONVERGENCE ROUND 13** (5 fresh lenses: MCP surface
  end-to-end, gRPC wire codec, drafts/`_status`/versions across surfaces,
  config loading + startup validation + secrets, email/reset/verify/MFA
  delivery). **3 HIGH, ~10 MED, ~15 LOW — NOT quiet, and one HIGH is a NEW
  class (F-class secret-derivation: a key derived from an unset config value).**
  All personally verified + fixed; gates green; UNCOMMITTED.
  - **HIGH (NEW class) — `crap.crypto` encrypted under a publicly known key.**
    With no `[auth] secret` (the default; the server generates and persists one
    for JWTs), the Lua crypto helpers keyed AES from the *raw* empty config
    value = SHA-256(""). FIX (structural): the secret is resolved at CONFIG
    LOAD (`AuthConfig::resolve_secret`), so every consumer — JWT, crypto, TOTP
    sealing, signed URLs — reads one resolved value; the helpers additionally
    refuse an empty key. Retires the "which consumer sees which secret"
    divergence rather than patching one call site.
  - **HIGH — versions lost every other locale's content** (D-class locale-ctx
    footgun, again): snapshots were built from the row as resolved under the
    WRITING locale, so restore wrote that value into the default-locale column
    and NULLed the rest. FIX: snapshots record every locale's decorated column;
    restore prefers those (bare key only as the legacy fallback); the draft
    overlay resolves per READING locale. `tests/versions_localized.rs`.
  - **HIGH — `LoginResponse.user` / `VerifyMfa` shipped unstripped rows**
    (P-class parity): the credential lookups are raw reads, so `hidden` and
    `access.read`-denied fields rode along while `Me` stripped them. One shared
    `prepare_user_document` now serves all three.
  - **MED — restore/unpublish wrote `_status` on `drafts = false` collections**
    (no such column → raw backend error after hooks ran); version rows never
    selected `created_at` (every consumer rendered an empty date); localized
    auth collections broke login and every bearer request (bare column names);
    MCP write tools dropped `null` (no way to clear a field / remove a
    translation); a collection and a global sharing a slug conflated
    `access.mcp` on MCP (cross-kind slug now rejected at load — same-kind
    redefinition stays legal, it is the documented plugin pattern); queued bulk
    over the cap returned INTERNAL not FAILED_PRECONDITION; NaN/Inf on the wire
    silently CLEARED a field; changing an email kept the verified flag;
    `read_config_file` redaction was line-based (dotted keys / inline tables /
    multi-line strings leaked); `restore --include-uploads` extracted a whole
    archive into the config dir (operator-code overwrite) and both destructive
    db commands ran happily under a live server.
  - **LOW** — pool-acquire errors INTERNAL on 8 RPCs (now classified);
    `scheduled_by` UNSPECIFIED for gRPC-queued bulk (enum gained MCP + CLI);
    Subscribe accepted unknown operation names; email templates rendered
    "expires in60minutes"; loose-permission warning covered 3 of 8 secret
    fields; `.jwt_secret` briefly world-readable; `db console` leaked the PG
    password via argv; libpq/`/`-containing password masking gaps; MCP
    `list_job_runs` uncapped; MCP client name unsanitized in audit logs; docs
    drift (job tiers, `access.mcp` evaluation timing).
  - **CLEAN (evidence in the reports):** MCP tool naming/schema-vs-runtime/
    session tracking/error scrubbing/exhaustion caps; the gRPC value mapping,
    present-null contract, pagination, locale validation, enum exhaustiveness
    and codegen parity; config parsing strictness (deny_unknown_fields
    everywhere, numeric-knob pin), env substitution, feature/cross-field
    validation, startup hook-ref completeness, identifier-length checks;
    token entropy/expiry/single-use/constant-time, enumeration resistance,
    MFA pending-token binding, password-hash never leaving the DB.
  Convergence: a new class appeared, so the streak stays 0. Round 14 pending.
- 2026-09-13 (23) — **ROUND 13 FOLLOW-THROUGH** — the four items Round 13
  had recorded as open, all landed, plus the missing self-service surface the
  first of them exposed. No new lens, no new class.
  - **F-class — spendable secrets stored in the clear.** Reset tokens,
    verification tokens and MFA codes sat in their columns as plaintext, so a
    backup, a replica, or a stray query log handed over a live credential.
    Fixed at the DB storage edge, not per flow: one `hash_security_value` /
    `security_value_matches` pair, applied inside `db::query::auth`, so every
    caller stores a SHA-256 digest and every lookup hashes what was presented
    and compares in constant time. Breaking with no migration by decision —
    outstanding links and codes die on upgrade.
  - **F-class — pre-auth password-policy oracle.** The gRPC create/update
    codec validated the password while unpacking the request, ahead of the
    access check, letting an unauthenticated caller read back the configured
    policy. The codec now only extracts; the service write path's existing
    check (which always also ran) is the only one left. Removing it exposed a
    second hole the codec had been masking: the coercion `as_str().unwrap_or("")`
    turned a non-string password into `""`, and the service treats an empty
    password as "no change" — on create that is a passwordless account. The
    codec now rejects a non-string as a wire-shape error (no policy detail in
    it), and `validate_password_policy` takes an explicit `EmptyPassword` mode
    so create rejects an empty value on every surface instead of relying on
    each caller to have checked upstream.
  - **S/P-class — MCP answered protocol errors as tool results.** An unknown
    tool came back `isError: true` rather than `-32602`, and an unknown
    resource URI as `-32603` rather than MCP's `-32002`. Both fixed, and the
    split is now typed: a dedicated `UnknownTool` error is the only thing the
    JSON-RPC layer promotes out of band.
  - **F-class — MCP confirmed which collections exist.** `Tool not available:
    <slug>` for a filtered/`access.mcp`-hidden collection versus `Unknown tool`
    for a name that was never generated let a client enumerate what it was
    being kept from. Both now answer the identical `UnknownTool`, matching
    `describe_collection`. Pinned by a test that asserts the two messages are
    byte-identical.
  - **M-class — no self-service verification resend.** Only an administrator
    could reissue a verification link, from the CLI; with the tokens now
    hashed, every outstanding link dies on upgrade and that gap becomes acute.
    Added `/admin/resend-verification` (linked from the login page when it can
    do anything) and a `ResendVerification` RPC, both over one service
    chokepoint that also serves the sign-up email, so link shape, 24-hour
    lifetime and single-live semantics cannot drift between the two. Shares
    the forgot-password rate-limit budget and its anti-enumeration answer.
  - **M6 — JSON-RPC batching was unimplemented** while the declared protocol
    version requires it. Both transports now dispatch batches through one
    shared `mcp::batch` module, with an empty array and a >100-member array
    refused whole so a small request cannot expand into unbounded work.
  - **D-class, found by the gate run — a documented Lua API did not exist.**
    `crap.validation_error`, the structured way for a hook to reject a write,
    was described in the CHANGELOG and asserted by an e2e test, but nothing
    ever registered it; the test had been failing on "attempt to call a nil
    value". Implemented with one encoder in Lua-land and one decoder in
    `ServiceError::classify`, both anchored on a single constant in
    `core::validate`, so the string channel between the two cannot drift.
  Convergence: no new class, and every item was already on the books from
  Round 13, so this does not count as a quiet round in its own right. The
  streak stays 0; Round 14 pending.
- 2026-09-13 (24) — **CONVERGENCE ROUND 14** (5 fresh lenses: access-rule
  engine, rate limiting, secondary-channel leakage, multi-node topology,
  time/expiry/dates). **35 reported, 34 confirmed — 5 HIGH, 12 MED, 17 LOW —
  NOT a quiet round**, and one finding is a NEW class, so the streak stays 0.
  Most findings sit in GUARDED classes whose guards did not reach them:
  - **F4 (guard failed) — HIGH: field `access.update` rules saw the incoming
    patch as `ctx.document`.** An owner rule passed for a caller who rewrote
    `owner` in the same write. The strip now has create/update entry points;
    update requires the stored document (loaded only when a field configures
    `access.update`) on update, bulk update, global update and the update
    dry-run. Integration tests with a real Lua owner rule.
  - **F18 (NEW) — HIGH: Redis cache clear deleted every rate-limit lockout**
    (`crap:*` covers `crap:rl:*`). Cache keys moved to `{prefix}cache:`;
    overlapping prefixes are refused at startup.
  - **P11 (guard failed) — HIGH: empty `[auth] secret` generated a different
    secret per node.** Refused when a Redis cache/transport/rate-limit backend
    is configured; warned on Postgres. MED: periodic memory-cache clear only
    ran on gRPC nodes — moved to process startup (serve + work).
  - **P7 (guard failed) — HIGH: bulk-update snapshots lacked the locale
    config**; **M4 — HIGH: default-locale tz restore used the bare column**;
    MED: restore kept per-locale columns of a write-denied field.
  - **F7 (guard failed) — eight rate-limit defects:** gRPC reset/verify
    keyspaces (one missing entirely), MFA issuance flood via gRPC, OAuth and
    admin-MFA clears instead of refunds, memory-backend sweep pruning by the
    caller's window, Redis same-instant member collision, resend thresholds
    diverging per surface. Keyspace names are now shared constants and every
    surface derives its limiter with `rescoped`.
  - **F17 (guard failed):** failed/stale email jobs kept rendered links, the
    validation-marker nonce reached `JobRun.error`, `webhook_url` unredacted.
    **F14:** client field name logged raw. **P2:** queued `delete_many` gate
    ignored soft delete; draft-only global reader got published content.
  - **L/S/D (time):** JWT 60 s leeway, refresh past `session_absolute_max_age`,
    DST-gap local times stored as UTC, `utc_now` pinned `.000`, legacy
    space-format timestamps on disk (versioned rewrite), `images purge` cutoff
    in the future, retention purge racing a restore (locked re-check), PG
    schema sync unserialized (advisory lock), event `sequence` per process
    (publisher id added). CLI `-p` visibility → `--password-stdin`.
  - Docs-only by decision: `heartbeat_interval` must match across nodes;
    index drop during rolling deploys.
  - Refuted: day-only+timezone list rendering (Appendix 2).
  Convergence: a new class appeared; streak stays 0. Round 15 pending, and a
  loadtest is still due before the tag.
  - **Post-fix review (5 reviewers over the round's own diff).** One HIGH
    regression came from this round's fix: the restore strip now removed a
    denied localized field's per-locale columns, and restore then wrote NULL
    into every locale of a field the snapshot no longer carried — worse than
    the bug it fixed. Restore now leaves an uncarried field untouched, judges
    field rules against the live row (it used the snapshot, letting a former
    owner pass an owner rule) and drops a denied date's `_tz` companions.
    Also fixed from the review: the MFA issuance throttle handed out a
    challenge whose earlier code had already expired or been consumed (now
    refused explicitly on both surfaces); gRPC MFA logins left the login
    counters charged; a JWT was still accepted during its `exp` second; a
    refresh at the exact session ceiling minted a dead token; version rows on
    tables created by early releases still took the space-format default
    (the insert binds timestamps now); the DST-gap check also ran on
    JSON-stored rows whose dates are never converted; coalesced event bursts
    were ordered by per-publisher `sequence`; the Redis overlap check compared
    URL spelling; stdio MCP skipped the periodic clear while Redis nodes each
    wiped the shared store; the dry-run read its stored row outside its
    transaction. Docs: upgrade item 27 advised a fresh secret that would
    destroy `crap.crypto` data re-encrypted under item 18, and omitted
    rolling-upgrade behavior on a shared Redis. Refuted: an extra per-row read
    in bulk updates (the match set is ids only). Lesson for the program: a
    fix that removes data from a pipeline must be traced to every consumer of
    that data, not only the one the finding named.
  - **Decisions after the review.** Draft saves keep judging field
    `access.update` rules against the published row (frozen). Timezone dates
    nested in JSON rows — blocks rows, groups inside rows, nested array rows —
    were stored as wall-clock digits while top-level and array-row dates were
    UTC; by decision they are now converted on write, shown in their row's
    zone, validated for DST gaps everywhere, and existing rows are migrated
    once (versioned per slug). Mapping that path surfaced a live data bug the
    audit had not: the admin form showed array-row timezone dates as their
    UTC digits and saved them back as local time, shifting the date by its
    offset on every save — the display side of M4 had never been wired for
    rows.
- 2026-09-13 (25) — **CONVERGENCE ROUND 15** (5 fresh lenses: generated
  contracts vs runtime, composite feature matrix, Unicode and text identity,
  endpoint authorization inventory, data portability and lifecycle tools).
  **37 reported, 36 confirmed — 8 HIGH, 18 MED, 10 LOW — NOT a quiet round.**
  No finding needed a new class; most sit in guarded classes whose guards did
  not reach them:
  - **M4 (guard failed) — HIGH ×2: versions mixed localized join rows.**
    Snapshots read array/blocks/relationship rows without a locale and restore
    wrote them back without one, and a non-default-locale draft overwrote the
    default locale's rows. Snapshots now keep per-locale join keys
    (`items__de`), restore writes each locale back under its own locale, and a
    draft writes only its locale's key; a snapshot taken before leaves
    localized rows untouched.
  - **L2 — HIGH: a draft save built on the published row**, so a second draft
    edit — another locale, or any partial update — reverted the earlier ones.
    Drafts now build on the latest draft snapshot.
  - **D1/D5 — generated contracts:** MCP block schemas discriminated on
    `blockType` while the runtime uses `_block_type` (HIGH); array and blocks
    rows had no `id`, so a schema-following update replaced every row; client
    typegen made required fields non-optional though drafts, field read access
    and `select` omit them (HIGH — every read field optional, by decision);
    read types lacked `_status`/`_deleted_at`/`_tz` and any `locale = "all"`
    shape (generated, by decision); `DescribeCollection` hard-coded a global's
    timestamps and drafts; `FieldInfo` lacked polymorphic targets, value lists
    and timezone (additive proto fields); generated Lua hook types named the
    wrong operations.
  - **P9/S9 — HIGH: email identity.** SQLite `LOWER` folds ASCII only, so
    `Ärger@…` could never log in and `Ärger@…`/`ärger@…` registered twice;
    NFC/NFD twins passed `unique`. Email is stored trimmed, lowercased and NFC,
    text NFC (by decision), filters compare the canonical form, and a
    versioned migration rewrites stored addresses, stopping startup on a
    collision instead of choosing one.
  - **S5 — identifiers:** a locale code with capitals broke Postgres filters,
    sorting, cursors and index DDL (generated names with capitals are quoted
    now); index names were not length-checked. **P1:** the in-memory filter's
    `contains` compared case-sensitively and `%` stopped at line breaks.
    **P9:** `like` escaping differed per backend (`ESCAPE '\'` everywhere
    now). **D1:** the browser `maxlength` counted UTF-16 units. **F14:**
    `Content-Disposition` carried raw UTF-8 and bidi overrides.
  - **D8 — HIGH ×2: portability.** Backups lacked the generated auth secret;
    export → import dropped credentials (`--include-credentials`, opt-in by
    decision), trashed documents, `_tz` companions and nested layouts, and
    re-minted array/blocks row ids.
  - **F13 (guard failed):** the upload API and `/uploads` loaded a token's
    user outside the evaluator — an unusable token became anonymous and a
    collection's accepted methods were ignored. **F6:** a custom page gate
    counted a filter table as allowed. **P2:** `/admin/collections` ignored
    `access.admin` that the dashboard applied, and MCP job tools read and
    cancelled bulk runs of collections MCP hides. **F11:** existence probes
    in upload delete and `TriggerJob`.
  - **P5 — lifecycle:** CLI `user delete` bypassed the service (reference
    protection, soft delete, hooks). **M4:** trash purge, CLI and retention,
    read upload rows without a locale. **L5:** `restore` checked only the
    server PID — a shared instance lock now keeps it apart from server, worker
    and stdio MCP. **P9/D2:** `backup` misreported Postgres and silently
    skipped remote uploads.
  - **Found while fixing.** The upload API tests had passed only because of
    the F13 bug: their helper minted tokens with a stale session version that
    the old path quietly downgraded to anonymous. A surface-parity allowlist
    entry went stale with the removed probe. Observation, not changed: the
    evaluator's `is_locked` check on the loaded document can never fire —
    reads don't select `_locked` — so locking takes effect only because
    `lock_user` also bumps the session version.
  Convergence: no new class, but eight HIGHs, so not a quiet round; the streak
  stays 0. Round 16 pending, and a loadtest is still due before the tag.
  - **Post-fix review (6 reviewers over the round's own diff).** 4 HIGH,
    14 MED, about 30 LOW or style. HIGH: `import` applied reference counts per
    document, so a document referencing one later in the file — collections
    import in slug order — failed the import on a fresh database (counts now
    settle once every document is written, in one transaction); localized
    array/blocks/has-many rows exported under the default locale only and
    import refused them (exported per locale now); a second draft save nested
    a group field's per-locale keys into the group, so the draft read showed
    another locale's edit and restore skipped the group's rows; and the
    round's own parked observation was a live HIGH on a sibling path — strategy
    auth read `_locked`/`_verified` off the hook's document, which never
    carries them, so a locked account signed in through a strategy (the lock is
    now read from the row on every credential path). MED: the in-memory
    matcher compared operands as typed (a `not_equals` on an address with
    capitals failed open); `migrate fresh` released the instance lock at once
    (now a `#[must_use]` `InstanceLock`) and `work` took it after writing;
    `trash purge` deleted files before commit; CLI `user delete` had no live
    transports; stored text was never NFC-converted; imported TOTP secrets
    sealed under another secret silently re-enrolled; a credential import
    revived revoked sessions; the sidebar showed pages the route refused;
    `TriggerJob` rejected a malformed payload before its access rule (an
    oracle); failed and stale bulk runs kept their payloads; timezone
    companions read back flat (localized, and in groups); `typegen proto`
    emitted an unsanitized `_tz` ident; and, pre-existing, colliding index
    names silently skipped an index. LOW/style: `like` escape judged before an
    email's trim, catalog index names unquoted on drop, restore wrote localized
    emails as typed, the kept previous secret was overwritten and written
    non-atomically, restore took its lock before validating the config,
    TypeScript names missing from the collision check, the MCP global update
    schema kept `required`, dead optional branches in the generators,
    over-long functions and parameter lists, stale comments, and docs that
    filed two breaking changes under Fixed.
  - **Decisions after the review.** Stored text is migrated to NFC like
    email: the rewrite covers columns, per-locale columns, array rows and
    JSON-stored rows, reruns when a collection's email/text column set changes
    (the meta value fingerprints it), and stops on collisions in unique fields
    and unique indexes. An import refuses TOTP secrets that don't open with
    the target's auth secret. TypeScript group and row types get `…Data` input
    variants beside all-optional read types; Go reads booleans and single
    groups through pointers. A filesystem without file locks stays a hard
    startup failure, documented. Refuted: client-chosen junction ids
    overwriting another document's rows (import uses plain `INSERT`, so a
    taken id is a primary-key error), credentials in exports without the flag,
    lock lifetime in `serve`/`work`/`mcp`. Lesson for the program: an
    observation that a guard can never fire is a finding — trace every caller
    of that guard before parking it.
  Convergence after the review: still not quiet; the streak stays 0.
  - **Re-review of the fix pass** (4 lenses over the fixes): 1 HIGH, 10 MED,
    ~25 LOW — all fixed test-first, gates green, UNCOMMITTED. HIGH: the
    canonical rewrite built per-locale column names from the raw locale code,
    so a hyphenated locale (`pt-BR`, column `title__pt_BR`) stopped startup.
    MED, same root: draft save and read, version restore and import built or
    read per-locale snapshot keys and columns in the raw form too — every site
    now goes through `locale_column`, and the draft read also resolves (and
    drops) per-locale `_tz` keys. MED: the unique-index check skipped trashed
    rows and failed on a localized field (one shared `compound_index_columns`
    for the index and the check); version restore validated and wrote
    snapshots as typed; a custom page without a rule was hidden under
    `default_deny` while its route rendered it; `trash purge` held two pooled
    connections; the TypeScript collision check counted enum names TypeScript
    never emits; MCP update schemas kept nested `required`; generated
    per-locale maps and TypeScript read fields weren't nullable; a localized
    field couldn't be imported with locales off. LOW: import refuses rows per
    locale with locales off and duplicate ids, runs IMMEDIATE, and reports
    password-less accounts only with password login; an empty `.jwt_secret`
    is replaced through a staged file; `migrate fresh` locks before the pool
    opens; `restore` refuses a non-project directory and doesn't keep aside a
    secret its own config load generated; CLI `user delete` reaches Redis
    before the prompt; a strategy's failed account lookup is logged with its
    own reason; one query reads a token's lock and session version; upload
    pool exhaustion answers 503; ordered in-memory operands are canonical; a
    read-denied timezone date takes its `_tz` along; `has_tz_companion`
    replaced ~30 copies; import-rule and function-length cleanups;
    `import_cmd.rs` split; stale doc wording fixed.
  - **Decisions after the re-review.** Per-locale map values and TypeScript
    read fields are nullable in every generated language. A job access rule
    sees `nil` `ctx.data` for a malformed payload — documented, not changed.
    CLI `user delete` builds its live transports before the prompt and fails
    when Redis is unreachable. Lesson for the program: a naming helper
    existing isn't enough — every construction of the same key has to route
    through it, or a variant input (a hyphen) splits the key space; grep for
    hand-built forms of the key when a helper is introduced.
  Convergence after the re-review: still not quiet; the streak stays 0.
  - **Focused review of the re-review fix pass** (3 lenses: locale keys &
    restore; import & CLI lifecycle; auth, typegen & MCP): 2 HIGH, 9 MED, ~12
    LOW — all fixed test-first, UNCOMMITTED. HIGH: a draft save stamped a
    localized timezone date's per-locale value but not its zone, which the
    draft read now resolves per locale (a regression of the previous pass);
    junction `_locale` defaults used the column form of the code (`en_US`)
    while every read filters on `en-US`, hiding existing rows once a field
    became localized (pre-existing). MED: a draft read with no value in the
    reading locale returned the saving locale's; the canonical unique-index
    check read only text values; an unreadable `.jwt_secret` was replaced and
    two starting processes could each generate a secret (both regressions of
    the previous pass — generation now runs under `data/.jwt_secret.lock`);
    `trash empty` failed on localized collections; one-shot CLI commands
    opened the database without the instance lock (every one now holds it
    shared through `open_project`); a denied timezone date leaked its zone on
    the flat, nested and `locale = "all"` strip paths; generated array-row
    types lacked the row `id` updates need; upload bearer auth answered 500
    on pool exhaustion. LOW: restore warns when a configured secret overrides
    the restored one, import report and comment wording, read vs write pool
    for IMMEDIATE transactions, tuple parameter bundles replaced by structs,
    builders/constructors, test setup shared, stale restore docs.
  - **Decisions after the focused review.** CLI commands that open the
    database take `data/crap.lock` shared; relational array-row types carry an
    optional `id` in every generated language (Lua included); the generated
    secret is resolved under an exclusive lock, and only a readable empty file
    is ever replaced. Lesson for the program: a fix pass that restructures code
    (a rewritten lookup, a split file, a new chokepoint) produces its own
    regressions at the seams — three of this review's findings came from the
    previous pass — so a structural fix pass gets a focused review before the
    next round.
  Convergence after the focused review: still not quiet; the streak stays 0.
  - **Second focused review** (3 lenses over the focused-review fixes): 1 HIGH,
    4 MED, ~15 LOW — fixed test-first, UNCOMMITTED. HIGH: a draft read resolved
    a localized field inside a group into a flat key beside the nested value,
    so a reader saw the locale the draft was last saved in and a read-denied
    sub-field could leak (older, extended by the previous pass to nulls and
    zones) — resolution now flattens, resolves and re-nests. MED: global draft
    and published snapshots skipped locale resolution; MCP offered a row `id`
    on rows nested in another row; CLI commands failed on a read-only data
    directory (the lock file was always opened for writing); admin file
    serving and custom routes read a database error during sign-in as an
    anonymous request. LOW: draft join fields fall back on empty rows and read
    `[]`; row-id tests for typegen, proto and Lua; row-id doc wording; Go
    `ID`; the polymorphic junction rebuild keeps the `_locale` default;
    neutral lock messages and `hold_exclusive_instance_lock`; restore detects
    a secret set in `crap.toml`; staged secrets swept under the lock; an empty
    secret file no longer counts as generated; a stale doc line, a duplicate
    log line.
  - **Decisions after the second focused review.** An existing lock file is
    locked through a read-only handle; a database error during sign-in answers
    `503`/`500` on every admin surface. Lesson for the program: a helper that
    rewrites document keys must run on the shape the data is stored in — test
    it with nested groups, not only top-level fields.
  Convergence after the second focused review: still not quiet; the streak
  stays 0.
  - **Third focused review** (3 lenses over the second-focused fixes): 4 HIGH,
    4 MED, 6 LOW — fixed test-first, UNCOMMITTED. HIGH: snapshots read
    per-locale columns as text, so drafts returned localized numbers, checkboxes
    and multi-value fields as strings and restore cleared a localized
    multi-value field (restore also skipped a non-localized one — pre-existing);
    the in-memory constraint matcher looked group sub-field paths up at the top
    level only, so a row rule on `seo__owner` misjudged nested documents; an
    admin request without credentials checked out a connection before finding
    nothing to evaluate, so the previous pass's `503` refused public pages and
    files under load (a regression of the previous pass); the exclusive instance
    lock went through a read-only handle, which NFS and `fcntl`-lock platforms
    refuse (a regression of the previous pass's decision). MED: `locale = "all"`
    draft reads resolved to the default locale; the upload access check
    answered `404` on a database error; the handler error log dropped the chain
    of non-transient errors; restore judged a configured secret from
    `crap.toml`'s text, missing `${VAR:-}` with the variable unset. LOW: empty
    groups kept through draft resolution; the snapshot resolver split, with a
    locale-pair struct; a `resolve_global_doc` parameter struct; empty secret
    files not backed up; a misplaced test doc; inline import paths in the MCP
    schema and config tests.
  - **Decisions after the third focused review.** Corrected: only the shared
    lock opens an existing lock file read-only; `restore` and `migrate fresh`
    open it for writing. Whether the auth secret was generated is recorded on
    the loaded config, not re-derived from `crap.toml`. Lesson for the program:
    a new early failure (a `503` on a checkout) must come after the paths that
    need no resource — check which requests reach the failing step — and a
    decision resting on platform semantics (read-only handles for locks) needs
    each mode's caveats read before it is recommended.
  Convergence after the third focused review: still not quiet; the streak
  stays 0.
  - **Chokepoint pass after the third focused review.** The three focused
    reviews kept finding one cluster: copies of the value ↔ column path
    disagreeing with the published read. Instead of another review, the
    copies were merged (new class P12): one encoding (`column_value`, used by
    create, update, restore and array rows; `stored_document_values` puts a
    draft save's edit in stored form), one decoding (`decode_row`, used by every
    row read, the credential lookups and the snapshot build; its `decode_value`
    also decodes array row columns), and one locale choice (`access_locale` /
    `read_locale`, used by the select, the write column, join hydration,
    filters and their subqueries, hook locale and the draft read). The
    round-trip matrix written first was red on four instances: a
    `locale = "all"` read of a localized multi-value field returned `[]`
    (pre-existing, the grouping ran before the list decode); a draft save
    stored values as sent, so drafts read checkboxes, JSON and timezone dates
    differently from the published read (pre-existing); a multi-value sub-field
    of an array row read back as its stored JSON text (pre-existing — array
    rows decoded their columns by hand); a locale that isn't configured was
    read differently by the select and by join rows, filters and the draft read
    (latent — requests validate the locale).
  - **Copies sweep after the chokepoint pass** (4 search agents, every claim
    verified in code before fixing; each fix landed in a chokepoint, test
    first). Fixed: version history returned stored snapshots, leaking hidden
    localized fields in MCP `list_versions` (snapshots now read as documents
    through `snapshot_read_document` + the read strips); draft-save, restore and
    unpublish reported the wrong document (`hydrate_reported`/`strip_reported`,
    restore re-reads after its writes, and the draft `_status = draft` stamp
    kept on purpose — events route by it); filters, sorts, FTS, ref counts and
    back-references each decided a column's localization on their own
    (`column_is_localized`/`stored_columns`); a snapshot copied plain group
    columns flat (`per_locale_columns`); `select` dropped `_tz`/`_lang`
    companions and the write strip `_lang` (`column_belongs_to`); the MCP schema
    and `make hook` walked wrappers one level; the unique check, import, column
    defaults and number-list validation encoded values their own way
    (`column_value`, `number_element`); polymorphic references and admin number
    tags were parsed by copies (`poly_ref::parse`, `tag_values`); the admin
    checkbox form and list cell accepted fewer spellings than the write stores
    as checked (`parse_truthy`, `json_truthy`), and date-picker values, form strings and default locale
    contexts were rebuilt per site (`date_picker_values`,
    `value_to_form_string`, `LocaleContext::default_for`); label reads
    skipped the read strips and read the default locale; image conversion,
    upload REST writes and the CLI purge bypassed the write reporting
    (`report_conversion`, `.infra()`, `purge_document`); the bulk queue read a
    localized auth collection without a locale; Login/Me skipped read hooks
    (`read_own_document`); values inside JSON-stored rows were stored as sent
    (`nested_value` on the shared `walk_nested_mut`, which now hands its visitor
    the containing object). UNCOMMITTED.
  - **Decisions in the sweep.** Version snapshots read as documents (default
    locale). Nested values are typed; "missing checkbox is unchecked" applies at
    admin form ingress only, since block rows merge with the stored row and a
    write-denied value must survive. Picker and labels read the editor locale.
    Image conversion is a reported system write (event, cache, `updated_at`; no
    hooks or version). Login/Me run the read pipeline without the collection
    read gate. One-time conversions are listed in `migrate::one_time`, removable
    after 0.1.0. Lesson for the program: search for copies by kind (value
    conversion, locale decision, naming/walks, read/write pipeline) — the copies
    a review finds one at a time cluster by kind, and a shared primitive that
    lacks one capability (the walker's sibling access) breeds copies until it
    gains it. UNCOMMITTED. Process rule added to the triage section:
    check for a chokepoint before any concrete fix.
  - **Review of the uncommitted pass** (4 read-only reviewers by area, every
    finding verified in code). 1 HIGH, ~10 MED, ~12 LOW, ~15 NIT; no new class.
    HIGH (introduced by the pass, never released): Login/Me returned the user
    unstripped when `before_read` failed — the new read pipeline made the
    strip fallible and the caller only logged. Decision: fail the request, and
    `read_own_document` clears the fields on error so no caller can leak.
    MED: the scheduled trash purge still hand-rolled the hard delete (P row —
    routed through `purge_document`); the conversion event had its own shape
    (one "reported shape" step); a code
    field's `_lang` companion was created but never stored or read
    (pre-existing — one companion list); column DEFAULTs encoded by type, not
    field; the admin restore-confirm read raw snapshots outside the service.
    Decisions: numbers trim surrounding whitespace everywhere; any non-zero
    number checks a checkbox everywhere (column and rows shared one rule
    only for strings). Lesson: a chokepoint that makes a formerly infallible
    step fallible must fail closed at the chokepoint itself, not rely on each
    caller's error handling. Refuted by its regression test: "undelete reports
    without rows" (`query::find_by_id` already hydrates). Fixed test-first by
    area. UNCOMMITTED.
  - **Architecture review of the two passes** (3 read-only lenses: layering,
    chokepoint completeness, decisions). Layering held except one structural
    inversion (`core::field::companion` importing the suffix constants from
    `db` — moved core-ward, `db` re-exports). Completeness: the pass itself
    left copies of the kind it removes — import carried `_tz` but not `_lang`
    (data loss on export/import, fixed test-first), the draft-snapshot store and
    the admin wrapper-nested code enrichment were `_tz`-only, upload `sizes`
    were folded at 9 sites (now `core::upload::shape_read_document`), the
    read/write hook traits carried the same strip bodies (now one
    `FieldReadStrip` trait), live events hand-rolled the strip pair, two
    `is_truthy` copies had already diverged (server condition vs admin form on
    `{}`), the in-memory filter had its own number/bool readings. Decisions:
    Login/Me hook failure now maps like a Find (was INTERNAL vs
    INVALID_ARGUMENT — same cause, two frozen codes); checkbox input is
    validated on every surface (user decision: strict, `"2"` == `2`);
    version reads take a `locale` (the read-shape snapshot had made non-default
    locales unreachable); populate cache key carries the fallback; replacing an
    upload's file cancels its queued conversions; a conversion finishing after
    a purge removes its orphan file; a nested group of only unchecked boxes now
    saves. Guard added: `tests/chokepoint_copies.rs` — a source scan per
    chokepoint (locale contexts, hidden strip, number parsing, hard delete,
    admin locale, companion spellings, sizes shaping) with reviewed allowlists,
    since none of the new chokepoints had an anti-copy guard. Lesson: a
    chokepoint pass needs its own completeness review — the new primitive's
    call sites are exactly where the next copies are written — and a scan
    guard the day the chokepoint lands, not later. UNCOMMITTED.
- 2026-09-24 (36) — **CONVERGENCE ROUND 26** (budget lifted; live Postgres 16
  for the harness and the example; 5 Opus lenses — authentication paths,
  jobs/scheduler, read access beyond find, uploads, filter-path grammar —
  5 Opus fix batches, 4 post-fix reviews, 4 follow-up batches, 1 split).
  **~50 confirmed — 2 HIGH (both security), ~20 MED, ~28 LOW — NOT quiet; no
  new class.**
  - **F (HIGH, security) — OAuth/external auth callbacks skipped MFA.**
    Password and strategy login passed the collection's MFA gate; the
    callback minted a session directly. User decision: gated by default, with
    a per-collection `mfa_exempt_callbacks` list for IdPs that enforce their
    own 2FA. One `service::auth::mfa_gate` + one admin challenge step now
    serve every session-minting path (frozen-contracts rule added).
  - **F (HIGH, security) — a hidden field was filterable through its other
    spelling.** `is_hidden_path` compared the query path with the denial's
    flat form, so `seo.links.url` reached a hidden `seo__links.url`. Both
    sides are canonicalised (`__` ≡ `.`) before comparing.
  - **F (security, MED):** session refresh signed a JWT from strategy claims
    (claims now carry `TokenUse::Strategy`; the signer refuses them; refresh
    requires a cookie session); bulk update/delete were a filter oracle on
    read-denied fields (checked at scope and at queue time); back-references
    and joins listed children through a field the viewer can't read; the
    in-memory evaluator matched operands SQL refuses (event gating fail-open);
    the upload serve path overwrote the SVG sandbox CSP; the extension check
    read the raw, not the stored, filename; wildcard MIME claims were stored.
  - **L — a job timeout didn't stop the job.** The outer `timeout` wrapped
    `spawn_blocking`; the Lua kept running and its run was re-queued beside
    it. Cooperative `ExecutionDeadline` (VM hook, DB access, pre-COMMIT,
    HTTP/email); the scheduler timer is now a watchdog that never stamps a
    live run; `timeout = 0` refused; finished runs wake the poll loop.
  - **M — Lua-created accounts got no verification** (jobs, routes,
    migrations, `on_init`: no email context); reset/verify token lookups
    broke on a localized auth collection and accepted trashed users.
  - **Filter grammar:** Postgres checkbox inside row JSON (parameter error);
    block types defining a name differently now read per type, with rows of
    undeclaring types reading the name as absent; arrays/blocks/has-many
    inside groups filterable; Join leaves in rows refused; dotted `order_by`
    normalised at the service chokepoint; cursors read the sort value the SQL
    orders by (nested groups, all-locales reads).
  - **NUL characters at any depth** (row JSON, has-many lists, rich text,
    snapshots) made every row-path `::jsonb` cast on Postgres fail — one
    `core::nul` rule at validation and every persist path; the narrow
    top-level guard removed.
  - **Uploads:** drafted replacement files served to draft viewers (lookup by
    exact url column, no cap — a capped LIKE was a denial-of-service); restore
    re-queues only variants it can't adopt (a leak otherwise); the write
    pre-check sees the file's derived columns; the REST pre-check with no
    data (wrongly refused data-aware rules) removed.
  - **Found by running it:** one committed Postgres unit test had asserted a
    superseded SQL form since the dotted-path change — it only runs under
    `--all-features`. A fix's CHANGELOG premise (a localized access row
    filter) was unreachable by design and was corrected.
  - **Splits:** `tests/admin_auth.rs` (2,626 lines → 5 binaries),
    `core/collection/auth.rs`, `document_info`, `uploads/serve.rs`,
    `core/upload/process.rs`, `filter/resolve/path.rs`, scheduler `poll.rs`.
    Gates (2026-09-24): clippy clean in both forms; full suite 8,620 green
    over 120 binaries (default features) + lib 6,825 under `--all-features`;
    Postgres harness 27/27 on a fresh PG16; example seeded, served and
    queried live on PG (checkbox-in-row filters, per-type readings, jobs
    drained); LuaLS clean on the golden and the whole example; all five
    `gen-*` checks, `cargo fmt`, `crap-cms fmt`, biome clean; e2e 341 green
    over 81 binaries (per binary). The field-dispatch guard caught two new
    `field_type` value maps in the filter resolver (reviewed, allowlisted).
    Streak: 0 quiet rounds.
- 2026-09-24 (35) — **CONVERGENCE ROUND 25** (budget lifted; live Postgres 16
  + a real SSE subscriber on PG; 5 Opus lenses — external API surfaces after
  the event/filter rework, locale × the new mechanisms, admin client-side,
  every write outside the service, the Lua API contract — 12 Opus fix
  batches, 5 post-fix reviews). **~60 confirmed — 4 HIGH (2 security), ~20
  MED, ~35 LOW — NOT quiet; no new class.**
  - **Runtime first:** R24's event rework verified end to end on PG — soft
    delete, undelete, delete and empty-trash purge all reached a live admin
    SSE stream.
  - **F (HIGH, security, pre-existing) — a filter/sort oracle on nested
    fields.** `unreadable_query_paths` probed only a path's ROOT with a null
    value, so `access.read` on any sub-field of a group, array row or block
    was never evaluated: `where items.secret like 'a%'` tested a value every
    response strips. Probes now carry the path's real container shape
    (`service::read::query_probe`) through the same strip responses use.
  - **F (security, reference code + a fix-introduced hole):** the example
    access rules authorized updates on the incoming patch (an author could
    claim any post). Mapping JSON null to `nil` in Lua (to close truthy-null
    fail-opens like `if ctx.user.is_admin`) opened the reverse: a constraint
    table with a NULL user field lost that key and widened. User decision:
    nil + `crap.null` (explicit nulls, array elements) + a NULL-read guard
    on `ctx.user` (a table returned after reading a NULL user field is
    Denied); custom-strategy users are reloaded through the token path's
    reader so the guard covers them. mlua's own serializer is now banned in
    `clippy.toml` (`disallowed-methods`) — a lint instead of a text scan.
  - **M/HIGH-ish — Lua CRUD in migrations and `on_init` deleted upload files
    before commit** (no cleanup queue): `HookRunner::run_in_system_tx` gives
    both commit-gated queues; the no-queue fallback now keeps the file.
  - **HIGH (client) — array/blocks reindex never renumbered the relationship
    picker's `field-name`**, so after duplicate/remove/move a pick wrote to the
    wrong row. Also: read-only/unresolvable references were cleared on save
    (now submitted back as `unavailable` items); multi-select values are a
    JSON array (commas in option values); failed multipart parse lost edits;
    richtext/focal-point edits invisible to the dirty guard.
  - **P2 / D — service bypasses:** `db cleanup` ref counts; CLI account
    actions/import without stream teardown or cache clear; `user create` via
    raw `query::create`; the ref-count backfill gate blind to field removal
    (now a registry-wide reference-topology fingerprint).
  - **M — never-validated relationship targets:** a documented TODO; a
    dangling target now crashed boot inside the recompute. Targets (incl.
    polymorphic, join `collection` and `on`) are validated at load.
  - **Locale:** junction filters ignored fallback (listing and filter
    disagreed; negative element ops matched shown values); Full-mode event
    rows were in the writer's locale.
  - **Other:** bad filter paths answered INTERNAL on Count/UpdateMany/
    DeleteMany (typed `invalid_query`); bare-day date filters now cover the
    whole UTC day (created_at/updated_at typed as dates); MCP write schemas
    from the write shape; `parent_id` indexes on array/blocks row tables;
    array rows filter nested JSON like blocks rows; PG session notices off.
  - **Guard maintenance:** two guard false positives fixed (`-> LocaleContext
    {` matched as a literal, in two chokepoints); several allowlists followed
    file splits.
    Gates (2026-09-24): clippy clean in both forms; unit 6,583 + integration
    ~8,430 green over 112 binaries; Postgres harness 26/26 on a fresh PG16;
    LuaLS clean on the golden and the whole example; all five `gen-*` checks,
    `cargo fmt`, `crap-cms fmt`, biome, stylua clean; e2e 341 green over 81
    binaries (per binary).
    Streak: 0 quiet rounds.
- 2026-09-23 (34) — **CONVERGENCE ROUND 24** (budget lifted: a live Postgres 16
  container for the harness and an example-project boot; 2 Opus lenses — admin
  list view + i18n, generated types vs real reads/writes — 11 Opus fix batches,
  4 Opus post-fix reviews). **~45 confirmed — 3 HIGH (1 security), ~15 MED,
  ~25 LOW — NOT quiet; no new class.**
  - **Running the real thing found what reading didn't.** The Postgres harness
    flaked on every fresh database (CI's case, ~2 runs in 3): a test created a
    system table outside the schema-sync advisory lock and raced another test's
    sync (`pg_type_typname_nsp_index`); production was safe. Booting the
    example on Postgres showed its seed broken twice — HTML written into a
    `format = "json"` rich text field (refused since R23) and polymorphic
    references in a pre-alpha.10 shape — because NOTHING ran the example's
    migrations. Guard: `tests/example_project.rs` (sync + seed on SQLite,
    SMTP overridden). The first smoke `serve` also inherited real
    `CRAP_SMTP_*` credentials from the operator's shell (15 rejected attempts,
    none delivered) — process lesson recorded.
  - **P1 — has-many element semantics were missing everywhere.** Scalar
    has-many columns were filtered as whole JSON text (`tags = "a"` never
    matched `["a"]`); the admin merged AND rows on one field into IN. Now
    element-wise on SQLite, Postgres and the in-memory evaluator (one rule for
    scalar lists, has-many relationships and references inside rows; negative
    operators = no element), AND means AND, sort on a list field rejected.
    User decision (no PG16 `IS JSON`): a STORAGE INVARIANT — the schema sync
    rewrites any non-array has-many value once (gated by a per-table
    fingerprint; unconvertible values fail the boot naming the row), and a
    stored single value in a column is one element (comma lists only where
    rows really stored them). Found on the way: has-many references inside
    rows were stored as comma text and read back as an empty selection
    (re-save erased them).
  - **F (SECURITY, HIGH) — live events leaked past row constraints.** The
    subscriber gate judged constraints on the event payload, which is empty in
    Metadata mode and for every delete: has-many `not_in` / `not_equals` and
    `not_exists` constraints matched empty data. User decision: events carry
    an opaque access-check snapshot of the stored row (no getter, redacted
    Debug, destructured away by every client encoder — compile-forced guard),
    deletes read the row before removal; Full-mode data is now stripped per
    subscriber from the stored row (it was stripped by the WRITER's access);
    trash purges publish their events and clear the cache; the in-memory
    evaluator judges `id`/timestamps (`constraint_row`, one chokepoint); a
    trashed row's hard delete is gated by the trash view. Redis carries full
    rows → `rediss://` TLS enabled (user decision), with one process-wide
    rustls provider installed at `open_client` (both ring and aws-lc-rs are in
    the tree, so `builder()` would panic).
  - **D1 — generated types described neither the write nor the read shape.**
    One write shape and one read shape in the typegen IR: TS `…Data` ids-only
    references, required single relationships required, upload-derived keys
    gone, `password?` on auth input, Join not writable, hidden fields gone from
    read types, `collection` tag declared, Lua `crap.input.*` /
    `crap.read_hook.*`, Go system names pre-seeded; goldens now cover every
    field kind (incl. `typegen proto`).
  - **List view / i18n:** operator labels resolved to the alphabetically-first
    locale (now the viewer's UI locale, one task-local re-entered on blocking
    threads); `_status` in an OR was lifted to a global AND; hidden/unreadable
    fields offered for sort → whole-collection 403; search dropped trash and
    filters; untranslated password-policy key; user-settings keys collided with
    collection slugs (namespaced, then the `collections` slug itself — review
    catch). Guard: `tests/admin_translation_keys.rs` (template/Rust keys, en/de
    parity).
  - **Also:** CLI config validation via one `load_config` + explicit
    `--skip-config-validation` on recovery commands (R23 follow-up); D4 — the
    shared guard scanner blanked every `not(...)`-gated item (122 files of
    `not(tarpaulin_include)` code were invisible to every guard); `crap-cms
    trash restore` bypassed the service; undelete of a live document ran
    `before_change`; the retention purge is batched and fails per row.
    Post-fix reviews: 4, each finding real issues next to the fixes (1 HIGH
    among them — the event gate), none a regression of a fix's own intent.
    Gates (2026-09-23): clippy clean in both forms; unit + integration ~8,236
    green over 110 binaries; e2e 325 green over 80 binaries (per binary); Postgres
    harness 22/22 on a fresh PG16; all five `gen-*` checks, `cargo fmt`,
    `crap-cms fmt --check`, `biome ci` and mdbook clean.
    Follow-up (user report — editor warnings in the new Lua golden): a real
    LuaLS `--check` found 93 problems in the generated types. Digit-leading
    slugs/field names (`2fa`), Lua keywords and LuaLS scope words (`private`)
    were emitted raw; accessor stubs on `crap.collections.<slug>` triggered
    `inject-field` for EVERY collection; an inline `fun(...)` return list in
    `types/crap.lua` swallowed its table's next member. One Lua naming
    chokepoint (`idents::lua_index` / `lua_field_key`), class-name collision
    check, factories on the class-bound local. Guards: a hermetic LuaLS-grammar
    test and `lua_types_pass_a_luals_check` (runs real LuaLS when found —
    `CRAP_LUALS`, PATH or Mason). The whole example project now checks clean,
    which exposed an example bug: the `reading_time` hook read its own empty
    field as HTML (always "1 min read"; raised on the JSON `content`).
    Streak: 0 quiet rounds.
- 2026-09-23 (33) — **CONVERGENCE ROUND 23** (budget lifted mid-round:
  5 lenses — newest code (jobs catalogue parity, custom-route CSRF, example
  job-manager plugin) and the validate dry-run on Sonnet, the CLI surface on
  Sonnet, the individual validation checks and the admin extension surface on
  Opus — 4 Opus fix batches, 2 Opus post-fix reviewers).
  **~45 confirmed — 5 HIGH, ~20 MED, ~20 LOW — NOT quiet; no new class.**
  The newest-code lens came back fully CLEAN; the two never-lensed surfaces
  (CLI, admin extensions) and the per-check validation sweep carried the round.
  - **P2 — the validate dry-run was a hand copy of write admission.** It never
    adopted the pending draft, never applied the locale lock and skipped the
    upload-metadata strip, so `validate` answered "valid" for publishes,
    translations and upload creates the real write rejects. Fixed at a new
    chokepoint, `service::write::admission` (`admit_create_input`,
    `admit_update_input`, `admit_global_update_input`, `PendingDraft::
    publishing_snapshot`), called by both the writes and `Validate`/
    `ValidateGlobal`; pinned by a `WRITE_ADMISSION` entry in
    `tests/chokepoint_copies.rs`.
  - **P2 — Lua CRUD writes never ran rich text node-attribute validation**:
    only `RunnerWriteHooks` injected the registry into the validation context.
    `validate_write_fields` now takes the registry as a required argument and
    `LuaWriteHooks` carries a non-optional `&Registry` (19 call sites),
    so a write path cannot compile without it.
  - **P1 — "the ids in this value" had four decoders.** `row_bounds` read a
    string as a JSON array only while the admin form sends comma lists and the
    writer split on commas (JSON-array strings became ids like `["a"`):
    `min_rows = 1` made a has-many relationship unsaveable, `max_rows` was not
    enforced. One decoder, `core::reference_items`, for validation, the writer
    and ref-counting.
  - **Known footgun recurring (find without a locale context)**: `user list`,
    `bench` and `trash` read, and `user create`/`bench create` wrote, with
    `None`, naming bare columns on localized collections. One CLI helper,
    `commands::cli_find`, plus `LocaleContext::default_for` on the writes.
  - **D — free-form provenance.** `scheduled_by` was a `&str` at nine insert
    sites; `queue_bulk` hardcoded `"api"` for both gRPC and MCP callers and
    `"system"` had no proto value. `core::job::ScheduledBy` (closed set,
    legacy `api` read as `grpc`), `JOB_SCHEDULED_BY_SYSTEM = 6`.
  - **D2 — CLI flags undocumented three times over** (`jobs trigger --priority`,
    `jobs cancel --id`, `images retry --priority`, plus a whole
    `templates layout` subcommand). New guard `tests/docs_cli_flags.rs`: every
    long flag of the live clap tree must appear in its own section of
    `flags.md`.
  - MED cluster, validation: JSON rich text sent as an object skipped node
    checks and unparseable JSON failed open; node-attr `before_validate` never
    ran in groups or rows; Lua `min`/`max` beyond i32 and invalid counts were
    dropped silently (now load errors); date bounds compared the raw text,
    not the stored UTC day, and broke `monthOnly`; scalar `has_many`
    `min_rows` skipped absent values.
  - MED cluster, admin/CLI: template errors answered 200 on the Lua-render
    paths; the `crap.pages` `section` option was documented everywhere and
    rendered nowhere (now grouped in the sidebar); the `make node` scaffold
    shipped the documented stored-XSS pattern; `make hook -t field` could not
    produce its own "any field" form; the empty-auth-collection warning was
    dead; `jobs cancel`/`purge` skipped the schema sync; a
    tutorial scenario (02) did not work as written.
  - **Post-fix review (2 Opus reviewers) found no regression from the fixes but
    surfaced two MEDs next to them**: an admin *draft* save under a non-default
    locale was refused by the locale lock (the admin form strip was
    publish-only; pre-existing, now visible because validate mirrors the
    write) — `strip_locale_locked_form_fields` applies to every admin save; and
    making template errors a real 500 left htmx requests with no feedback
    (htmx does not swap 4xx/5xx) — every admin error response now carries an
    `X-Crap-Toast` through one helper, which also stopped non-ASCII toasts from
    being dropped at the header conversion.
    Test fallout: two guards did their job on the round's own changes — the
    `field_tree_dispatch` inventory row for `checks/required.rs` (its field-type
    match became predicates) and the upgrade-guide parity gate (two new
    Breaking bullets needed guide items); plus one obsolete unit test that
    pinned the old last-wins `template_data` registration.
    Correction recorded: the CLI lens (and the fix report after it) claimed
    `typegen`, `db backup/restore/console` and `logs` ran on an unvalidated
    config because they skipped `apply()`. False — `CrapConfig::load` itself
    validates; `apply()` adds only the nesting-limit install. The real gap was
    the reverse: an upgrade-invalidated config blocked `backup` too. Follow-up
    (user decision): one validated load path for every command
    (`commands::load_config`, pinned by `CONFIG_LOAD` in
    `tests/chokepoint_copies.rs`) plus an explicit `--skip-config-validation`
    on the offline recovery commands only, via `CrapConfig::load_unvalidated`.
  - **D4 — the shared guard scanner was blind to `not(…)` gates.**
    `tests/common/mod.rs` judged an item test-only by evaluating its `cfg` with
    every non-`test` atom *on*, so `#[cfg(not(tarpaulin_include))]` — most CLI
    entry points and `main.rs` — and `not(feature = "x")` items were blanked
    as test code, hiding them from every guard built on `production_code`
    (chokepoints, dispatch inventory, sink escaping, MCP scrubbing, wiring,
    surface parity). Now satisfiability with `test` off (`can_hold`/`can_fail`
    per sub-predicate), pinned by
    `production_code_keeps_items_gated_on_a_negated_atom`. The first thing the
    un-blinded scan caught: `jobs trigger` / `cancel` / `purge` wrote on a
    READ-pool connection (now `pool.write()`).
    Gates (2026-09-23): clippy clean in both forms; unit + integration ~8,025
    green over 108 binaries; e2e 323 green (80 binaries, per binary); all five
    `gen-*` checks, `cargo fmt --check`, `crap-cms fmt --check`, `biome ci`
    and the mdbook build clean. Postgres harness NOT run.
    Cost: ~3.3M agent tokens (5 lenses ~1.0M, 4 fix batches ~1.35M, 2
    reviewers ~0.4M, docs lookups ~0.13M). Streak: 0 quiet rounds.
- 2026-09-21 (32) — **CONVERGENCE ROUND 22** (budget mode, Opus orchestrator:
  3 Sonnet lenses — export/import fidelity, generated descriptions & typegen,
  admin rendering layer — 2 Opus fix batches, 1 reviewer).
  **~9 confirmed — 4 HIGH, 3 MED, 2 LOW — NOT quiet; no new class.**
  - **The round's shape: three separate instances of one thing — a truth that
    lives in a chokepoint and an outward description that never learned it.**
    Every read of an upload collection folds the per-size columns
    (`{size}_url`, `_width`, `_height`, `{size}_{format}_url`) into a nested
    `sizes` object, but the client type generators, the Lua type definitions
    and the Rust proto decoder all walked the stored field list, so every
    generated document type declared four-plus columns per size that no read
    returns. Fixed at a new chokepoint, `core::upload::read_shape_fields`,
    with the column names themselves hoisted to `CollectionUpload::
    size_columns()` so the schema injection, the system/derived name sets and
    the read shape can no longer disagree. Adjacent find: a user field named
    `sizes` was silently clobbered on every read — now a reserved name. (D1.)
  - **A JSON rich text field was described as a string** by the generators and
    the MCP tool schema, and the Rust proto decoder dropped it. One predicate,
    `FieldDefinition::parses_json()`, now answers for all three. The decoder
    needed more than the predicate: the JSON kinds fell into a fallback that
    always returned `None`, so the fix alone would have traded one data loss
    for another — the generated client gained real `StructValue`/`ListValue`
    conversion, which also restores `json` fields, empty groups and blocks.
  - **`admin.dev_mode`'s per-request template reload was inert.** Every
    template was registered with `register_template_string`, and
    handlebars-rust only re-reads templates registered from a path, so an
    edited overlay did nothing until restart while six doc locations promised
    live editing. Config-dir overlays now register by path. (D10 — an upstream
    option relied on without checking what enables it; D3 in effect.)
  - **`admin.readonly` never cascaded into a container.** A child's readonly
    was its own flag or the locale lock; the parent's was not consulted, so a
    read-only Group/Array/Blocks rendered fully editable sub-fields — and the
    array/blocks row controls (move, duplicate, remove, drag) were gated by
    *nothing*, so even a locale-locked array could be reordered and emptied.
    The codebase's own pin at `builder/single.rs` documents that this exact
    shape was fixed for Checkbox/Select and never extended to containers.
    (M2.) Two helpers, not one: only `admin.readonly` cascades
    (`cascaded_readonly`), because cascading the rendered flag would fold in
    the locale lock and break the rule that a *localized* field inside a
    non-localized group stays editable in a non-default locale.
  - MED: the relationship/upload "create new" link was gated on the locale
    lock alone, so a read-only field still offered a raw `<a href>` into the
    create form; export is an operator tool that reads the database directly
    (no `access` rules, no read hooks) and the CLI reference did not say so.
    LOWs: `strip_option` stopped at the first layer; wire-parity spellings
    needed `WireKind::Int32` + `grpc_optional` to be expressible at all.
  - Deliberate non-fix, recorded: `force_hard_delete` is a plain `bool` on the
    wire while every sibling flag is `optional bool`. The wire model now
    *describes* that rather than hiding it; flipping it is source-breaking for
    generated clients. Follow-ups left open: the generated *input* types
    declare server-derived upload keys (a pre-existing overclaim — the real
    fix is a write shape mirroring the read shape, which needs a per-field
    read-only flag in the client IR and regenerated goldens), and the
    generated Rust proto file emits unconditional helpers that warn as dead
    code in a consuming crate.
    Post-fix review of this round's diff (1 Sonnet reviewer): **no HIGH, no
    MED, no regression** — the second round in the program with a clean
    post-fix pass. It traced every `ancestor_readonly` construction site, the
    read/write boundary of `read_shape_fields`, the generated decoder against
    `proto/content.proto` field by field, and the `../readonly` depth at each
    template call site. Test fallout was three pinned assertions that the
    intended changes invalidated: two on the generated proto import line, one
    on the upgrade-guide parity gate demanding entries for the two new
    Breaking bullets.
    Gates (2026-09-21): clippy clean in both forms; unit 6,119 + integration
    ~1,800 green; e2e 322 green (80 binaries, per binary); doc tests, all five
    `gen-*` checks, `fmt --check`, `crap-cms fmt --check` and the mdbook build
    clean. Postgres harness NOT run (no `TEST_DATABASE_URL`).
    Cost: ~1.5M agent tokens. Streak: 0 quiet rounds.
- 2026-09-21 (31) — **CONVERGENCE ROUND 21** (budget mode, Opus orchestrator:
  3 Sonnet lenses — concurrent writers/races, storage backends & upload
  serving, live events & transports — 2 Opus fix batches, 1 reviewer).
  **~10 confirmed — 4 HIGH, 2 MED, 4 LOW — NOT quiet; no new class.**
  - **Structural framing that made the race lens pay: every pool write opens
    `transaction_immediate`, which is `BEGIN IMMEDIATE` on SQLite (full
    serialisation) but a plain `BEGIN` on Postgres (MVCC).** Every race found
    is therefore Postgres-only, and every fix is "take the existing
    `lock_row` earlier" rather than a new mechanism: the publish path read
    the pending draft *before* any lock (a concurrent draft save was buried
    silently), the update path skipped its UPDATE — and with it the row lock
    — when a write had no scalar SET clauses, leaving an Array/Blocks
    diff-write unlocked, and the upload file-key snapshot was read unlocked
    (a replaced file leaked).
  - **The remote serve path had drifted from the local one**: local goes
    through `ServeFile` (streamed, Range, ETag, conditional GET); S3 and
    custom buffered whole objects and ignored `Range`, while the docs
    claimed parity. One header path for both now, plus a ranged read on the
    trait (default = get-and-slice, real ranged GET on S3).
  - **F-class — the Redis event channels were hardcoded and unvalidated**
    while the cache and rate limiter both carry configurable, overlap-checked
    prefixes; pub/sub ignores the selected DB, so two deployments on one
    Redis cross-delivered full document payloads.
  - Lesson: a lens that first establishes *what already serialises* (and on
    which backend) turns an open-ended race hunt into a short list — state
    the invariant before enumerating the interleavings.
    Post-fix review of this round's diff (1 Sonnet reviewer): 2 MED, 3 LOW,
    no HIGH and no regression. MED 1 was the price of the race fix, not a
    mistake: the row lock now spans the before-write hook pipeline, so a hook
    that writes a second document can deadlock with a concurrent write taking
    those rows in the other order — kept (Postgres cannot release a row lock
    mid-transaction), documented at the lock, and SQLSTATE class 40
    (serialization failure / deadlock detected) added to the transient
    classifier so the aborted side answers 503 and clients retry instead of
    seeing a 500. MED 2: an S3-compatible provider that ignores `Range` and
    answers 200 was served whole — the slice is taken locally now. LOWs: the
    416-before-304 ordering documented as deliberate (evaluating freshness
    would cost the round trip the range avoids), a byte/character wording fix,
    and redundant re-locking left as the harmless no-op it is. Test fallout
    was two pre-existing load-sensitive fixtures (a 1-second pool timeout
    under a full parallel run) and the new `[live] channel_prefix` key, which
    the config-doc and scaffold-template pins demanded.
    Gates (2026-09-21): clippy clean in both forms; unit 6,076 + integration
    ~1,790 + macros/xtask green; e2e 322 green (per binary); all five `gen-*`
    checks and `fmt --check` clean. Postgres harness NOT run (no
    `TEST_DATABASE_URL`) — note that every race fixed this round is
    Postgres-only, so the harness is the only place they could be proven end
    to end; the tests added pin the lock ORDERING instead (`CountingConn`).
    Cost: ~1.52M agent tokens. Streak: 0 quiet rounds.
- 2026-09-20 (30) — **CONVERGENCE ROUND 20** (budget mode: 3 Sonnet lenses —
  definition-change schema sync, auth state machine, job-queue semantics —
  one Opus fix batch, one reviewer). **PENDING TOTALS — see the post-fix
  line. ~15 confirmed: 6 HIGH, 5 MED, 2 LOW, no new class.**
  - **P — bulk update bypassed the email-change→unverify rule** the single
    update has, and the single update used the raw unverify (no session
    retirement) instead of the hardened service fn. One shared step now.
  - **Definition changes on a table with data** were the under-lensed area:
    `unique` added later got no index (validation still enforced it — a
    race, not a hole); a changed `default_value` never reached an existing
    column (the DB default was load-bearing) — defaults are app-side now; a
    changed field `type` only warned while SQLite drifted the read shape and
    Postgres failed every write — fails the boot now; `soft_delete` switched
    off made every trashed row visible — fails the boot now; removed
    collections/globals left their tables (auth hashes included) invisible
    to every tool — `db cleanup` reports them; junction/global orphan-column
    warnings brought to parity; relationship retargets warned.
  - **L-class (dead parameter) — `work --queues` and `--no-cron` never
    reached the scheduler**: parsed, logged, forwarded to the detached
    child, and dropped; the multi-server docs built a topology on them. Two
    HIGHs, one root cause; a `SchedulerParams` field each now, with the
    claim query filtering by queue.
  - Lesson: "what happens on the NEXT boot after a definition change" is a
    lens family of its own (one per change kind); the sync had guards for
    the changes it was designed around (locale, soft-delete on) and none for
    the rest. Appendix 3 now lists the change kinds verified CLEAN.
    Post-fix review of this round's diff (1 Sonnet reviewer): 1 MED, 4 LOW,
    no HIGH and no regression — the highest-risk change (type-mismatch boot
    failure) was checked exhaustively for false positives across both
    backends' type spellings and came back clean. The MED was a reach gap
    again: the type check landed on collections only while the docs claimed
    it for globals — shared helper now. Also fixed: a reordered polymorphic
    target list no longer reads as a retarget. Two style LOWs taken;
    `is_junction_shape`'s heuristic accepted (explicit `--drop-tables -y`).
    Gates (2026-09-20): clippy clean in both forms; unit ~6,020 + integration
    ~2,190 + macros/xtask green; e2e 322 green (run per binary — a
    multi-binary e2e invocation stalls on this tree, cause not chased); all
    five `gen-*` checks and `fmt --check` clean. Postgres harness NOT run.
    Cost: ~1.45M agent tokens (3 lenses 0.64M, 2 fixes 0.59M, review 0.21M)
    — under half of R19. Streak: 0 quiet rounds.
- 2026-09-20 (29) — **CONVERGENCE ROUND 19** (5 lenses on Sonnet with explicit
  file lists and the Appendix 3 skip list: gRPC/REST API surface, hook
  execution model, MCP surface, read path (populate/filters/search/cache),
  versions × locale × soft-delete interplay). **~20 confirmed — 3 HIGH,
  6 MED, ~8 LOW — NOT a quiet round, but the lowest count since R13; no
  new class.** The API and MCP lenses came back without a HIGH.
  - **M8 — `live.filter` never validated at boot**: the one HookRef site the
    startup inventory missed; a typo'd filter dropped every live event with
    a warning. Field-level `FieldAccess` was read positionally, so a new key
    would have escaped the same way — destructured now, and the inventory is
    pinned against a source scan of every `HookRef` field.
  - **L-class — batch populate lost its cycle guard across has-one/has-many
    recursion**: the recursion re-entered the public entry that seeds a fresh
    `visited` set, so a mutual has-one expanded to the full depth on list
    reads while `FindByID` stopped correctly.
  - **L8 — VM-pool exhaustion classified `Internal`** while the identical
    DB-pool timeout is `Transient`: an untyped `anyhow!` string. Typed now.
  - **S1 — MCP dropped a `Join`-named key silently** (its "known field" set
    came from a walker that classifies Join as a leaf); one writable-field
    predicate now.
  - **M-class — the `required_locales` completeness gate judged the LIVE
    row's other locales**, which R17's publish-adopts-the-whole-draft (and
    version restore, all along) overwrite after validation: a draft that
    cleared a required translation published cleanly. The gate now judges the
    snapshot the write will land. A mechanism (validation) not extended for a
    new instance (the write-back) — the R17 fix's own post-fix review missed
    it because the reviewer's lens was file lifecycle, not validation.
  - P2 sibling: `undelete` ran no lifecycle hooks while `unpublish` runs the
    full set; hooked now.
  - D1 instances: `mcp_reserved` hand list missing `trash`; `rpcs.md` depth
    default and FTS5 wording; `publish_event` rustdoc. L4: config-file write
    and stdio line cap.
  - Lessons: (1) the cheap-model lens with a file list and the skip
    appendix cost ~280–380k tokens each (vs 300–560k on Fable) and found
    the same class of bugs; (2) a "fresh entry point" that seeds state is a
    recursion hazard wherever a private `_with_state` variant exists — the
    public entry must not be reachable from inside.
    Post-fix review of this round's diff (1 Sonnet reviewer, ~40 files):
    2 MED, no HIGH — the first round whose blind-written fixes survived
    review without a regression. The MEDs were both *reach* gaps of the
    round's own fix: the new HookRef pin scanned `src/core` only, so a
    custom page's `access` gate (`src/admin/custom_pages.rs`) stayed
    unvalidated; and `undelete`, now hooked, lacked the `hooks = false`
    opt-out every sibling has. Both fixed by hand. First-run test fallout
    was small: one fixture that had relied on an unvalidated live-filter ref,
    and the reserved-args pin that demanded hand prose for every argument —
    resolved by taking each argument's description from the wire field
    itself (the hand map is now an optional override), which also removes
    the last reason for that list to drift.
    Gates (2026-09-20): clippy clean in both forms (`--all-features` and
    default features); unit ~5,980 + integration ~1,790 + macros/xtask green;
    e2e 322 green; all five `gen-*` checks and `fmt --check` clean. Postgres
    harness NOT run (no `TEST_DATABASE_URL`). Streak: 0 quiet rounds.
- 2026-09-19 (28) — **CONVERGENCE ROUND 18** (5 fresh lenses: per-field-type
  round-trip fidelity across surfaces, failure atomicity & resource cleanup,
  the Lua API contract, the admin UI server side, configuration/operations/
  upgrade). **~40 confirmed — 3 HIGH, 12 MED, ~25 LOW — NOT a quiet round;
  no new class.** Two lenses (atomicity, ops) came back with no HIGH and
  long CLEAN lists; the round-trip and Lua lenses did not.
  - **F9/D4 — admin logout never revoked the session.** `/admin/logout` sat
    on the base router, the auth middleware is layered only on the protected
    sub-router, so the handler's optional principal was always absent and
    the documented `_session_version` bump never ran; a captured JWT stayed
    valid until `exp`. The ledger's own F9 anchor named a structural test
    that did not exist — the guard row was a doc comment. Fixed + the real
    scan added.
  - **M6/F16 — a coroutine escaped the instruction limit.** mlua's
    per-thread hook uninstalls itself on a thread it has no callback for,
    and a new coroutine inherits the parent's hook pointer; `coroutine` sat
    on the reviewed-safe allowlist. Global hook now.
  - **M2/D1 — checkbox and `json` read shapes depended on nesting** (column
    `1` vs nested `true`; string vs parsed vs as-sent) while every generated
    contract promised one shape. Unified at the one decode chokepoint;
    client-visible, guide items 50/51.
  - **L12 — two blocking-on-async / pool-discipline sites survived R17's
    guard**: the scoped Lua tx held its write-pool slot across effects, and
    the scheduler's heartbeat arm wrote through a READ-pool connection in
    autocommit — the R17 guard keyed on `transaction_immediate` only.
  - **S4/S2/S3 — validators with a type hole**: Date accepted any non-string
    silently; text fields only rejected wrong types when a length bound
    existed; an empty Lua table (`{}` → `Object`) could not clear a has-many
    list; `list_runs` `.ok()`'d wrong-typed options; validator/live-filter
    returns of unexpected types were interpreted in opposite directions.
  - **P2/L17 — siblings**: `trash purge` without the confirmation gate its
    twins have; `page_with_toast` without the partial-render decision
    `render_page` got; the admin lock action run unconditionally after the
    document commit; `_status` sortable on collections that have no column.
  - Lessons: (1) a dependency's *threading model* (Lua coroutines vs mlua's
    per-thread hook) is a D10 instance the sandbox allowlist review cannot
    see — audit what a "safe" global can spawn; (2) a guard anchored on a
    test NAME must be pinned by existence (D4 now covers "anchor test
    missing"); (3) a wire-shape contract (typegen, MCP schema, docs) needs a
    pin against the actual decode at every nesting depth, not the top level.
    Post-fix review of this round's diff (2 Sonnet lenses over ~130 changed
    files): 2 HIGH, 1 MED — all fixed, plus the ~25 test expectations the
    boolean/JSON read-shape change and the seconds-preserving date renderer
    invalidated. The HIGHs were both blind-edit regressions of the round's
    own fixes: the new snapshot decoder (`read/decode.rs`) decoded only flat
    top-level keys, so a group's nested values and every blocks row in an
    old-form version snapshot stayed `1`/text — the round's own test for
    that case would have caught it on the first run; and the admin lock fix
    moved the account action BEFORE the document write, so a save that then
    failed (unique clash, hook error) had already locked or unlocked the
    account — mirror image of the bug it replaced. Fixed by splitting the
    account-action chokepoint into its access half (run before the write)
    and the mutation (run after it lands). The MED: two CLI doc notes
    inserted mid-table. The new clap-parity pin for the MCP `cli_reference`
    tool found the hand-curated copy 34 items behind (7 `make` subcommands,
    4 `user`, 3 `templates`, `jobs cancel`, ~19 flags) — the tool is now
    generated from the clap tree, killing that D1 instance rather than
    refilling the copy. Lesson (same as R17): a blind-written fix that adds
    a test cannot be trusted until the test has RUN once; the coordinator's
    first full run is part of the fix, not of the gate.
    Gates (2026-09-19): clippy `--all-targets --all-features -D warnings`
    clean; unit ~5,930 + integration ~1,780 + macros/xtask + doctests green
    (every failure of the first run was a pinned expectation of the old read
    shape, retargeted); e2e 322 green; all five `gen-*` checks and `fmt
    --check` clean. Postgres harness NOT run (no `TEST_DATABASE_URL`).
    `upload.rs` tests split into `upload/tests/{support,lifecycle,publish}`.
    Streak: 0 quiet rounds.
- 2026-09-16 (27) — **CONVERGENCE ROUND 17** (5 fresh lenses: the new
  draft/publish/file state machine, dependency error modes and defaults,
  guard-the-guards / test quality, Postgres-only behaviour since R11, the
  non-CRUD permission matrix). **~35 confirmed — 6 HIGH, ~18 MED, ~10 LOW —
  NOT a quiet round; no new class.** Density is well below R16 and two lenses
  (permissions, dependencies outside the DB crates) came back mostly clean.
  - **D10/F1 — dependency defaults (the R16 S3 pattern, generalized):** the
    deadpool pool had no timeouts or runtime, so `pool.get()` blocked a tokio
    worker forever on exhaustion; `recycle` returned `Ok` unconditionally, so a
    PG restart left dead connections in the pool for good; the PG statement
    cache was never invalidated, so a concurrent `ALTER TABLE` (another node's
    schema sync) turned every cached statement into a permanent 500; constraint
    and transient errors were classified by message text only (locale-dependent
    on PG; an FK violation reported as `ALREADY_EXISTS`); the `cron` crate
    numbers Sunday as 1 and nothing translated — the documented "Mondays"
    example fired on Sunday and the standard Sunday spelling never ran; every
    `image/*` upload went through the image decoder, which has no SVG decoder,
    so the SVG sanitising path was unreachable; `crap.http` honoured proxy env
    vars around its SSRF pin; the email renderer ran non-strict over operator
    templates. Fixed at the crate seams with pins.
  - **P2/L7 — the R16 lifecycle code:** globals never got the publish-adopts-
    draft step; `update_many` adopted the drafted file but never settled it
    (orphaned files, dropped conversions); unpublish snapshotted the live row
    and pruned without releasing files; restore never pruned; a one-locale
    publish dropped the draft's other locales; a file-bearing publish still
    adopted the draft's derived columns. Fixed as ONE shared "finish a write"
    step and ONE "create version and prune (and release files)" step.
  - **D4 — the guards themselves:** the S3 and Redis test modules have never
    run in CI (no job enables the feature AND runs tests; the CI pin only
    checked a string); the chokepoint scan truncated a file at the first
    `#[cfg(test)]` attribute, hiding ~4,500 production lines (the whole
    `PgConnection` impl) from every copy pattern; five surface-parity forbidden
    calls named functions nobody calls that way; several guards had no
    inventory floor or matched comments. Fixed with positive controls.
  - **P9/P10 — Postgres:** nested dot-path filters a hard error
    (`jsonb_array_elements_text(text)`); the new read-expression chokepoint
    inherited quoting-only-capitals, so a field named `user` compared the
    session user; the cron claim wasn't atomic outside SQLite; the soft-delete
    rebuild copied columns unquoted and dropped a referenced table.
  - **F6/P2 — permissions:** a custom route's `access` rule returning a filter
    table counted as allow-all (the pages twin was fixed in R15); global draft
    events were routed to a view the gate declared absent; MCP `list_jobs`
    ignored job `access` while gRPC filtered. The CRUD, restore, unlock, upload
    serve and MCP trust-model rows verified CLEAN.
  - Lessons: (1) a dependency's *domain convention* (cron weekday numbering,
    `image`'s decoder set vs `image/*`, reqwest's ambient proxy) is a D10
    instance even when nothing was "misconfigured" — audit conventions, not
    only options; (2) a feature-gated test module needs a CI row that enables
    the feature AND runs tests, pinned structurally; (3) a source-scanning
    guard must pin how much of each file it actually scanned.
    Post-fix review of this round's diff (3 lenses over the ~120 changed
    files): 2 HIGH, 6 MED, ~12 LOW — all fixed. The two HIGHs were blind-edit
    regressions of the round's own fixes: the Postgres soft-delete transition
    bound its table name as `$1::regclass`, which the driver cannot bind a
    string to (every PG transition would have failed at boot — caught only by
    reading, since the PG harness needs a live server); and a non-default-
    locale publish now made the drafted file live through the snapshot
    write-back while skipping the settle half (conversions never queued, the
    old file's jobs never cancelled). The MEDs: the statement cache's
    "promote on commit" map was built on a false premise (a rollback discards
    portals, not statements) and its in-transaction retry could only report
    25P02, hiding the cause; `PoolError::Backend` classified a rejected
    password as transient; the snapshot strip judged localized fields with
    `ctx.locale = nil`; a stripped write-denied checkbox was written as `0`
    by the full row update (pre-existing, on the single update path too — the
    strip now puts the stored value back); the chokepoint-copy scan and four
    sibling guards now share `tests/common::production_code`, which blanks
    each test-gated ITEM (not the rest of the file) and evaluates `cfg`
    predicates instead of token-matching `test`. Two pins of the round were
    vacuous (`apply` installed a value equal to the fallback; the `.no_proxy`
    scan matched a commented-out call) and were made falsifiable. Lesson:
    an agent that cannot run the backend it edits (PG) needs a reader who
    traces the driver's bind rules — the harness test it writes proves
    nothing until someone runs it.
    Gates after the post-fix fixes (2026-09-18): clippy `--all-targets
    --all-features -D warnings` clean; unit 5,838 + integration 1,772 +
    macros/xtask + doctests green; e2e 322 green; `gen-lua-types`,
    `gen-proto`, `gen-wire-doc`, `gen-doc-tables --check` and `fmt --check`
    clean. The Postgres harness tests were NOT run (no `TEST_DATABASE_URL`).
    Four files the round grew past the cap were split (`collection/
    soft_delete.rs`, `postgres/stmt_cache.rs`, `pending_draft/file.rs`,
    `pg_test/*`). Streak: 0 quiet rounds.
- 2026-09-15 (26) — **CONVERGENCE ROUND 16** (5 fresh lenses: admin form
  round-trip matrix, stored-file lifecycle across storage backends, process
  death/restart/shutdown, locale configuration as a variable, alpha.9→alpha.10
  upgrade path + guide completeness). **~45 confirmed — 7 HIGH, ~22 MED, ~16
  LOW — NOT a quiet round; no new class.** Every HIGH sits in a GUARDED row
  whose guard didn't reach it:
  - **P2 (guard failed) — HIGH ×2 + L4 HIGH: the admin edit form
    re-implemented the upload write lifecycle** instead of calling the
    service's `update_upload`, missing Round 4's rules: a draft save with a
    new file deleted the file the PUBLISHED row referenced (unrecoverable
    404s), a replace never cancelled the old file's conversions, and the
    cleanup guard lived in the async handler across a `spawn_blocking` await
    (a dropped request committed the row and deleted its files). Fix: one
    service entry for every upload write (`service::upload`), deletion keyed
    on "the row stopped referencing the key" and queued post-commit,
    conversions cancelled/enqueued inside the write tx, guard never leaving
    the blocking body; anti-copy guard in `tests/chokepoint_copies.rs`. The
    routing guard pins service *op bodies*; the file lifecycle sat outside them.
  - **F1 — HIGH: S3 `put` never checked the HTTP status** (rust-s3 built
    without `fail-on-err`): a 403/503 committed a row pointing at an object
    never stored; `delete` orphaned, `get` served the error XML as image bytes
    (cached a year on public collections), `exists` could not see a 404. One
    status-check chokepoint for every verb. Same batch: atomic local writes,
    custom storage refuses instead of a silent local placeholder, restore
    skips non-local uploads like backup, restore never leaves the DB absent, a
    conversion of a replaced file discards its output.
  - **P2 — HIGH: the validation-error re-render dropped `_locale`**, so the
    corrected save of a German edit wrote the English columns and overwrote
    shared fields (create too). **P6/D8 — HIGH: `admin.hidden` checkboxes and
    lists clobbered on every save** — the form rendered non-hidden fields, the
    absent-value normalizers walked all. One predicate now decides both.
    Plus list-cell copies, code-in-group-in-row `_lang`, `readonly` inert on
    checkbox/select/radio, comma in a tag, error state, unlisted select value.
  - **D9 — HIGH (data loss on first boot, found by the migration trace, not a
    lens):** alpha.9 stored a has-many scalar list inside a nested row as
    comma text; the typed-values conversion read JSON only and stored `[]`.
    `coerce_has_many_scalar` reads both forms; conversion version bumped.
    Refuted on the way: the same hazard for has-many relationships (alpha.9
    stored `id1,id2`, which `parse_id_list` reads).
  - **P12 (guard failed) — MED-HIGH: `fallback` applied by the SELECT only**;
    filter, sort and keyset used the bare locale column and the cursor took
    the fallback value → rows missed, sorted as NULL, skipped/repeated across
    pages. One read expression (`ReadLocale::column_expr`) for all four, and
    `count` agrees with `find`. Also: localization flip strands the bare
    column (values carried at column creation), ref-count gate blind to locale
    changes (fingerprinted), silent `default_locale` change (warn), `pt-BR`/
    `pt_BR` collision, restore clearing a later-added locale, `all` on writes.
  - **L4/L5/L6 — shutdown:** `serve` force-exited without draining jobs (a
    planned restart terminally staled a queued bulk run); gRPC had no drain
    deadline and `Subscribe` ignored the token; `work --stop` killed at 10 s;
    cron windows reset to boot time; verification and reset tokens minted
    outside their email's transaction; Redis cache never expired at the
    default; `/ready` lied during recovery; scheduler write txs on read-pool
    connections. Drain deadline derived from queue timeouts, one drain helper
    for both servers, token+job in one tx (one chokepoint for reset used by
    admin and gRPC), stored fire time as the cron window.
  - **D1 (pair never registered) — ~30 Breaking/behaviour bullets had no
    upgrade-guide entry** (5 HIGH: `ctx.id` rename, empty access constraint
    denies, `access.unlock`, auth `email` type+unique boot failure, OAuth
    callback fail-closed), four guide claims contradicted the code, no
    backup/rollback item. Guide items 38–49 + "Before you upgrade"; guard
    `tests/upgrade_guide_parity.rs` pins every Breaking lead-in to the guide.
    Data side CLEAN except gate coarseness: `legacy_timestamps` gated once per
    DB (per table now), `canonical_text` unpaged (shared paged scanner),
    `nested_values` gate unfingerprinted, slug-keyed gates shared by a
    collection and a global of one slug (per table now).
  - Lessons: (1) a surface that re-implements a *lifecycle* (not just an op)
    is invisible to the routing guard — the anti-copy scan must name the
    lifecycle's primitives (`process_upload`, `CleanupGuard`,
    `delete_upload_files`); (2) a dependency's error mode (`fail-on-err` off)
    is a fail-open predicate class F1 instance and must be pinned where the
    crate is wired, not per call; (3) the upgrade guide ↔ CHANGELOG pair is a
    D1 pair like any other and needs its parity pin; (4) reading the previous
    release's *storage form* (via `git show <tag>:`) before writing a
    conversion is part of writing it. Fixed test-first by area; almost no
    test had a separate red run (agents cannot compile; the coordinator's
    build was the first run). The first compile of the whole round was
    error-free; clippy then found seven lints and the suite nine failing tests,
    all in the round's own tests or doc pins (a test scenario that assumed a
    create in a non-default locale, which the app refuses by design, was
    rewritten as an update). Gates green: fmt, clippy `--all-targets
    --all-features -D warnings`, suite 7327/7327, e2e 321/321, every gen-*
    check. Convergence: streak stays 0 (7 HIGH); Round 17 pending.
  - **Post-fix review of the round's own diff** (3 read-only lenses over
    `git diff`: storage/durability, locale/migration/filter, admin/docs):
    THREE regressions introduced by the round's fixes — (1) the
    localization-flip value carry ran `WHERE to IS NULL` against a column
    created with the field's DEFAULT (every checkbox), so it never fired and
    marking a checkbox localized would have read false everywhere (fixed:
    guard on the source, plus a stored per-table `locale_shape` record so a
    second flip carries again); (2) the "what the admin form renders"
    predicate assumed hidden fields are filtered at every depth, but the
    builder filtered only at the root, so unchecking a hidden nested checkbox
    stopped persisting (fixed: filter at every depth — which also exposed that
    a display condition on a field nested alone in a group never fired); (3)
    the reference diff for file deletion counted a queued-format derivative as
    kept because its column is only overwritten later (fixed: pending
    conversion columns are excluded). Also: `locale = "all"` still reached the
    admin upload write; `exists`/`not_exists` semantics under fallback were
    unrecorded; `serve --stop` still killed at 10 s while the docs said it
    drained; retired options now block every save unless unchanged; `db
    cleanup` ignored globals and stale-locale rows; `backup` reported success
    on a missing `tar`. Decisions (user): publishing = latest draft + request
    on every surface (drafted files become live at publish; conversions at
    publish); upload files are deleted only when no live row, draft or version
    snapshot references them (reference checks, no stored counter; pruning
    releases files; purge takes all); the duplicate locale-picker template keys
    are removed after verifying nothing shipped read them. Lesson (again, R14's):
    a post-fix review of the round's own diff is part of the round — three of
    the round's fixes carried regressions that no gate caught, because the
    tests written alongside them pinned the wrong premise.
    Gates after the post-fix pass: fmt, clippy `--all-targets --all-features
    -D warnings`, suite 7181/7181, e2e 322/322 (incl. the new nested-condition
    test), every gen-* check. Two older tests pinned superseded rules and were
    updated (restore now leaves an uncarried locale untouched; the trash purge
    no longer needs a file-primitive exemption).
- 2026-09-07 (21) — **CONVERGENCE ROUND 12** (5 fresh lenses: globals-vs-
  collections parity, relationships/populate/back-refs/ref-count, hook
  semantics & Lua-from-hook contracts, client-side JS/templates/htmx,
  search/filter/sort ingress + custom routes). **2 HIGH, ~12 MED, ~10 LOW —
  NOT quiet, no new class**; every finding an instance of an existing row
  (D/M locale-ctx footgun, P chokepoint, F fail-closed/oracle, S strictness,
  M12–M14 client family). Populate/access/CSP/escaping/custom-routes/
  cursor-injection all verified CLEAN. All fixed test-first (RED shown for
  both HIGHs), gates green, UNCOMMITTED.
  **HIGH — SQLite FTS blanked on every write to a localized collection**
  (D-class locale-ctx footgun, M-class mechanism): the per-doc sync read
  `title__en` keys from a locale-aliased re-read → every column indexed
  empty until restart. FIX (structural): `fts_upsert` is row-backed —
  takes an id, reads the indexed columns itself using the SAME
  `get_fts_columns` set the rebuild uses, on both backends (also retires
  PG's "index every string column" drift, F2). **HIGH — undelete failed on
  every localized soft-delete collection** (locale-ctx footgun again;
  `find_by_id(.., None)`) → `ctx.default_locale_ctx()`. MED: hidden /
  read-denied fields were a filter/sort/search oracle (F-class) → ONE
  service chokepoint `reject_unreadable_query_fields` (find/count/search)
  + FTS default-set exclusion + docs; `after_read` had CRUD on Lua-driven
  reads and fail-open committed its writes → `AfterReadScope` marker
  refused at the CRUD entry points; bare pool-mode CRUD lacked the tx scope
  (`crap.tx.on_commit` from a hook rolled the op back; files-before-commit;
  phantom event on commit failure) → ONE `run_scoped_tx` behind
  `with_lua_db` and `crap.transaction`; globals silently dropped a shared
  field under a non-default locale (P-class parity; same guard collections
  have) → `reject_locale_locked_fields` generalized to fields + admin
  globals strip via the shared helper + persist-level check catching
  hook-injected fields (H4); PG lock_row missing on global update / bulk
  update / restore (L-class race, sibling of R5's fix); relationship
  values stored as text when not an id (S-class) → shape check; backfill
  aborted boot on a dangling ref → skip+warn; self-reference blocked its
  own delete → not counted at the ONE ref reader; Lua traceback in hook
  errors (F17 secondary-channel leak) → stripped at classify; filter value
  type mismatch → 400 not PG 500, LIKE on numeric casts, cursor type check
  (S/L). Client (M12–M14): dirty-form cleared by cancelled/unrelated htmx
  requests, radio groups never re-evaluated client conditions, duplicate-
  row cloned stale selections, day-only dates rendered as local timestamps,
  nested label watchers, duplicate panel ids. Diagnosability: unresolvable
  display condition now warns; hook-ref resolver names both attempts.
  **Feature gaps noted, not bugs:** no `UnpublishGlobal` RPC/MCP tool; no
  populate `depth` on `get_global`; global version ops admin-only; export
  omits globals (documented). Two lessons: (1) an e2e `..` destructure of
  `BrowserTestCtx` drops `app` and with it the temp config dir → lazily
  `require`d hooks vanish (bind `app: _app`, as `browser: _browser`);
  (2) `.ok()?` on a hook-resolve path hid a failure for a full debugging
  round — every fail-open must log. Round 13 pending; still need TWO
  consecutive genuinely-quiet rounds.
- 2026-09-07 (20) — **CONVERGENCE ROUND 11** (5 fresh lenses: uploads/media,
  live-updates/SSE, scheduler/jobs, migration/DDL/dialect, admin-render with
  fresh eyes on the new array row-identity code). **1 HIGH, 4 MED, 3 LOW, 1
  doc — NOT quiet, no new class.** Row-identity change reviewed CLEAN
  (duplicate/forged id, JS reindex, XSS, lifecycle). Fixed: **HIGH — publishing
  a translation was rejected**: the admin form submits locale-locked shared
  fields read-only under a non-default locale (the hidden row-id input made an
  all-disabled shared array submit a key too); the service rejects shared fields
  on a non-default write (guards gRPC/Lua/MCP from silent default-locale
  overwrite); save_draft stripped them, publish didn't → admin publish now strips
  like save_draft (`strip_locale_locked_for_publish`), service guard kept for
  programmatic surfaces. **MED — `delete_upload_files` deleted user `*_url`
  fields** (suffix heuristic + hand-maintained `image_url` carve-out instead of
  the authoritative `system_field_names`; a forged user `source_url` → cross-
  document file deletion) → `upload_file_keys` resolves only server-derived url
  columns; `FileCleanupQueue` carries pre-resolved keys. **MED — backfill
  global legacy flag short-circuited per-collection detection** (a collection
  added later never backfilled → ref-count under-count / delete bypass) →
  always run per-slug detection. **MED — `identifier_check` skipped version
  index names** (~33+ char slug truncates on PG → unique index silently
  skipped → duplicate version rows) → validated at load. **MED — redis
  invalidation pump dropped its own `Lagged`** (try_send on the same full
  queue → lost revocation fail-open) → evict-on-overflow for invalidation,
  events best-effort; frozen-contract line extended. **LOW** `execute_ddl`
  `" INTEGER"→" BIGINT"` rewrote string literals → quote-aware
  `pg_widen_integer`; auto_purge measured from created_at → COALESCE(completed_at,
  created_at). **FALSE POSITIVE reverted:** ALTER-adds-timestamp-DEFAULT (SQLite
  forbids non-constant defaults on added columns; writes bind timestamps) —
  recorded in Appendix 2. **DOC** jobs.md called per-queue caps "soft" — they are
  advisory-lock exact. Round 12 pending; HIGH resets the quiet streak.
- 2026-09-07 (19) — **CONVERGENCE ROUND 10** (5 fresh lenses: write-path data
  integrity, error-status classification, admin field-context, DB-dialect/import,
  CLI outcome-reporting). **1 HIGH, several MED/LOW — NOT quiet, no new class.**
  Fixed: **HIGH — SQLite import upsert data loss**: `INSERT OR REPLACE`
  deleted+reinserted the row, zeroing unlisted system columns → re-importing a
  referenced doc reset `_ref_count` to 0, defeating delete protection (PG's `ON
  CONFLICT` was already safe) → SQLite `ON CONFLICT … DO UPDATE`; import writes
  present-null explicitly vs preserving absent, and carries `_status` (M2/D8).
  **MED — all-locales read leaked denied locales** (field-read strip evaluated
  `access.read` once at the default locale, keeping/dropping the whole
  `{locale:value}` map) → per-locale; frozen-contract line added. **MED — import
  skipped `fts_upsert`** → imported docs unsearchable (P5) → re-read + index like
  the service path. **MED — 8 gRPC job/auth handlers skipped `reclassify`** and
  the TOTP flow's 3 `pool.get()` sites used `Internal` not `classify` (500 not
  503) → fixed. **LOW** relationship inline-create label blank
  (`collection_singular_name` never supplied) → populated in
  `enrich_relationship`, crate-level `admin::test_state` builder promoted; admin
  `NotFound` → 500 instead of 404 → arm added; `restore --include-uploads`
  reported success on tar failure (L17) → fails the command. **Array/blocks
  row-identity (A-1)** designed in `array-row-identity.md` (since IMPLEMENTED —
  see priority queue). **CLEAN, added to Appendix 2:** sort/filter never
  silently falls back; `from_locale_string(None)` re-verified. Round 11 pending.
- 2026-09-07 (18) — **CONVERGENCE ROUND 9** (5 fresh lenses: concurrency/
  races, cache correctness, query-builder correctness, field-type correctness,
  Lua API surface). **6 findings — 1 HIGH, 3 MED, 2 LOW — NOT quiet, but no new
  class**, and it RESOLVED a long-standing noted-not-fixed item. Auth-token
  lens equivalent (Lua API) came back essentially CLEAN (only a doc nit); the
  query builder verified correct on both backends except one keyset case;
  concurrency verified guarded except one multi-node-PG cap; cache keys verified
  leak-free (per-user access applied post-fetch, override fetches never cache).
  Fixed: **(1) HIGH — keyset pagination dropped NULL-sort-value rows** on
  DESC-forward/ASC-backward pages (three-valued `col < ?` excluded NULLs that
  sort to the tail); incomplete-fix sibling of the R5 NULLS-order work. `col IS
  NULL` added on the `<` branch + end-to-end test. **(2) MED — per-slug/queue
  job caps overshoot N× on multi-node Postgres** (SKIP LOCKED picks disjoint
  rows, READ COMMITTED count misses peers' uncommitted claims): serialized the
  count+claim with a transaction-scoped advisory lock when a cap is configured
  (new `advisory_xact_lock`, PG mutual-exclusion test) + corrected the
  over-claiming docstring. **(3) MED — conn-mode cache invalidation asymmetry
  (the Round-4 noted-not-fixed item, now RESOLVED):** Lua job/route/transaction
  writes cleared the populate cache pre-commit → stale-repopulation window;
  deferred to post-commit via a `cache_dirty` flag mirroring the file-cleanup
  deferral. **(4) MED — Checkbox `default_value = true` silently stored false**
  (parser/DDL/backfill honored it, write path forced 0): fixed across 3 surfaces
  (write-leaf honors the default for a genuinely-absent checkbox; admin form
  normalizes an unchecked box to explicit 0; new-item form renders the default
  as checked) per user's "make it work" choice. **(5) LOW — `crap.storage.register`
  rustdoc showed a `url` handler the validator rejects** (doc/dead-test drift)
  → corrected. **LOW noted (not fixed): a list `default_value` on a has-many
  Select/Radio is silently discarded** (S1-shaped; analogous to the checkbox,
  rarer — candidate for the same treatment or a load-time rejection). R8's
  deferred pre-auth body-limit coupling (D-1) remains deferred. Round 10 pending;
  the keyset HIGH resets the quiet streak, but "no new class + a tracked item
  retired" continues the stabilization.
- 2026-09-06 (17) — **CONVERGENCE ROUND 8** (5 fresh lenses: user-writable
  server-managed state, bulk operations, auth tokens/cookies/CSRF/reset,
  resource-exhaustion/DoS, migration/schema-evolution). **8 findings — 1 HIGH,
  3 MED, 4 LOW — NOT quiet, BUT no new class** (the HIGH is R7's upload class
  with an incomplete guard, not novel). Auth-token lens came back fully CLEAN
  (JWT alg-pinning, token_use discriminator, per-request revocation, cookie
  flags, double-submit CSRF, single-use reset tokens, TOTP replay guard — all
  wired + tested); bulk ops verified CLEAN except the HIGH (atomicity, per-item
  access, whole-collection-filter reject, counts, queued re-auth all hold); DoS
  lens verified the guarded paths (populate visited-set is graph-bounded not
  N^depth, pagination/depth clamps, image-bomb-before-decode, connection caps,
  gRPC msg size) all hold. Fixed: **(1) HIGH — `update_many` skipped the
  upload-column strip** (A + B found it independently): the bulk per-doc body was
  a THIRD write path the R7 two-site fix missed, so a forged `url` bypassed the
  serve gate across a whole match-set. Fixed STRUCTURALLY — nest+strip fused into
  one `canonicalize_write_input` that all persisting paths call, + a
  `write_paths_canonicalize_before_persist` guard test that fails the build if a
  `persist_*` caller skips it (converts the R7→R8 recurrence into a red test).
  **(2) MED, D — soft-delete rebuild missed a unique field nested in a wrapper**
  (`seo__slug`): stale inline UNIQUE survived the upgrade, blocking re-insert
  after soft-delete; the trigger now walks flattened specs. Same rebuild dropped
  orphan-column data → now re-adds orphan columns before copy. **(3) LOW, F —
  ref-count backfill swallowed a SELECT error** while still stamping the gate →
  now propagates (re-runs next boot). **(4) LOW, F — image bomb check fell
  through to decode when header dims unreadable** → fail-closed. **(5) LOW —
  back-ref list unbounded** → per-query cap (delete block uses O(1) _ref_count).
  **DEFERRED: MED — pre-auth admin routes inherit the upload-sized body limit**
  (raising `upload.max_file_size` inflates the login/reset body cap): a genuine
  coupling, but axum's outermost `DefaultBodyLimit` wins over per-route limits
  and the codebase's only working override (merge outside the global layer)
  strips the nonce-CSP/CSRF auth pages need — too risky for a MED; recorded for a
  focused follow-up (separately-layered auth sub-router with CSP/CSRF preserved).
  D-3 (bulk no-cap default) is by-design (frozen opt-in valve). Round 9 pending;
  the R8 HIGH resets the quiet-round streak, but "no new class" is a convergence
  signal — the class ledger is stabilizing.
- 2026-09-06 (16) — **CONVERGENCE ROUND 7** (5 fresh lenses:
  output-escaping, serialization round-trip, soft-delete/versions/restore,
  live-updates/SSE, storage/uploads/signed-URLs). **8 findings — 1 HIGH,
  4 MED, 3 LOW — NOT a quiet round**, and the HIGH is a genuinely NEW
  class, so convergence is not reached. Output-escaping came back fully
  CLEAN (triple-mustache inventory all escaped, richtext `is_safe_url`,
  SQL identifier validation, no open-redirect/CRLF); signed-URL HMAC,
  `validate_key` on all backends, and image-bomb guards verified solid.
  Fixed test-first: **(1) HIGH, NEW class — upload serve-gate bypass via
  forgeable `url`:** the injected upload columns (`url`/`*_url`/filename/
  dims) were hidden but WRITABLE, and a no-file update wrote them verbatim;
  the serve gate authorizes by matching the request against the stored
  `url`, so a caller with write access could point their own readable doc's
  `url` at a victim's file path (same collection) and read the bytes
  through the gate — and `delete_upload_files` would delete the referenced
  file (the MED E-2, same root). Fixed by stripping the server-derived
  columns (`CollectionUpload::derived_field_names` = system minus focal) at
  the ONE write chokepoint (`create_/update_document_in_conn`) for all
  untrusted surfaces; only the file-processing handlers set
  `trusted_upload_metadata` and may write them. New class GUARDED (chokepoint
  strip + cross-surface unit test + end-to-end upload-update test). **(2)
  MED, F12/F9/P2 — admin SSE failed OPEN on a lagged/closed revocation bus**
  while the gRPC `Subscribe` twin failed closed; SSE now fails closed too.
  **(3) MED, L18/D8 — clear-to-null lost across a before-hook:** Lua nil
  drops the key, so a gRPC `field = null` clear vanished when a returning
  hook rebuilt `ctx.data`; present-null now preserved (mirrors the field-hook
  `was_present` rule). **(4) MED+LOW+LOW, P2 — version restore validated
  inconsistently with the write path:** no locale-ctx/`required_locales`
  (published restore could skip localized completeness), no draft flag
  (draft restore rejected at publish strictness), and no live-target guard
  (restore onto a trashed row silently rewrote it) — all three fixed by
  harmonizing restore's `ValidationCtx` + a `NotFound` guard. **(5) LOW,
  D8 latent — empty `[]`→`{}` across a Lua round-trip:** no reachable
  corruption today (every write edge treats them alike); documented, not
  fixed. Round 8 pending; the streak of quiet rounds is broken by the HIGH,
  so at least two consecutive quiet rounds are still required.
