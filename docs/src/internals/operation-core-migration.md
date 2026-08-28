# Operation Core — Architecture & Migration Plan

> **Status: Stage 0 landed (August 2026), in an extended form; later stages
> remain future work.** What shipped:
>
> - **`AppInfra`** (`service::app_infra`) — the process-stable dependency
>   bundle, assembled **once at boot** (`bootstrap_startup`) and shared as a
>   single `Arc<AppInfra>` by every surface in `serve` (gRPC, admin, MCP-HTTP,
>   scheduler). Standalone processes (`crap-cms work`, stdio MCP) assemble
>   their own via `AppInfra::standalone`.
> - **`ServiceContextBuilder::infra(&AppInfra)`** sets all eight infra fields
>   of a `ServiceContext` in one call — the forgot-a-field bug class this
>   document opens with is now structurally impossible on the pool-mode paths.
> - The per-surface god-structs were **de-duplicated**: `ContentService`,
>   `AdminState`, `McpServer`, and `ToolExecCtx` each hold the shared `infra`
>   instead of 7–10 copied fields; the surface start-params
>   (`GrpcStartParams`, `AdminStartParams`, `SchedulerParams`) carry
>   `infra` + genuinely per-surface state only.
> - The **Lua surface** deliberately does not hold `AppInfra` (the runner owns
>   the VMs — a cycle) — its VM-stable app-data is consolidated as
>   `LuaVmInfra`, fed from the same boot wiring; see the module docs on
>   `service::app_infra` for the full rationale.
>
> **Not done (still this document's future work):** Stage 1's
> `OpContext`/`ServiceContext` split, and Stages 2–4 (typed per-operation
> inputs, the shared dispatch entry, per-surface codecs). The sections below
> are kept as the design for those stages; read "Stage 0" as shipped.

## 1. The problem, stated from the evidence

Ten rounds of "chokepoint" audits (see the audit memos) found the same shape of
bug over and over:

- a guard added to the gRPC handler but missing from its Lua/MCP twin;
- a limit floored on three surfaces and not the fourth;
- a `ServiceContext` built for one operation that silently dropped a field
  (`update_many` once omitted `password_policy`).

These are not ten unrelated defects. They are **one architecture leaking in ten
places**: the same operation is implemented three-to-four times — once per
surface — and each copy re-does request parsing, authentication, input
coercion, the service call, response mapping, and error mapping. Keeping the
copies in step is currently a *manual, recurring audit*. The audits work, but
they are a tax we pay forever.

Concretely, `create / update / delete / find / find_by_id / versions` each exist
in:

| Surface | Location |
| --- | --- |
| gRPC | `src/api/handlers/**` |
| MCP | `src/mcp/tools/**` |
| Admin (HTTP/Handlebars) | `src/admin/handlers/**` |
| Lua CRUD | `src/hooks/lua_api/crud/**` |

## 2. The insight: the core already (nearly) exists

This is **not a green-field rewrite.** The service layer is already the
operation core:

- Every operation is already `fn op(ctx: &ServiceContext, input: SomeInput)`
  (`src/service/**`).
- The canonical inputs already exist as typed structs: `WriteInput`,
  `FindDocumentsInput`, `FindByIdInput`, `CountDocumentsInput`,
  `SearchDocumentsInput`, `GetGlobalInput`, `ListVersionsInput`
  (`src/service/types/**`).

What is missing is **discipline at the boundary**. Two things blur it:

1. **`ServiceContext` conflates two lifetimes of thing.** It has 19 fields (18
   optional) and 42 builder methods (`src/service/context.rs`), mixing
   startup-stable *infrastructure* (pool, runner, cache, event/invalidation
   transports, email context, password policy, locale config) with *per-operation*
   state (slug, def, user, flags, queues). The field docs literally describe
   defensive fallbacks — "a context that forgets to thread it degrades to the
   default." That sentence is the bug class in prose.

2. **Some `Input` structs leak infra too.** `FindByIdInput` carries `registry`,
   `cache`, and `singleflight` right next to `id` and `depth`
   (`src/service/types/find_by_id_input.rs`). So the same conflation repeats.

3. **Each surface re-implements the glue around the core.** gRPC bundles a
   bespoke `*BlockingInput` per op (15+ fields, re-carrying all the infra),
   resolves auth itself (`resolve_auth_user`), and repeats a `spawn_blocking`
   tail and error-mapping at every call site. MCP repeats arg-parsing + lookup.
   Admin repeats form-decode + auth. The recurring chokepoint work has been
   deduplicating *fragments* of this glue; the glue itself is the thing to
   remove.

The target is therefore a **consolidation toward an existing core**, which is a
much lower-risk program than "rewrite."

## 3. Target architecture

Three moving parts.

### 3.1 `AppInfra` — immutable, built once

One struct holding everything that is stable for the process lifetime:

```rust
pub struct AppInfra {
    pub pool: DbPool,
    pub runner: HookRunner,
    pub registry: Arc<Registry>,
    pub cache: SharedCache,
    pub storage: SharedStorage,
    pub event_transport: Option<SharedEventTransport>,
    pub invalidation_transport: Option<SharedInvalidationTransport>,
    pub token_provider: SharedTokenProvider,
    pub email: EmailContext,
    pub locale_config: LocaleConfig,
    pub password_policy: PasswordPolicy,
    // …every "constructed at boot, never varies per call" dependency
}
```

Built once in `main`/server setup, shared by `&AppInfra` (or `Arc<AppInfra>`).
Because it is one value, **no operation can forget to forward a field** — the
entire "degrades to default because a context was built by hand" class is gone.

### 3.2 `OpContext` — the per-call envelope

```rust
pub struct OpContext<'a> {
    pub infra: &'a AppInfra,
    pub actor: Actor<'a>,          // resolved auth: user doc, override flag, ui_locale
    pub target: Target<'a>,        // slug + Def (collection|global)
    pub locale: Option<LocaleContext>,
    pub flags: OpFlags,            // draft, include_deleted, emit_events, …
    pub tx: Option<&'a dyn DbConnection>, // set when running inside a hook tx (Lua path)
}
```

`ServiceContext` collapses into `OpContext`. The `Input` structs shed their
infra fields (registry/cache/singleflight move into `AppInfra`) and keep only
genuine per-call data (`id`, `depth`, `select`, `data`, `password`, …).

### 3.3 The operation registry + one dispatch entry

Each operation is declared **once** with: its canonical `Input`/`Output`, its
auth policy, and its handler (the existing `service::*` fn). Surfaces stop
carrying bespoke glue and instead call one generic entry:

```rust
// Conceptual — the single funnel every surface shares.
pub async fn run<O: Operation>(
    infra: &AppInfra,
    creds: Credentials<'_>,     // bearer/session/headers — surface-neutral
    target: TargetRef<'_>,      // slug (+ kind)
    input: O::Input,
) -> Result<O::Output, CoreError> {
    let actor  = auth::resolve(infra, creds, &target)?;   // one auth path
    let ctx    = OpContext::new(infra, actor, target, /*flags from input*/);
    O::run(&ctx, input)                                   // the service fn
}
```

A **surface becomes a codec**:

- decode wire → `O::Input` + `Credentials` + `TargetRef`,
- call `run::<O>(…)`,
- encode `O::Output` → wire, map `CoreError` → wire error.

Auth, validation, limit-flooring, and draft/trash/version gating live **inside**
`run`/the service op. They are structurally impossible to have "on three of four
surfaces," because there is exactly one implementation.

The blocking-offload (`spawn_blocking`) and its error tail become part of `run`
(or a single `run_blocking` it calls), not per-handler boilerplate.

## 4. Migration path (safe, staged, each independently shippable)

The point of staging is that **the tree stays green and shippable after every
stage** — no big-bang branch. Each stage is a normal PR with tests.

### Stage 0 — introduce `AppInfra`, no behavior change
- Define `AppInfra`; construct it once at boot.
- Have the existing surface constructors (gRPC `ContentService`, MCP
  `ToolExecCtx`, admin state, hook runner) hold `Arc<AppInfra>` internally and
  read fields from it. Public signatures unchanged.
- **Risk:** trivial. Pure introduction. Existing `ServiceContextBuilder` still
  works, now sourced from `AppInfra`.

### Stage 1 — split `ServiceContext` → `OpContext { infra, … }`
- Add `OpContext`. Implement `ServiceContext`'s current API as a thin shim over
  `OpContext` so existing call sites compile unchanged.
- Move infra fields **out** of the `Input` structs (`FindByIdInput.registry` /
  `.cache` / `.singleflight` → read from `infra`). Update the handful of
  service fns.
- **Risk:** medium-mechanical, but caught entirely by the compiler. Regression
  net: the existing service unit/integration tests.

### Stage 2 — one auth resolution + one dispatch entry
- Extract the single `auth::resolve(infra, creds, target) -> Actor`. gRPC's
  `resolve_auth_user`, admin's session/bearer path, and MCP's override path all
  become thin adapters that produce `Credentials`, then call the shared
  resolver.
- Introduce `run::<O>()` and `run_blocking`. Port **one** operation end-to-end
  (recommend `find_by_id` — read-only, no events/tx) across all four surfaces as
  the reference conversion.
- **Risk:** medium. The reference op is the design's proof; review it hard
  before templating the rest.

### Stage 3 — port operations surface-by-surface
- Convert the remaining ops (`find`, `create`, `update`, `delete`, `versions`,
  bulk) to `run::<O>()`. Each conversion **deletes** a `*BlockingInput`, a
  bespoke auth call, and a `spawn_blocking` tail.
- Do it one operation at a time, all surfaces at once per op, so parity is
  proven per op rather than per surface.
- **Risk:** contained per op. This is where the audit's whole backlog
  evaporates — as each op lands, its cross-surface drift becomes unrepresentable.

### Stage 4 — delete the dead glue
- Remove the now-unused per-surface preamble/tail helpers, `*BlockingInput`
  structs, and the `ServiceContext` shim from Stage 1.
- Fold the still-useful chokepoints (`floor_optional_limit`, the draft/trash
  resolvers, `normalize_email`) into the core op definitions as their canonical
  home.

## 5. What this ends

- **Cross-surface drift** — the entire category. There is one implementation, so
  "fixed in gRPC, missing in MCP" cannot be expressed.
- **The `ServiceContext` forward-a-field bug** (`inherit_write_infra`,
  `update_many` dropping `password_policy`) — infra is one immutable value.
- **Per-surface auth divergence** — one resolver, surface-specific only in how
  credentials are *read off the wire*.
- **The `spawn_blocking` / error-mapping boilerplate** — lives in `run` once.

## 6. What this explicitly does **not** touch

- **`DbConnection` / `Boxed{Connection,Transaction}`** — the SQLite/Postgres
  abstraction is clean and holds up across every audit. Untouched.
- **The relational-spine + nested-JSON storage model** (groups → flat `__`
  columns, top-level arrays/blocks/relationships → join tables, nested → JSON).
  It is a deliberate trade for SQL-filterable subfields; do **not** collapse it.
  (The separate *resolved-schema IR* proposal — compute column names / storage
  strategy once and have migration/ref-count/validation/access/typegen/FTS read
  it instead of re-walking — is complementary and can land independently.)
- **The access model** (independent `read`/`draft`/`trash`/`versions` keys),
  already redesigned and hardened through audit rounds 5–17. Mature.

## 7. Success criteria

- Every CRUD/version operation has exactly **one** implementation; surfaces
  contain only wire codec + error render.
- `AppInfra` is the only holder of process-stable dependencies; no `Input` or
  context struct carries infra.
- A new operation is added by declaring one `Operation` + N codec adapters —
  never by copying a handler.
- The recurring "harmonize the surfaces" audit round is **retired**: the
  property it verified is now enforced by construction.
