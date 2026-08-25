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

1. **VM-acquire ceiling.** `VmPool` is a `Mutex<Vec<Lua>> + Condvar`; concurrency
   above `vm_pool_size` **blocks up to 5s** (`src/hooks/lifecycle/runner/vm_pool.rs`).
   A direct contributor to the conc-50 collapse *whenever hooks are on*.
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

**A3 — Thread-local VM affinity. (internal, non-breaking)**
Hook work already runs on `spawn_blocking` threads. Pin **one Lua VM per
blocking thread** instead of a shared `Mutex<Vec<Lua>>`.
- *Payoff:* removes the pool lock, the condvar wait, and the `vm_pool_size`
  ceiling (it becomes = blocking-thread count, i.e. the real concurrency).
- *Bonus:* makes the connection injection naturally thread-scoped — this is what
  makes **Option A/B** of the
  [connection-injection plan](lua-connection-injection.md) clean.

**A4 — Compile a per-collection hook plan at registration. (internal)**
Precompute per `(collection × event)`: any hooks? which fields have field-hooks?
Store as flags on the resolved schema. Runtime does one branch — "skip / run
batch" — and for field-level events marshals **only** fields that actually have a
field-hook, never the whole document.

---

## Part B — the read pipeline

**B1 — Read/write connection-pool split. (near non-breaking; highest system ROI)**
SQLite WAL allows **unlimited readers + one writer**, but reads and writes
currently contend for one pool, so a writer holding the lock starves readers of
connections — the conc-50 collapse. Split into:
- a **large read pool** (deferred / no-transaction simple reads), and
- a **small write pool** (~4 connections, `transaction_immediate()`).

Add a mixed-workload load scenario (concurrent find + update) — the current
single-scenario test does not capture this. (Design note already tracked
separately; this is its home.)

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

1. **B1 (pool split)** — highest ROI, lowest blast radius. Ship first; it needs
   no API change and fixes the concurrency cliff for *all* workloads.
2. **A3 (thread-local VMs)** — internal; removes the hook-path acquire ceiling
   and unblocks the connection-injection hardening.
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
