# Docs ↔ Code Audit & Feature Design Review — 2026-08-31

## Executive summary

Seven read-only research agents swept every mdbook section against the code
(both directions); the serious claims were spot-verified by hand; each feature
got a concept-level design review. Totals: **~110 findings** — 14 HIGH,
~45 MED, ~50 LOW — plus 16 feature-design verdicts.

**The headline pattern**: the docs are *strongest exactly where generation
exists* (proto RPC list, generated references, where-clause page, routes
reference — all verified fully accurate) and *weakest where tables are
hand-maintained twins of code* (MCP arg tables, options tables, slot tables,
config references). The single highest-leverage fix is extending the
generate-and-check pattern to the remaining hand-written tables.

**Code-side discoveries** (docs described the intended behavior; the code
diverges — these are FIX candidates, not doc edits; all personally verified
unless noted):
1. ★ Login-path auth strategies ignore `surfaces`/`activates_on` scoping (security).
2. ★ `Subscribe` ignores its documented `token` request field (proto promises it).
3. ★ `access.admin` treats a filter-table return as allow; MCP fails closed on
   the same shape; docs promise "config error" for both.
4. ★ Lua bulk conn-path writes never clear the populate cache (docs promise it).
5. ★ `[cache] backend = "custom"` is accepted and permanently no-ops (no
   registration API exists).
6. `Me` bypasses the `bearer` method check; MCP global exclusion is
   listed-but-uncallable; strategy writes aren't rolled back on failure;
   ranked FTS is dead code while docs claim ranking. (agent-verified with
   citations)

**Docs-side HIGHs**: the quickstart's gRPC examples fail on a fresh project
(reflection off + default-deny); two config keys documented in sections where
copying them aborts boot; the depth-defaults page inverts reality on 3 of 4
rows (and three stale code comments repeat it); phantom static-path
"compatibility aliases"; `admin.hidden` described as API-stripping; locale
docs promising shared-field writes that are silently dropped; the new
partial-nav model undocumented in the admin-ui guides; date filters normalize
to noon, docs say midnight.

Everything below carries file:line citations on both sides.

---

## Part C — Feature design review (concept quality)

Verdict scale: **Sound** (concept is right, keep), **Sound with caveats**
(right concept, specific rough edges), **Revisit** (works, but the concept
has a structural weakness worth addressing before it freezes harder).

### C1. Field system & storage model — **Sound**

The core idea — a *relational spine* (top-level scalars as real columns,
top-level arrays/blocks/has-many as join tables) with *nested JSON* below the
first container level — is the best compromise in this design space. It keeps
the hot queries SQL-native (filterable, indexable, FTS-able) while allowing
arbitrary nesting without schema explosion. The `group__sub` flattening for
groups is pragmatic and now consistently canonicalized (nested shape
in-memory, flat only at the DB edge). The one conceptual wart: the rule "a
join table only exists at the top level; the same field type nested one level
down becomes JSON" is invisible to users until they wonder why a nested
array isn't its own table. It's *documented*, and dot-notation filters mostly
hide it, but it remains the field system's steepest learning cliff. The
transparent layout wrappers (Row/Collapsible/Tabs contribute nothing to
naming) are exactly right.

### C2. Lua-first definition model — **Sound**

Definitions as executable Lua (`crap.collections.define`) instead of
YAML/JSON buys real wins: computed/shared config, typed hook references next
to the schema, comments, and the typing factories give LuaLS-level DX that
static config formats can't. The strict unknown-key rejection everywhere
turned the config surface from "silently ignores typos" into a real schema —
one of the best hardening decisions of this cycle. The init-only `define()`
rule (registry immutable post-boot) is the right simplicity trade; runtime
schema mutation is a can of worms correctly left unopened. Caveat: two VMs
re-running definition files (init VM writes registry, pool VMs re-run for
Lua-side state with a no-op define) is subtle machinery a contributor can
misread; it's internal, but deserves its internals page.

### C3. Versions & drafts — **Sound with caveats**

Snapshot-per-write with `_versions_{slug}` tables, `_status` on the parent,
and draft overlay reads is a clean, queryable model. `max_versions` retention
with prune-on-write is correct. Caveats: (1) `versions = true` defaulting to
*unbounded* history is a data-growth foot-gun — the bench DB grew 23 MB of
snapshots silently; a default cap (or a startup warning for uncapped
versioned collections) would be kinder. (2) The draft-read fallback chain
(published snapshot for unpublished globals, draft overlay per document) is
conceptually right but is the part of the system I'd least like to explain on
a whiteboard — the docs carry the burden here and must stay perfect.

### C4. Soft delete + ref-count delete protection — **Sound**

`_ref_count` as a denormalized integer maintained at every write chokepoint,
giving O(1) delete protection, is a genuinely good design — the recursive
walker sharing one implementation across create/read/backfill paths removed
the classic divergence risk. Trash-view semantics (soft delete doesn't
decrement; only hard delete adjusts) are coherent. The versioned backfill
gate (recompute once per computation change) is the right migration pattern.
No concerns worth listing; this subsystem earned its freeze.

### C5. Access control model — **Sound** (post-redesign)

