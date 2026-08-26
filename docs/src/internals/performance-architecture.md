# Performance Architecture — Plan

> **Status: proposal.** Several root causes from the March-2026 load-test
> baseline have already landed (see §2); this plans the *remaining structural
> ceilings*. It assumes **alpha's breaking-change freedom** — the single biggest
> hook-seam win requires changing the public Lua hook signature. Cross-links:
> [Operation Core](operation-core-migration.md),
> [Lua Connection Injection](lua-connection-injection.md).

## 1. Baseline: where throughput collapses

From the March-2026 load test (SQLite/WAL, single-scenario):

| Scenario | Conc | Req/s | p99 | Errors |
| --- | --- | --- | --- | --- |
| read_list | 1 | 203 | 7.4ms | 0.05% |
| read_list | 10 | 59 | 201ms | 1.69% |
| read_list | 50 | **42.9** | **2558ms** | **11.66%** |
| read_single | 50 | 61.5 | 2052ms | 8.13% |
| grpc_find (deep) | any | **~20** | — | 0% |
| grpc_write | any | ~25 | — | 0% |

Two shapes of ceiling: **read concurrency collapses** (list @ conc-50 → 11%
errors, 2.5s p99) and **deep reads floor at ~20 req/s** (N+1 population).

## 2. Already landed (do not re-litigate)

The tactical fixes from the original investigation are in:

- **Batched cross-document population + singleflight** — has-one is collected
  across the list and fetched once; concurrent cache-miss fetches dedupe
  (`src/db/query/populate/batch/**`, `SharedPopulateSingleflight`).
- **No-hook fast path** — `run_hooks` / `run_hooks_with_conn` skip VM
  acquisition entirely when a collection/event has no hooks
  (`has_registered_hooks_for`, `src/hooks/lifecycle/runner/run.rs`).
- **Prepared-statement cache** — rusqlite's cache is enabled and configurable
  (`stmt_cache_capacity`, `src/db/pool.rs`). SQL is `format!`-built with stable
  per-shape strings, so it cache-hits.
- **`pool.get()` moved into `spawn_blocking`** on the admin read paths.

What remains are **structural** ceilings, below.

## 3. Two cost centers

- **Part A — the hook / Lua-VM seam.** Cost only when a collection *has* hooks,
  but then it dominates: per-document VM work on the read hot path.
- **Part B — the read pipeline.** Connection contention, deep-read round-trips,
  and per-surface re-serialization — independent of hooks.

---

## Part A — the hook / VM seam

### A cost model (per hook-bearing request)

1. **VM-acquire ceiling.** *(Addressed by A3 — see below.)* The old fixed-size
   `Mutex<Vec<Lua>> + Condvar` pool blocked up to 5s once concurrency exceeded
   `vm_pool_size` (`src/hooks/lifecycle/runner/vm_pool.rs`) — a direct
   contributor to the conc-50 collapse whenever hooks were on. The pool is now
   elastic (grows to `max_vm_pool_size`).
2. **Eager full-document marshalling.** `after_read_one` runs **per document**
   (`for doc in docs.iter_mut()`, `src/service/read/post_process.rs`), and each
   call copies **every field** of the doc into a fresh Lua table
   (`document_to_lua_table`, `src/hooks/lifecycle/converters.rs`), FFI-crosses,
   and unmarshals back. A 20-doc × 40-field list ≈ 800 field marshals per read.
3. **FFI crossings** = O(docs × events).

### A moves, ranked

**A1 — Batch the hook contract. (breaking; the big one)**
Change per-document hooks to receive and return a **batch**: `after_read(docs)`
rather than `after_read_one(doc)` in a loop. A list read then does **one** VM
acquire, **one** FFI crossing, one marshalled array — **O(N) → O(1)**.
- *Why breaking matters:* the hook signature is public Lua API; alpha permits
  the change. This is the reason to do it now rather than never.
- *Payoff:* the biggest single win for any hook-using deployment; directly lifts
  the conc-50 read ceiling.
- *Migration:* the runtime already stores event hooks as a list per event; add a
  batch-invocation entry that marshals `Vec<Document>` once. Provide a one-release
  shim that adapts an old single-doc hook (call it per element) behind a
  deprecation warning if a softer landing is wanted.

**A2 — Lazy document proxy instead of eager table copy. (mildly breaking)**
Expose `Document` to Lua as **`UserData`** with `__index` / `__newindex`
(+ `__pairs`) so only *touched* fields cross the boundary and mutations are
tracked, instead of copying all 40 fields and diffing a rebuilt table.
- *Payoff:* large for wide documents; most hooks read 1–2 fields.
- *Risk:* `pairs()` / `type()` / `next()` semantics — cover with `__pairs` and
  tests. Composes with A1 (the batch is an array of proxies).

**A3 — Elastic VM pool (remove the fixed ceiling). (internal, non-breaking) — ✅ LANDED**
The old pool was a fixed-size `Mutex<Vec<Lua>> + Condvar`: concurrency above
`vm_pool_size` blocked up to 5s even when the machine had spare capacity. The
pool is now **elastic** — it pre-warms `vm_pool_size` VMs and grows on demand up
to `max_vm_pool_size` (a new config; default `cores × 8`, min 32), reusing
returned VMs across threads. It blocks *only* when every VM up to the cap is
checked out.
- *Payoff:* the condvar wait and the `vm_pool_size` ceiling are gone for the
  common case; VM count tracks real concurrency up to a bounded cap.
