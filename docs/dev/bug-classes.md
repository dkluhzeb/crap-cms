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
| F14 | Untrusted value interpolated into an interpreter/protocol sink without the sink's escaper | `tests/sink_escaping.rs`: the reviewed sink→escaper inventory (10 sinks: HTML text/attr, JSON-in-markup, SQL idents, email CRLF, Lua source, fs paths ×2, DOM `h()`) with per-anchor liveness pins + CRLF/NUL behavior pin + positive control | GUARDED |
| F15 | Untrusted content interpreted as markup/code (39 `innerHTML` writes, HTML-payload uploads served as text/html, SVG entity expansion) | `h()` DOM builder (one annotated parse site left), MIME/extension cross-check + SVG `<!DOCTYPE>`/`<!ENTITY>` rejection at upload, nonce CSP without `unsafe-inline` | GUARDED |
| F16 | Sandbox capability denylist incomplete — removing A and B but not sibling C (`load` after `loadfile`; **`io.popen` after `os.execute`** — found live by building this guard) | `sandbox_globals_match_reviewed_allowlist` pins the complete surviving global + `os`/`io`/`string` capability sets; per-capability regression tests; sandbox contract recorded in frozen-contracts.md | GUARDED |
| F17 | Sensitive/internal detail escapes via a secondary channel — error bodies, `Debug`, logs, serialization, timing | redacting newtypes (`JwtSecret`, `S3SecretKey`, `SmtpPassword`, `McpApiKey`, + new `RedisUrl`, `WebhookHeaders` — the partition test found all three missing ones on its first run), sentinel partition test `tests/secret_redaction.rs` over Debug AND Serialize, scrubbed responders, constant-time compares | GUARDED |

## P — Surface parity & chokepoints

| ID | Class | Guard | Status |
|----|-------|-------|--------|
| P1 | Per-surface copies of one operation/grammar drift — incl. a second execution engine (`matches_constraints` in-memory evaluator) and duplicate parsers of one syntax | wire model + `wire_parity.rs` + one `decode_where_map`; the in-memory engine has no equivalence pin against SQL | GUARDED |
| P2 | Sibling missing a fix its twins got — sibling axes: API surface, entity kind, delete-path set, browser-side restatement of server rules | `wire_parity` (fields), `surface_behavior_parity` Phase 2 (gRPC ↔ Lua ↔ **MCP through the real JSON-RPC dispatch**: totals, filters, validation, uniqueness); admin covered by browser e2e + the routing guard pinning it to the same op bodies; entity-kind parity rests on shared DDL/column-spec chokepoints | GUARDED |
| P3 | Capability/policy gate lives in a per-surface codec instead of the service op | op bodies own gates; `surface_parity.rs` routing guard blocks reaching past the service layer | GUARDED |
| P4 | Limit/depth/offset clamp missing on one surface | `apply_pagination_limits`, `clamp_depth`, `floor_optional_limit`, `PaginationCtx::resolve_limit`; frozen read-surface invariant | GUARDED |
| P5 | A path bypasses service invariants — CLI raw SQL, an admin handler calling a deep helper, or re-admitted stored data skipping validation | `cli_commands_write_only_through_reviewed_paths` (surface_parity): write primitives in `src/commands` confined to a reviewed, staleness-checked allowlist documenting the invariants each site hand-maintains; `WriteHooks::validate_fields` on restore. Working the guard found + fixed two live instances: CLI `user create` skipped `ref_count::after_create`+`fts_upsert`, `user delete` skipped `fts_delete` | GUARDED |
| P6 | Literal/predicate/selector re-spelled at N sites drifts — incl. template partials, JS selector/attr lists (`__INDEX__` set), cookie regexes, i18n/theme literals bypassing `t()`/CSS vars | named consts + per-literal pins, shared partials (`partials/field.hbs`), `static/components/util/*`; discovery is manual | PARTIAL |
| P7 | Context/param bundle rebuilt by hand, silently dropping a field | `inherit_write_infra`, `ServiceContextBuilder::infra`; inputs carry only per-call data | GUARDED |
| P8 | One concept spelled differently per surface (op names, casing, result keys) | `FilterOp::op_name`/`scalar_from_name`, snake_case decision, wire model option keys | GUARDED |
| P9 | Backend/platform-specific assumption breaks the sibling target (SQLite-isms on PG, Unix-isms on Windows) | `DbConnection` trait (`ddl_type`, `quote_ident`, `now_expr`, `greatest_expr`, `supports_fts()`); CI runs `sqlite+postgres` and `postgres-only` build+clippy+suite jobs; live-server PG smoke and Windows remain manual | PARTIAL |
| P10 | Create, alter, and table-rebuild paths provision differently (rebuild dropped FK/PK constraints) | `collect_system_columns` chokepoint; rebuild must preserve constraints — no pin | PARTIAL |
| P11 | Process-local state assumed cluster-global | redis backends + cron dedup + `SKIP LOCKED`; `deployment/multi-server.md` is the operator checklist, now pinned by `multi_server_doc_covers_every_node_local_subsystem` (8 subsystems incl. the newly documented per-node MCP session labels); new node-local state = add mechanism + doc row + pin entry | GUARDED |

## M — Mechanism coverage

| ID | Class | Guard | Status |
|----|-------|-------|--------|
| M1 | Tree walker doesn't descend a nested composite / layout wrapper | `core::walk::field_children` classifier (exhaustive over `FieldType`) is the sole composite-dispatch source — every field-tree walker routes through it, so a new composite is a compile error; **source-scan pin `tests/field_tree_dispatch.rs`** inventories every production `match …field_type` and fails on a new hand-rolled dispatch outside the reviewed allowlist | GUARDED |
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