The redesigned model — independent `read`/`draft`/`trash`/`versions` view
keys, each `true|false|filter`, with draft/trash/versions falling back to
`update`, reads computed as the *union of allowed views* (downgrade, never
error) — is the strongest conceptual piece of the auth story. The fallback
choices encode a sensible default policy ("previewing drafts is an editor
capability") while staying overridable. Field-level access being data-aware
across every surface incl. SSE closes the classic leak. Remaining
sharp edge: the *number* of keys (10) with three different fallback targets
is approaching the complexity where users will make wrong guesses;
the docs table of fallbacks is load-bearing and the startup validation of
unknown keys is what keeps this safe. Would not add an 11th key without
consolidating.

### C6. Auth methods & MFA — **Sound**

The `methods` list with a unified evaluator (password_login, bearer,
session_cookie, custom strategies with Always/Header activation, per-surface
scoping) is a clean extension point — the decision to make Payload-style
API keys a userland *strategy* rather than a built-in was correct
(one general mechanism instead of N specials). MFA's shape — challenge
tokens with a dedicated purpose + 5-minute expiry, `mfa_when` as a Lua gate
(fail-closed), `mfa = "custom"` + `mfa_deliver` with startup-validated
pairing — composes rather than special-cases. Shared login flow across
admin/gRPC ("the two surfaces cannot drift") is the op-core philosophy
applied to auth, correctly.

### C7. Hooks & Lua runtime — **Sound with caveats**

The lifecycle set (before/after × validate/change/read/delete + broadcast)
with conn-mode transaction access for before-hooks is the right power/safety
split: before-hooks join the caller's transaction (atomicity), after-hooks
can't (no CRUD), so a hook can't half-commit. The elastic VM pool and
hook-depth guard are solid. Caveats: (1) hook side effects (events published
from conn-mode before the caller's tx commits) remain a known wart —
the transaction-cleanup-hooks idea in the backlog is the real fix and should
happen before 1.0. (2) `after_read` fail-open (a hook error logs and returns
the unmodified doc) is defensible for availability but is a *policy* choice
that deserves its prominent doc note — a security-filtering after_read that
throws will silently stop filtering.

### C8. Validation pipeline — **Sound** (recent scar noted)

Chokepointed validation with typed errors + i18n keys, strict-by-default
after the freeze-hardening (unknown keys, non-numeric numbers, slug rules),
and the completeness publish-gate for localized required fields is coherent
and defensible. The scar: strictness interacting with old leniency
workarounds — this week's upload-placeholder bug (`_pending_upload` string
in Number fields) is exactly the class to expect more of; each strictness
change should grep for sentinel values that previously slid through.
The validate-follows-write-access decision (dry-runs gated like the writes
they preview) closed a real enumeration channel and is the right call.

### C9. Localization — **Sound with caveats**

Per-locale columns (`field__de`) for scalars and `_locale`-tagged join rows
— rather than row-per-locale documents — keeps single-document identity (one
id, one ref-count, one version chain), which is the right call for a CMS.
Locale-locked fields, create-in-default-locale-only, and required-locales
completeness are individually sensible. Caveats: (1) the *sum* of locale
rules (fallback, `all` mode, locale-locked, default-only create, completeness
gates, join-row localization by own-flag-only) is the system's highest
concept count per feature; it needs its single "mental model" doc page kept
authoritative. (2) The known open edge — sub-field `required` inside
localized arrays being locale-blind — should be fixed or explicitly
documented as a limitation before beta.

### C10. Uploads & image pipeline — **Sound with caveats**

Synchronous resize at upload (sizes are part of the document's contract) +
queued format conversion (webp/avif are enhancements) is the right sync/async
split. Storage as a trait (local/S3/custom-Lua) with the hoisted key
contract is clean. Caveats: (1) upload metadata fields being *injected* into
the def by the Lua parse path is invisible magic — a Rust-built def (tests,
future embedders) silently lacks them; either document loudly or expose the
injection helper. (2) The serve route being cookie/Bearer-gated makes
cross-origin/CDN use of private media impossible — the signed-URL idea in
the backlog is a real gap for production media serving, arguably pre-beta.

### C11. Live updates — **Sound**

One `MutationEvent` stream fanned out to gRPC server-streaming and admin SSE,
with per-subscriber access filtering and slot limits, over an in-process or
Redis transport, is a proportionate design — no over-built message bus. The
unidirectional subscription model (declare filters once) is what makes the
REST/SSE story cheap later. Known gaps are honest ones: no replay/resume,
per-event `after_read` cost for hot streams — both are documented backlog,
neither blocks the current scale.

### C12. Jobs & scheduler — **Sound**

Queue table in the same DB (visibility, transactional enqueue), per-queue
retry policy, cron + poll loops, system jobs (email, image-convert) riding
the same machinery as user jobs — boring in the best way. The
worker/live-transport gap found earlier (job handlers lacking event
transports) was fixed; standalone worker process sharing `AppInfra` assembly
keeps parity. The bulk-ops-on-queue idea remains the natural growth path.

### C13. API surfaces, op core & wire model — **Sound** (the crown jewel)

Eighteen operations defined once; every surface (gRPC/MCP/Lua/admin) a thin
codec of auth→ctx→op→encode; the wire model declaring each op's options once
and *generating* the MCP schemas, proto messages (pinned append-only tags),
and reference docs, with parity tests over the remaining hand sources. This
is the architecture that makes the freeze credible — drift became a compile
error. Design-wise there is nothing comparable to criticize; the one
discipline to hold is that *every* future surface (REST, if built) must be
one more consumer of the same model, never a parallel description.

### C14. Admin UI & customization surface — **Sound with caveats**

Server-rendered Handlebars + htmx + shadow-DOM web components with **one**
customization mechanism (drop-in overlay, now with finer-grained slots) is
a coherent identity — no build step, no framework churn, deterministic
override semantics, and the new partial-swap navigation keeps it feeling
modern without an SPA. The stable-API framing of slot names + the pin test
is the right way to make customization survivable. Caveats: (1) whole-file
template overrides still carry drift cost — the answer (more slots, then
finer partials driven by real override data in beta) is agreed, execution
pending. (2) Shadow-DOM components are a styling wall by design; the CSS
custom-property seams need to stay generous or "just restyle it" users hit
frustration. (3) `layout/base.hbs` just became more load-bearing (the
`htmx_partial` branch) — every structural change to it is now a documented
overlay-migration event; keep those rare.

### C15. Migrations & freeze machinery — **Sound**

Schema sync from definitions + explicit Lua data migrations + versioned
one-time backfill gates + `migrate fresh` for dev is a complete story.
The freeze machinery (frozen-contracts.md as a contract page, generated
wire artifacts with `--check` gates, per-alpha freeze-restart policy) is
unusually honest engineering — the freeze is enforced by construction and
*measured* per release rather than promised. This is the project's best
argument to future users.

### C16. Cross-cutting (config, CLI, errors) — **Sound with caveats**

Config: TOML + env substitution with defaults that lean production-safe;
strict parsing matches the definition-side philosophy. CLI: single binary,
subcommands cover the lifecycle (serve/migrate/user/status/fmt), detach mode
is a nice touch. Errors: the source-classified pool errors and typed
CoreError→per-surface mapping landed this cycle; the residual wart is that
`ServiceError::HookError` still carries several semantically different
failures (validation-ish, capability, access-adjacent) as strings — a typed
split would improve API error mapping fidelity. Not urgent, but it's the
kind of thing that gets harder to change post-1.0.

### Design review — overall

The system's signature strength is **chokepoint discipline**: one write
envelope, one validation pipeline, one access evaluator, one wire model, one
event stream. Where bugs appeared this cycle, they were almost always at
places that had *escaped* a chokepoint (the upload placeholder, the dead
proto field) — and the fixes consistently routed them back through one.
The features whose *concepts* still owe something before 1.0: hook
transaction cleanup (C7), signed media URLs (C10), the localized-array
required edge (C9), and the HookError split (C16). Nothing reviewed here
looks conceptually wrong; the architecture is over-delivering for an alpha.

---

## Part A — Docs contradicting code *(sweep results)*

### A-AdminUI (agent-reported, citations spot-checkable)

**HIGH**
- **Phantom "compatibility aliases."** `admin-ui/index.md:81-85` and `scenarios/08-upgrade.md:68-70,105-108` promise old static paths resolve via aliases with deprecation warnings. No alias layer exists (`static_assets.rs:57-61` → bare 404), and `upgrade/migrating-from-old-layout.md:8-11` explicitly says so — **two pages contradict a third on upgrade-breaking behavior**.
- **The new `#main` partial-nav model is undocumented in the admin-ui section.** `guides/template-overlay.md`, `reference/components.md:276`, and `scenarios/01-restyle.md:107-121` still describe `layout/base.hbs` as a plain shell and casually recommend forking it; a pre-change extract re-ported naively loses the `htmx_partial` branch and breaks all navigation. (Only CHANGELOG + upgrade/alpha-10 were updated.)

**MED** (selection; full citations preserved from sweep)
- `index.md:169` names `<crap-session-guard>`; real tag is `crap-session-dialog` (session-guard.js:353).
- `index.md:204-205` + `css-variables.md:88,169-200` + `themes.md:51` reference the dead pre-1.0 flat static layout (`styles.css`/`themes.css`); real paths are `styles/main.css`, `styles/themes/default.css`, `styles/tokens.css`.
- `guides/template-formatter.md:75-84` example still ships `hx-target="body"` — the last one in the docs tree.
- `guides/slots.md:125` documents the new `list_toolbar_actions` context key as `documents`; real key is `docs` (context struct `collections.rs:46`) — a slot author's `{{#each documents}}` silently renders nothing. *(our own yesterday's doc)*
- `atom-inventory.md` disagrees with `components.md` on the uploads tag (`crap-uploads` vs real `crap-upload-preview`), picker-base importers, tier counts, and util signatures.
- `themes.md` self-contradicts on where theme CSS lives (steps 1 vs 2); its picker-swatch example uses inline `style=` that CSP blocks.
- `scenarios/01-restyle.md:44` overrides a `--font-family` token that doesn't exist (font hardcoded in reset.css:36).
- `scenarios/02` links a nonexistent `list_settings.rs`; `scenarios/04` invents an unroutable nested custom-page slug (`/admin/p/widgets/weather` — slugs are single-segment, custom_pages.rs:112).
- `components.md:100-102` documents a `crap:document-deleted` event that nothing dispatches.
- `index.md:130-163` route table omits 8 routes that `reference/routes.md` (verified accurate) carries — two route tables, one stale.

**LOW**: drift-tooling counts "four read-only commands" then lists five (extract isn't read-only); stability-tier counts off by one (20→21, 32→33); scenario 08 titled "Scenario 7"; two stale code-side comments (htmx-nav-link.hbs header, tokens.css pointer).

**Verified accurate**: `reference/routes.md` (route-by-route), `guides/display-conditions.md`, `guides/static-files.md`, scenarios 03/05/06/07.

**✔ Disposition (2026-08-31): section COMPLETE.** All HIGH/MED/LOW items fixed:
phantom aliases rewritten (index + scenario 08); partial-nav documented in
template-overlay.md warning box + components.md base.hbs row + scenario 01
caution; session tag corrected; CSS paths modernized (index, css-variables,
themes); formatter example → `#main`; slots context key → `docs` (+
breadcrumb_extras/field_help rows added); atom-inventory demoted to planning
doc (components.md declared normative) + uploads tag fixed; themes.md step-1
path unified + swatch example moved to CSS (CSP); restyle font token →
`html { font-family }` override; scenario 02 → list_helpers.rs; scenario 04 →
single-segment slug; document-deleted event claim corrected (navigates, no
event); index route table replaced by pointer to normative reference/routes.md;
drift-tooling 4→5 commands; tier count 20→21; scenario 08 title; both stale
code-side comments (htmx-nav-link.hbs header, tokens.css pointer).


### A-Auth/Access (agent-reported; ★ = personally verified in code)

**⚠ Code-side findings — the docs describe the intended/safer behavior and the CODE diverges.
These are candidate pre-tag fixes, not doc edits:**

- **★ A7 (HIGH, security): login-path strategies ignore `surfaces` and `activates_on`.**
  `login_flow.rs:243` iterates every strategy on the collection unfiltered — a strategy
  declared `surfaces = {"grpc"}, activates_on = { header = "x-api-key" }` still executes on
  an admin form POST. The documented scoping exists only in the per-request evaluator
  (`evaluator.rs:304-328`). Docs: custom-strategies.md:104-109, auth-methods.md:95-98.
- **★ A2 (HIGH): `access.admin` returning a filter table PASSES the gate** (`gate.rs:71`,
  `Allowed | Constrained(_)` → through; comment says "no row scope to filter"), while docs
  promise "filter table = config error" AND `access.mcp` fails CLOSED on the same shape
  (`mcp/access.rs:46-54`). Deliberate-looking but inconsistent in the unsafe direction —
  admin is the last gate before rendering.
- **★ M9 (MED): `Me` RPC bypasses the `bearer` method check** — validates the JWT directly
  via token_provider (`me.rs:95-106`) instead of the evaluator; a collection with `bearer`
  removed still answers `Me`.
- **A5 (HIGH doc-or-code): the "reads downgrade, never error" rule is false for the trash
  view** — `resolve_trash_scope` hard-denies (`service/access.rs:70-76`); only draft
  downgrades. Either the model doc narrows its claim or trash adopts downgrade.
- **A6 (MED): `Login` on a strategy-only collection still proceeds via strategy fallback**
  (login_flow.rs:140-165) — docs claim local login refusal makes API keys "the only path in".

**Doc-side HIGH**
- field-level.md:43,47 names `admin.hidden` as the API-stripping flag; the real flag is
  top-level `hidden` (definition.rs:340; collect_api_hidden_field_names reads field.hidden).
  Readers will believe exposed values are hidden.
- overview.md:157-160 says `crap.access.check` rejects non-CRUD keys; `"unlock"` is accepted
  with the documented fallback (lua_api/access.rs:204-219).

**MED**: evaluation order is fixed precedence (bearer→cookie→always→header), never
`methods` declaration order (evaluator.rs:233-328) — two pages imply otherwise; a
decodable-but-invalid credential short-circuits all remaining paths (undocumented);
admin login requires an explicit collection field (login-flow.md:153 claims "tries all");
custom-strategies.md:279-283 shows the pre-redesign `auth = { mfa = "email" }` shape that
is now a hard startup error (self-contradicted 15 lines later); "enabled with empty
methods → error" is unreachable from Lua (defaults injected, parse/auth.rs:105-107);
"strategy without activates_on → error" is actually a warning.

**STALE**: grpc-api/authentication.md predates MFA entirely (no mfa_required/VerifyMfa);
public-route list predates /admin/mfa + OAuth callbacks; JWT claims table missing
session_version / token_use / auth_time (the revocation + replay mechanisms!);
overview.md still speaks the pre-`methods` "strategy chain" vocabulary. One REVERSED case:
`mfa_when`'s Rust doc-comment says "only meaningful with mfa=email" but code runs the gate
for any non-Off mode — the DOCS are right, the code comment is stale.

**✔ Disposition (2026-08-31): COMPLETE.** Code-side: **A7 → CODE FIX (Security)** —
`login_flow::strategy_applies` filters login-path strategies by surface + activation
(unit-tested); **A2 → CODE FIX (Security)** — `gate_passes` makes both admin gates
boolean (Constrained = logged error + deny; unit-tested); **M9 → CODE FIX (Security)** —
`Me` resolves through `resolve_auth_user` (gRPC regression `me_honors_collection_methods_without_bearer`);
**A5 → doc** (trash/versions are different sets, not supersets — hard-deny is the right
design; overview.md narrowed to "downgrade on the status axis"); **A6 → doc** (Login
→ PERMISSION_DENIED only when no strategies; strategies now scoped by A7).
Doc-side HIGH: field-level `hidden` fixed earlier; `crap.access.check` now lists
`unlock`. MED: fixed-precedence evaluation order (bearer → cookie → always → header,
short-circuit on decodable-invalid, login-path scoping) written once and reused on
custom-strategies.md + auth-methods.md; admin login = explicit `collection` field;
custom-strategies.md MFA example → methods shape; validation-rules list rewritten
(load errors / startup errors / warnings) — and made TRUE: **CODE FIX (Breaking)**
`methods = {}`, strategy without `authenticate`, strategy without `activates_on` are
now load errors. STALE: grpc-api/authentication.md rewritten (MFA + VerifyMfa, claims
table incl. session_version/token_use/auth_time, Me via metadata/strategy, revocation);
login-flow.md public routes (+/admin/mfa, callbacks, /health, /ready) + request-auth
paragraph; overview.md "strategy chain" → methods vocabulary; `mfa_when` Rust comment
fixed. **Bonus bug found by the new validator test: `auth = { methods = {...} }`
without `enabled = true` parsed as DISABLED (mlua reads a missing bool as `false`),
and `forgot_password` defaulted to off — fixed via strict `get_bool` (Fixed entry).**

### A-Collections/Fields (agent-reported; ★ = personally verified)

**HIGH**
- **Depth defaults table is wrong on 3 of 4 rows** (population-depth.md:16-20): gRPC Find,
  Lua find, Lua find_by_id all default to `default_depth` (config default **1**), not 0 —
  one shared `clamp_depth` (helpers.rs:37). The page's headline advice ("Find defaults to
  depth=0 to avoid N+1") is inverted: out of the box every list read populates one level.
  THREE stale code comments repeat the wrong claim (config/features/depth.rs:11, a proto
  comment, lua find.rs:59) — fix together.
- **Top-level `has_many` vs `relationship.has_many` silent storage trap** (fields M1/D1):
  both parse; only `relationship.has_many` produces a junction table — the top-level flag
  on a Relationship/Upload silently forces a TEXT JSON-array column: no junction, no
  populate, no ref-counting. No startup check rejects the combination. Docs read as if the
  top-level key is the switch. Candidate parse-time rejection (house precedent exists).

**MED**
- RestoreVersion preserves the snapshot's status (restore.rs:268 — deliberate, doc says
  "sets published"); restore also CLEARS all non-default-locale translations
  (versions/restore.rs:39-43 — destructive, documented nowhere).
- Unknown field type = hard error (never falls back to text as text.md:50 claims).
- Number `step` defaults to "any" unless integer=true (docs say "1").
- Hook refs everywhere accept `{ ref, options }` tables; definition-schema still says
  plain strings; validate-context docs omit `ctx.options`.
- Soft delete silently rewrites `unique` into a partial index (trashed values reusable)
  and enabling it on existing tables with uniques triggers a full table rebuild — both
  undocumented.
- Slug rules (lowercase/digits/_ + reserved `many_`/`by_id_` prefixes) documented nowhere;
  `required_locales` missing from both schema pages; join fields hard-error on
  required/localized (docs imply ignored); admin key table lists 10 of 23 accepted keys.

**LOW**: tab label not actually required (silently ""), delete-protection error text
differs per surface (three variants), admin.width accepts any CSS width (docs claim 3
values, richtext page contradicts), option label accepts LocalizedString, join-table DDL
samples omit _locale/_tz companions, nesting depth warns at startup (not silent),
checkbox PG SMALLINT unmentioned.

**Verified accurate**: group/row/collapsible storage docs, date.md (fully), email
injection, versions DDL, globals access-key rejection, delete-protection semantics.

**✔ Disposition (2026-08-31): COMPLETE except one Tier-4 item.** Depth table
fixed earlier + all THREE stale code comments fixed (depth.rs, wire_proto/proto
pair, Lua find.rs — regen gates at final pass). MEDs: restore status+
translation-wipe documented in versions.md (restore-from-snapshot escalated to
Tier 4); unknown-field-type → hard-error doc; number step → "any"/integer-"1";
hook refs `{ref,options}` documented (definition-schema, hooks overview,
validate ctx.options row); soft-delete partial-unique + rebuild documented;
slug rules section added to definition-schema; required_locales rows added to
both schema pages; join required/localized hard-error documented; admin key
table completed 11→16 keys (all FieldAdmin fields). LOWs: tab label now a
LOAD ERROR (code fix + regression test + Breaking CHANGELOG — matches the
documented contract); width→any-CSS; select option label LocalizedString;
array/blocks DDL tables +_locale/_tz rows; depth cap warns at startup (doc);
checkbox PG SMALLINT noted. Dismissed: delete-protection error-text variants
are INTENTIONAL (admin toast omits the id — context already shows the doc;
service/gRPC include it). Remaining: has_many parse-time rejection → Tier 4.

### A-Query/Locale/Cache (agent-reported; ★ = personally verified)

**HIGH**
- **Locale-lock silently drops shared-field writes under a non-default locale**
  (update.rs:196-203; join save too): docs claim "non-localized fields always written".
  `update(id, {title=x}, {locale="de"})` succeeds while dropping title. Related: the
  documented create-in-de example errors at runtime (create is default-locale-only), and
  the "include checkbox values" advice in the remove-translation recipe is inert.
  *(Design: silent drop vs loud error — see recommendations.)*
- Date filter values normalize to NOON not midnight (helpers.rs:82-89) —
  `greater_than "2024-01-15"` excludes that morning's rows; doc example says T00:00.
- query overview :26 still claims gRPC rejects numeric/bool shorthand — pre-unification
  text contradicting the (correct) where-clause page.

**MED**
- **★ `search` is a filter, not ranked search**: Find composes FTS as `id IN (...)` with
  normal ordering; the ranked `fts_search` (ORDER BY rank) has zero production callers
  (verified — test-only). Docs claim relevance ranking. Also: terms are PREFIX-matched,
  and Postgres FTS exists (docs say SQLite-only).
- **★ Lua bulk conn-path writes never clear the populate cache** (verified:
  create_many/update_many/delete_many `*_in_conn` have no clear_cache; singles do) —
  cache.md explicitly claims bulk clears. CODE BUG candidate.
- api-surface-comparison: Lua events row wrong (queued + opt-out exists), Lua depth row
  wrong (config default, not 0).
- filter-operators.md scopes itself to access constraints but half its operators are
  rejected there (validate_filters); `exists=false` on the Lua ACCESS path is silently
  dropped (third grammar — see D1 rec).
- in/not_in element rules (non-scalar = hard error; mixed scalars stringified) and error
  grammar undocumented on all filter pages.

**LOW**: page=1+cursor accepted (exclusion only for page>1), Lua reads never touch the
cache at all (broader than documented), multi-op range objects undocumented, empty
in/not_in semantics (not_in:[] matches everything — bulk-delete foot-gun), cursor params
silently dropped in page mode, admin URL grammar is a third dialect, select retains
_status, redis clear non-atomic. Drafts collections silently prepend `_status ASC` to
every sort (MED, all surfaces).

**✔ Disposition (2026-08-31): COMPLETE except 3 Tier-4 items.** HIGH: date-noon +
gRPC-shorthand text fixed (overview + filter-operators); locale-lock silent drop →
documented as-is in locale/overview.md, **decision open (Tier 4)**. MED: search re-framed
as a prefix *filter* (ranked mode = **Tier 4** decision); ★ bulk `*_conn` cache clear →
**CODE FIX** (`clear_cache()` in create_many/update_many/delete_many conn paths +
regression `bulk_conn_paths_clear_cache`); api-surface rows fixed; filter-operators.md
re-scoped to CRUD with an access-subset callout; **`exists=false` → CODE FIX (Breaking)**:
both parsers (`decode.rs` + Lua `FilterOperators`) now reject any value but `true` —
previously wire = IS NOT NULL (inverted), Lua = silently dropped; access path fails
closed (Denied) with regression tests on all three; in/not_in element rules + error
grammar documented. LOW: page=1+cursor, Lua-reads-no-cache, ranges, empty in/not_in,
cursor-drop-in-page-mode, `_status ASC` prepend, redis non-atomic — all documented;
select: doc bullet was WRONG (only `id` is unconditional; created_at/updated_at/_status
stripped unless named; unknown select names silently ignored) → fixed; admin URL
grammar → **internal, not a public contract** (admin list URLs are a UI concern; full
unification onto `decode_where_map` is rec #17, tracked as a post-tag refactor).
**New Tier-4 question**: should an unknown `select` name be a hard error (strict-input
policy) instead of silently selecting nothing?

### A-Setup/Config/CLI (agent-reported; ★ = personally verified)

**HIGH**
- **The quickstart's gRPC examples fail on a fresh project**: grpc_reflection defaults
  false (doc claims "server supports reflection") AND access.default_deny=true denies the
  anonymous Find. First-run experience is broken as documented.
- **rate_limit_backend/redis_url/prefix documented under `[auth.password_policy]`** —
  deny_unknown_fields makes copying the doc a boot failure; they belong in `[auth]`
  (multi-server.md has it right).
- **`trust_proxy = true` alone is now a fatal startup error** (needs trusted_proxies);
  crap-toml.md never mentions it.

**MED**
- **★ `[cache] backend = "custom"` is a permanently silent no-op** — accepted by config,
  returns a placeholder whose get is always None, and NO `crap.cache.register` exists
  (verified; email/storage customs are fully wired). Worst-of-three-options trap: reject
  at validation or implement registration.
- Validation-rules doc lists 8 fatal checks; code has ~30 (+9 warnings) — incl. the
  32-char mcp.api_key minimum absent from the [mcp] table.
- `[jobs.queues]` example uses `emails` but the framework queue is `email` — copying the
  example silently fails to override email delivery (startup warning only).
- CLI: RUST_LOG default table wrong (dev_mode-conditional; work/mcp missing); make job
  --retries omission inherits queue default (doc claims 0); init --no-input requires the
  dir (doc claims ./crap-cms default); jobs healthcheck always exits 0 (doc implies CI
  gate); import skips hooks/validators (undocumented); 7 whole `make` subcommands
  (page/route/slot/node/field/theme/component) undocumented; container/blocks/tabs field
  shorthand undocumented but USED in the page's own example.
- config-directory: jobs/ load order + routes/ dir omitted; database/overview.md still
  describes ONE shared pool (read/write split is documented only in crap-toml).

**LOW**: dev_mode scaffold claim wrong, bulk_max_documents missing from full-reference
block, busy_timeout is ms not seconds (page self-contradicts), auth system columns
understated (6+ not 1), scaffold tree/wizard lists incomplete, distro-path list missing
/bin /sbin, db console errors on postgres.

**Verified accurate**: single-server.md (fully), multi-server.md (fully),
installation.md (nearly), serve/user/migrate/backup/trash/typegen/update CLI docs.

**✔ Disposition (2026-08-31): COMPLETE except the `[cache] custom` decision.** HIGH ×3
(quickstart reflection/default_deny, `[auth]` rate-limit keys, trust_proxy/trusted_proxies)
were fixed earlier and re-verified. MED: `[cache] backend = "custom"` → **user discussion
open** (implement via `LuaVmLease` like storage/email vs remove the dead limb; code
untouched pending the call); validation-rules section rewritten as a full table (all
`bail!` sites in validate.rs + 7 warnings, `deny_unknown_fields` noted); mcp `api_key`
32-char minimum in the table; `[jobs.queues]` example `emails`→`email` + framework-queue
note + typo-is-only-a-warning note; CLI: RUST_LOG table (dev_mode-conditional, work/mcp),
`make job --retries` = queue default, `init --no-input` requires DIR, **`jobs healthcheck`
→ CODE FIX** (exit 0/2/1 like `status --check`, `JobHealth` + `classify_health` unit
test), `import` documented as raw per-collection-transaction upsert skipping
hooks/validation/access/events (+ **CODE FIX**: all slugs validated before any write),
7 `make` subcommands (page/route/slot/node/field/theme/component) documented with
flags + output paths, container/blocks/tabs shorthand grammar documented;
config-directory: tree (routes/, pages/, slots/, components/, themes/), init-created
dirs, load order (jobs/ 4th, init.lua last, hooks/access/routes lazy via refs);
database/overview: read/write pool split. LOW: dev_mode scaffold claim corrected (`false`),
`bulk_max_documents` added to the full-reference block, `busy_timeout` ms exception in
the duration note, auth system columns (6 + 3 verify + 2 MFA) listed, scaffold tree +
wizard "Project path" prompt, distro paths (+`/bin/`, `/sbin/`, symlink-resolved),
**`db console` on PG → CODE FIX** (launches `psql <database.url>`, unit-tested).

### A-Uploads/MCP/gRPC/Live/Lua-CRUD (agent-reported; ★ = personally verified)

**HIGH**
- **★ `Subscribe` ignores its documented `token` request field** — the proto comment
  itself says "Pass a valid JWT in SubscribeRequest.token (not in metadata)" (the field's
  entire raison d'être, since streaming metadata is sent once), but subscribe.rs:298 reads
  ONLY the `authorization` metadata header; req.token is never read. Every documented
  authenticated-Subscribe example silently subscribes anonymously. (`Me` implements the
  fallback correctly — inconsistent within one service.) CODE FIX candidate.
- Lua `list_versions` returns `documents`; docs iterate `result.docs` (nil).
- `crap.globals.<slug>.unpublish()` doesn't exist (accessor binds only get/update);
  the page contradicts itself vs its own validate section.
- Pagination keys documented camelCase in two places; everything is snake_case since the
  casing unification (collections.md self-contradicts across 15 lines).

**MED**
- Upload API: invalid token is 401 not 403; 409/503 undocumented; "all endpoints require
  auth" is false (anonymous allowed when access rules permit).
- MCP: `write_config_file` writes ANY file type (doc says Lua) — opt-in justification
  understates scope; globals are listed-but-uncallable when excluded (filter asymmetry
  between tools/list and execute — CODE FIX candidate); reserved-args table missing
  `events`+`hooks` keys (drifted despite wire model — the mdbook table remains
  hand-written); list_versions/restore_version params undocumented.
- rpcs.md GetGlobalRequest snippet missing `draft`; image-processing says id-prefixed
  filenames (actually random nanoid — client-uploads has it right).
- "Only available inside hooks" claims stale across 8 spots — Lua CRUD works in job
  handlers and custom routes since auto_tx/pool-mode.

**LOW**: SSE with live disabled returns an empty 200 stream (docs say unavailable),
uploads hidden-fields claim, collections.md still calls offset an "alias for page"
(prose page missed in our fix), format quality defaults (webp 80/avif 60) undocumented,
custom-storage `exists` handler undocumented, MCP HTTP 1MiB cap + 204-notifications
undocumented.

**Verified accurate**: grpc-api/overview.md (all 33 RPCs), live-updates/hooks.md +
admin-sse.md, image-processing.md (except the prefix claim), type-safety.md (minus 2
FieldInfo fields).

**✔ Disposition (2026-08-31, code fixes landed 2026-09-01): COMPLETE.** `Subscribe` now honors `SubscribeRequest.token` (regression `subscriber_authenticated_via_request_token_field`); MCP globals obey include/exclude in listing + schema resources (regression `global_tools_obey_include_exclude_in_listing`). Original note follows.
Fixed: list_versions → `result.documents` (3 spots); globals accessor now BINDS
unpublish+validate (code fix + regression test + typegen + CHANGELOG — doc
example was made true instead of removed); pagination casing (4 files);
upload-API auth/status table (401/403/409/503 + anonymous note); MCP
write_config_file scope + trust warning; reserved-args table +events +hooks
(with the single-vs-bulk default asymmetry); list_versions/restore_version
args; GetGlobalRequest draft field; image filename → nanoid prefix; "only in
hooks" stale claims (8 spots, incl. the runtime error text); SSE-disabled
behavior corrected (200 empty stream vs gRPC UNAVAILABLE); offset row fixed;
webp/avif default qualities noted; custom-storage `exists` documented; MCP
HTTP 1 MiB cap + 204-notification documented. Dismissed: uploads
hidden-fields claim (overview.md:55 verified CORRECT — admin.hidden only).
Remaining (Tier 1): Subscribe token field; MCP globals list/execute asymmetry.

### A-Hooks/Jobs/Lua (agent-reported)

**HIGH**
- **`before_broadcast` runs INLINE on the request's blocking thread** — docs say
  "background task, never blocks the response" (three places); broadcast.rs:234 comment
  confirms inline-by-design. A slow broadcast hook delays every write response. Either
  docs change or the design decision gets revisited (it was deliberate: avoids nested
  blocking-pool competition).

**MED**
- transaction-access.md over-promises: access checks get NO dedicated transaction (guard
  on the caller's conn), and auth strategies run on a bare pooled connection — **a failed
  strategy's writes are NOT rolled back** (design-relevant, not just docs).
- The "CRUD only in hooks" error text + availability list predate pool-mode (job
  handlers, custom routes, crap.transaction).
- `[jobs] max_concurrent` is CLUSTER-wide (COUNT with no node filter); docs say
  per-server in three places.
- `crap.metrics` doesn't exist but is shown as a "common pattern" (copy-paste = nil
  index); ctx.slug vs ctx.job.slug in the same table.
- `[http] max_response_bytes` is actually `[hooks] http_max_response_bytes`; JSON decode
  DOES enforce a depth limit (docs claim it doesn't — inverted safety note).
- `ctx.id` overclaim: before_read has no id (generated crap.lua is correct; prose isn't).

**LOW**: elastic-pool staleness ("fully initialized at startup"), field-hook tier exists
for only 4 of 9 events (overview implies all), ctx.context request-vs-operation scope
(prose right, Rust doc-comment + typegen wording stale — fix Rust side).

**✔ Disposition (2026-08-31): all MED+LOW items FIXED.** transaction-access.md
rewritten honestly (access = caller's conn, strategies = no tx + rollback
caveat + idempotency guidance); CRUD availability rewritten across overview/
collections/globals/jobs + the runtime error text itself updated in
tx_conn.rs (pool-mode contexts enumerated); max_concurrent → cluster-global
(jobs.md table + crap-toml row); crap.metrics row → crap.log.warn; ctx.slug →
ctx.job.slug; `[hooks] http_max_response_bytes` key fixed; JSON decode
depth-limit note un-inverted (serde_json 128-level cap); before_read ctx.id
scoped; elastic-pool wording fixed; field-hook tier scoped to its 4 events;
hook_context.rs doc-comment → operation-scoped (regen types/crap.lua at
gate). HIGH before_broadcast-inline → Tier 4 (user decision, pending).

## Part B — Code lacking docs *(sweep results)*

### B-Hooks/Jobs/Lua + APIs (agent-reported)
- `ctx.edited_by` on before_broadcast, field-hook `ctx.options`, job-handler
  `ctx.options`, route `ctx.ui_locale` — all real context fields absent from prose.
- `crap.any.route_handler` missing from typing-factories; overview namespace table
  omits 6 namespaces that have their own pages; `crap.hooks.list` undocumented.
- schema.md field table missing 12 keys; jobs CLI list missing cancel/healthcheck;
  email queue seeded defaults undocumented; `crap.jobs.define` strictness unnoted;
  `crap.email.register` init-only unnoted.
- **Upload validation is 3 checks deeper than documented**: magic-byte MIME
  verification, extension↔content cross-check (PNG-named-.svg rejected), SVG XXE scan,
  EXIF orientation + metadata stripping — significant security/privacy properties with
  zero doc coverage (a selling point left unsold).
- **undelete/unpublish/restore_version all publish as `update` events** — subscribers
  can't distinguish; EventViewMeta already carries the discriminator (design note).
- `public_url`/`[upload.s3] public_url_base`/Lua `url` handler: documented, complete,
  and DEAD (no production caller; everything routes through the /uploads proxy) —
  ties into the signed-URL backlog item.

**✔ Disposition (2026-08-31): COMPLETE except 2 Tier-4 items.** `ctx.edited_by` was
already documented (live-updates/hooks.md); field-hook `ctx.options` row, job-handler
`ctx.options` line, route `ctx.ui_locale` row added; `crap.any.route_handler` section
added to typing-factories.md; 5 missing namespaces (access/json/routes/pages/
template_data) added to the overview table; `crap.hooks.list` was already documented;
schema.md field table = the `crap.schema` *FieldInfo* view (deliberately narrower than
the definition schema — not a gap); jobs CLI list +cancel/+healthcheck; email/images
queue seeded defaults documented in crap-toml; `crap.jobs.define` strictness +
`crap.email.register` init-only/strict noted; **upload validation chain** (size, MIME
allowlist, magic bytes, extension↔content, SVG XXE scan, EXIF orientation + metadata
strip) now has its own section on uploads/overview.md. Remaining **Tier 4**:
EventOperation vocabulary for undelete/unpublish/restore; dead `public_url` limb.

### B-Auth/Access (agent-reported)
- `ctx.options` on access functions undocumented; collection-level table doesn't say
  `ctx.document` is field-only.
- Sliding session refresh (`/admin/api/session-refresh`) + `auth.session_absolute_max_age`
  (default 30d) — completely undocumented; `token_expiry` reads as the only bound.
- Field-level access rule returning a filter table = treated as Allowed (check.rs:70,133)
  — undocumented semantics.
- Startup validation rules absent from docs: empty `surfaces` = error, empty strategy
  `authenticate` = error, empty activation header = error; strategy with MISSING
  `authenticate` is silently dropped by the parser (genuine silent-drop, worth fixing or
  documenting).
- `Invalid(Unaccepted)` clears the session cookie; `Invalid(Lookup)` keeps it — nowhere
  described.
- Admin MFA-code issuance throttle (reuses forgot-password budget; silently reuses prior
  code) — undocumented, and gRPC deliberately has no issuance limiter (asymmetry).
- Account-state guards (locked users can't consume verification tokens; lock/unverify bump
  session_version and tear down live streams) — undocumented.

**Verified accurate**: access-control/filter-constraints.md (every claim),
authentication/cli-user-creation.md.

**✔ Disposition (2026-08-31): COMPLETE.** `ctx.options` + "`ctx.document` is field-level
only" rows added to the access ctx table; sliding refresh + `session_absolute_max_age`
documented (login-flow.md "Session lifetime" + crap-toml `[auth]` row); field-level
filter-table = allowed noted on field-level.md; startup validation rules rewritten on
auth-methods.md (and the silent-drop of a strategy with missing `authenticate` is now a
**load error**, see A-Auth); `Invalid(Unaccepted)` clears cookie vs `Lookup` keeps it
documented in login-flow.md; MFA guess/issuance throttles + gRPC asymmetry documented in
custom-strategies.md; account-state guards (lock/unverify bump `session_version`, stream
teardown, locked users can't consume reset/verification tokens) on auth-collections.md.

### B-AdminUI (agent-reported)
- **Slots guide table lists 12 of 14 built-ins** — `breadcrumb_extras` and `field_help` are declared in templates (and pinned by the new stable-API test) but absent from the guide's table; `field_help` even serves as the guide's own hash-params example without being listed.
- **`list_footer` renders only when the list is non-empty** (`items.hbs:159-164` — slot inside `{{#if docs}}`) — undocumented conditional; legends/totals vanish exactly on empty filter results. *(behavioral nuance of yesterday's slot)*
- Four `@stability stable` components missing from `components.md` tables: `crap-array-row`, `crap-pill-list`, `crap-column-picker`, `crap-filter-builder` (only in the explicitly non-normative atom-inventory).
- Three shipped events (`crap:column-picker-saved`, `crap:filter-builder-applied`, `crap:pill-removed`) have no `EV_*` constants — events.md's own rule classifies them "internal", while atom-inventory presents them as stable subclass surface. Contradictory classification.
- The sidebar/nav partial family (`sidebar_item.hbs`, `sidebar_global_item.hbs`, `header.hbs`) — where the `#main` contract now lives — is referenced by no docs page.

**✔ Disposition (2026-08-31): COMPLETE.** Slots table 14/14 (fixed earlier);
`list_footer` non-empty-only condition noted; 4 stable components added to
components.md; the 3 events got `EV_PILL_REMOVED` / `EV_COLUMN_PICKER_SAVED` /
`EV_FILTER_BUILDER_APPLIED` constants (dispatch + listener sites switched, events.md
sections added) so the "internal" classification contradiction is gone; nav-partial
family documented on template-overlay.md (note: they live in `templates/layout/`, not
`partials/` as the finding said — `partials/htmx-nav-link.hbs` is the only partial).


## Part D — Recommendations *(accumulating)*

### From Collections/Fields + Query/Locale + Config sweeps (endorsed)
10. **Fix the depth-defaults story as one unit** — docs + three stale code comments +
    (decide) whether Find SHOULD default to 0; today's default-1 quietly re-creates the
    N+1 the docs brag about avoiding. Perf-relevant.
11. **Reject top-level `has_many` alongside a `relationship` table at parse time** —
    silent wrong-storage trap; matches the house "no meaningless config" stance.
12. **Locale-lock should stop being silent**: either error like create does, or return
    the dropped-field set. A success response that dropped fields is the worst option.
13. **Bulk conn-path cache clear** (verified gap) — add clear_cache to the three
    `*_in_conn` bulk paths; trivially fixable, doc already promises it.
14. **Decide the search story**: wire the (tested, dead) ranked mode or delete it and
    re-frame docs as full-text *filter* with prefix matching.
15. **Kill `[cache] backend = "custom"`** until registration exists (validation error
    "not implemented"), or implement `crap.cache.register`.
16. **Fix the quickstart**: enable reflection in the scaffold, or rewrite the examples
    with auth — first-run failure is the most expensive doc bug in the book.
17. Unify the remaining two filter dialects (Lua access-constraint parser, admin URL
    grammar) onto decode_where_map with post-decode allowlists — makes "one grammar"
    literally true (agent D1, endorsed).
18. Fix the misplaced `[auth]` rate-limit keys + trust_proxy docs before anyone deploys
    behind a proxy from the book.

### From the Auth/Access sweep (endorsed after spot-verification)
5. **Fix the login-path strategy scoping (A7) before the tag** — a 5-line filter honoring
   surface + activation on `auth.strategies()`; the current behavior voids documented
   security scoping.
6. **Unify Constrained handling on the boolean gates** (admin vs mcp): pick fail-closed
   (mcp's shape, matching versions/create precedent) and document it.
7. **Decide trash-view semantics** (A5): downgrade like draft, or document the deny.
8. **Two auth pipelines (login_flow vs evaluator) is the root cause** of A6/A7/A8 — either
   converge semantics or document the two fixed precedences explicitly; drop all
   "declaration order" language.
9. Consider `surfaces = "all"` sentinel before a third surface makes every existing
   two-surface example silently exclusive (agent D4; endorsed).

### From the Admin UI sweep (agent design observations, endorsed after review)
1. **Generate the doc tables that keep drifting.** Slots table, component-tag table, CSS-token table — same treatment as template-context.md (`gen-*` + `--check`). Roughly half the admin-UI findings become mechanically impossible.
2. **Give `base.hbs` fewer reasons to be forked.** The FOUC/theme bootstrap deserves a config key or dedicated slot; a risk column in the customization-mechanism matrix (overriding base.hbs ≠ overriding toast.js).
3. **Resolve the two-reference problem**: make components.md the single normative reference; demote atom-inventory to planning prose or fold it in.
4. **The new toolbar slots invite buttons with nowhere to POST** — an additive server-side seam (custom-page POST handlers via `crap.routes.register` for `/admin/p/{slug}`) closes the loop the slots opened. Candidate for the window backlog.

### Final synthesis — prioritized

**Tier 1 — code fixes before the alpha.10 tag** (small, correctness/security):
strategy scoping on the login path (#1); Subscribe token field (#2 — honor it
like `Me` does); bulk conn-path cache clears (#4); admin-gate Constrained
handling decision (#3); reject `[cache] backend="custom"` (#5). Each is
hours, not days, and each has its regression-test shape implied by the finding.

**Tier 2 — the doc-generation wave** — ✔ IMPLEMENTED 2026-09-04 (user chose
pre-tag): `cargo xtask gen-doc-tables [--check]` + `src/docgen/{region,
css_tokens,mcp_reserved,components}.rs`. Live targets: slots-guide table (from
`SLOT_DOCS`, which now also drives the stable-API pin), whole-file
css-variables.md (from tokens.css itself), MCP reserved-args table (tools
column derived from the wire model — immediately exposed the hand table missing
locale/draft on reads and events on delete/undelete/unpublish), and the three
component tables (from `@category`/`@stability` header annotations beside every
`customElements.define`, 35 files annotated). CI additionally gained the
missing gen-proto/gen-wire-doc gates (CLAUDE.md claimed them; ci.yml lacked
them) plus gen-doc-tables. STAGED (not built): config-reference key tables from
the serde structs — needs a derive; assess post-tag.

**Tier 3 — high-traffic doc fixes by hand** (the quickstart, the depth page,
the locale page, transaction-access.md, the misplaced [auth] keys,
trust_proxy, partial-nav in the admin-ui guides, the camelCase pagination
remnants, `crap.metrics`).

**✔ Recommendations disposition (2026-09-01).** Collections/Query/Config: #10 done
(docs + 3 stale comments; "should Find default to 0" stays Tier 4 with the depth
table); #11, #12, #14 → Tier 4; #13 done; #15 → user discussion (implement via
`LuaVmLease` vs remove); #16, #18 done; #17 → post-tag refactor (admin URL grammar is
internal; access-constraint parser already shares `FilterValue` with Lua CRUD — the
`exists=false` divergence it caused is fixed on both parsers). Auth/Access: #5, #6, #7,
#8 done (two fixed precedences documented once, all "declaration order" language
dropped); #9 (`surfaces = "all"` sentinel) → **Tier 4** (additive; decide before a third
surface exists). Admin UI: #1 = Tier 2 generation wave (not started — decision: pre-tag
or post-tag); #2 partially (partial-nav warning + nav-partial family documented; a
config key/slot for the theme bootstrap → Tier 4); #3 done (atom-inventory demoted);
#4 → backlog (custom-page POST handlers via `crap.routes.register`).

**Tier 4 — design decisions surfaced by the audit** (deliberate choices to
make, not bugs): locale-lock silent-drop vs error; before_broadcast inline vs
background; search=filter framing vs wiring ranked mode; EventOperation
vocabulary for undelete/restore; dead `public_url` limb vs signed-URL plan;
`has_many` parse-time rejection; strategy-transaction semantics;
**restore-wipes-translations** (snapshots DO carry `__xx` locale values but
`restore_locale_columns` NULLs all non-default locales — restore-from-snapshot
would be strictly better; documented as-is for now, versions.md);
**`[cache] backend = "custom"`** — ✔ DECIDED + IMPLEMENTED 2026-09-01 (user chose
implement): `crap.cache.register` + lease-backed `CustomCache` + per-VM `LocalLease`
for in-VM clears + boot check, tests, docs (`lua-api/cache.md`);
**unknown `select` names** silently select nothing — hard error under the strict-input
policy? *Resolved by reasoning (no user decision needed): trash/versions hard-deny
(different sets, not supersets — documented).*

**✔ Tier-4 DECIDED + IMPLEMENTED 2026-09-03** (user rule: cleanest solution,
break pre-tag): locale-lock → validation error; has_many → parse rejection;
event vocabulary → dedicated undelete/unpublish/restore operations end-to-end;
public_url limb → removed; strategy writes → transactional (commit on success);
unknown select names → hard error; surfaces → strict + "all" sentinel; ranked
search → dead code deleted (additive feature later); restore → translations
restored from snapshot. Deliberately post-tag: before_broadcast stays inline;
theme-bootstrap key; ranked-search feature. All with regression tests +
CHANGELOG + frozen-contracts.md "Pre-alpha.10 design freezes".

**A meta-observation for the freeze**: every doc-vs-code divergence found
here predates the generated-contract machinery. The subsystems that went
through this cycle's single-sourcing came out clean; the ones still on
hand-maintained twins did not. That is strong evidence the Tier-2 investment
pays for itself before beta.