- *Design note:* strict per-thread affinity (the original framing) is
  incompatible with a hard memory cap — once `cap` threads each own a VM a new
  thread could never get one — so the cap-bounded elastic pool is the correct
  realization. It still composes with the
  [connection-injection plan](lua-connection-injection.md) Option A (a scoped
  primitive, independent of pool shape).
- *Bounding:* each VM holds the full registry/Lua state, so `max_vm_pool_size`
  is the worst-case VM-memory bound; raise the pre-warm (`vm_pool_size`) to
  avoid first-request build latency under an immediate burst.

**A4 — Compile a per-collection hook plan at registration. (internal)**
Precompute per `(collection × event)`: any hooks? which fields have field-hooks?
Store as flags on the resolved schema. Runtime does one branch — "skip / run
batch" — and for field-level events marshals **only** fields that actually have a
field-hook, never the whole document.

---

## Part B — the read pipeline

**B1 — Read/write connection-pool split. (near non-breaking; highest system ROI) — ✅ LANDED**
SQLite WAL allows **unlimited readers + one writer**, but reads and writes
previously contended for one pool, so a writer holding a connection starved
readers — the conc-50 collapse. `DbPool` now holds two backends:
- a **large read pool** (`pool_max_size`, default 64) behind `DbPool::get()`, and
- a **small write pool** (`write_pool_max_size`, default 4) behind
  `DbPool::write()`, used by every `transaction_immediate()` write path.

Routing follows the risk asymmetry — a read misrouted to the tiny write pool
would starve, but a write left on the large read pool is merely un-isolated — so
`get()` stays the safe default (read pool) and only verified writes move to
`write()`: the service create/update/delete/version ops (the chokepoint for
gRPC/MCP/admin), Lua pool-mode CRUD + `crap.transaction`, login/verify/reset, and
image-conversion enqueue. The scheduler control loop and standalone CLI commands
stay on the read pool by design (bounded by their own concurrency budget; a
size-4 write pool would throttle background job polling). Postgres keeps one
shared pool
(MVCC handles concurrent writers), so the split is SQLite-only.

`tests/grpc_loadtest.sh` gained a `mixed` scenario (concurrent Find readers +
Update writers via two parallel `ghz` runs, reported as `mixed_read` /
`mixed_write` with full percentiles) — the before/after measurement this split is
accountable to. Unit tests in `db::pool` assert the pools are independent
(exhausting the write pool does not block reads) and share one database.

**B2 — Depth-1 population via `LEFT JOIN` in the main SELECT. (internal)**
Population is batched now but still **post-query** — one round-trip per
`(collection, field)` level. For the common **depth-1** case, fold the related
row into the main `SELECT` with a `LEFT JOIN`, eliminating the extra round-trips
that floor deep `find` at ~20 req/s. Keep the post-query batch path for depth ≥ 2.

**B3 — One canonical wire encode. (breaking-friendly)**
There are ~130 per-surface serialization sites that rebuild the wire form from a
`Document` through an intermediate `serde_json::Value` (gRPC → proto, MCP/Admin →
JSON). With breaking allowed, make the **response shape the canonical shape** so
the hot read path serializes **once** (Document → wire) instead of
Document → Value → wire per surface. This lands naturally on top of the
[Operation Core](operation-core-migration.md) `Output` type — one encode per
surface, defined once.

---

## 4. Staged migration (measure each)

Each stage is a normal PR with a **before/after load-test delta** as its
acceptance evidence — never merge a perf change without the number it bought.

1. **B1 (pool split)** — ✅ **landed.** Highest ROI, lowest blast radius; no API
   change, fixes the concurrency cliff for *all* workloads. Measure the
   `mixed` scenario before/after to confirm the read half holds its
   standalone-`find` baseline under write load.
2. **A3 (elastic VM pool)** — ✅ **landed.** Internal; removes the hook-path
   acquire ceiling (grows to `max_vm_pool_size` instead of blocking at
   `vm_pool_size`) and unblocks the connection-injection hardening.
3. **A1 (batch hook contract)** — the breaking change; do it while alpha allows.
   Bundle A4 (hook plan) so the batch path also skips untouched work.
4. **B2 (JOIN population)** — targets the deep-read ceiling specifically.
5. **A2 (lazy proxy)** and **B3 (single encode)** — larger refactors; land on top
   of the batch contract and the Operation Core `Output` respectively.

## 5. What **not** to change

- **SQLite as the default embedded engine** — the pooling/WAL model is right; the
  wins are in *how we share connections*, not in replacing the engine.
- **`spawn_blocking` offload** — correct for a synchronous embedded DB; the fix is
  thread-local VM affinity *within* that model, not an async-SQLite rewrite.
- **The relational-spine + nested-JSON storage model** — its column layout is what
  makes B2's `LEFT JOIN` and SQL-filterable subfields possible in the first place.
- **Everything already landed in §2** — batched population, the no-hook fast path,
  the statement cache. Build on them.

## 6. Success criteria

- `read_list @ conc-50` no longer errors and holds sub-second p99 (target: B1 + A3).
- deep `find` clears the ~20 req/s floor by ~10× at depth-1 (target: B2).
- a hook-bearing list read costs **one** VM acquire and **one** FFI crossing
  regardless of document count (target: A1).
- the hot read path serializes each document **once** (target: B3).
