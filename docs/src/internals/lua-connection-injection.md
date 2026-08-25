# Lua Connection Injection — Hardening Plan (optional)

> **Status: proposal, lower priority.** The current design is correct,
> guarded, and tested. This plan is about *shrinking the unsafe surface* and
> the ambient-state complexity, not fixing a known bug. Land it only if/when
> the seam is being touched anyway — ideally alongside the
> [Operation Core](operation-core-migration.md) refactor, with which it
> composes.

## 1. What exists today

Lua CRUD functions (`crap.collections.create`, …) need the **active database
connection** for the current call. They don't receive it as a parameter — they
read it from ambient per-VM state:

- The hook runner injects a borrowed `&dyn DbConnection` into the VM as
  `app_data` before running before-event hooks (`run_hooks_with_conn`), and Lua
  CRUD reads it back via `get_tx_conn(lua)` / `with_lua_db`.
- Because mlua requires `set_app_data<T>` where **`T: 'static`**, and the
  connection is a *borrow* with a shorter lifetime, the borrow is smuggled
  through as raw words:

```rust
// src/hooks/lifecycle/types.rs
pub(crate) fn new(conn: &dyn DbConnection) -> Self {
    let fat_ptr: *const dyn DbConnection = conn;
    // transmute the (data_ptr, vtable_ptr) fat pointer to [usize; 2]
    let [data, vtable]: [usize; 2] = unsafe { std::mem::transmute(fat_ptr) };
    Self { data, vtable }
}
pub(crate) fn as_ptr(&self) -> *const dyn DbConnection {
    unsafe { std::mem::transmute([self.data, self.vtable]) }
}
// + unsafe impl Send/Sync for TxContext
// get_tx_conn: Ok(unsafe { &*ptr })   // lifetime tied to &Lua, not to the tx
```

Two dispatch modes ride on the same channel (`with_lua_db`,
`src/hooks/lua_api/crud/tx_conn.rs`):

- **conn-mode** (hooks, `crap.transaction(fn)`): one shared `TxContext` for the
  whole call.
- **pool-mode** (job handlers): a `PoolContext` (a `DbPool`) is set instead;
  each CRUD op opens its own short-lived `IMMEDIATE` tx and *temporarily*
  installs it as a `TxContext`.

Alongside the connection, other ambient values live in the same per-VM
app_data namespace: `PoolContext`, `HookDepth`, `UserContext`, `VmLabel`,
`InitPhase`.

## 2. Why it's worth hardening

Nothing here is *broken* — but this is the codebase's most soundness-sensitive
code, and its correctness rests on invariants the compiler does **not** check:

1. **Lifetime laundering.** `get_tx_conn` hands out `&'a dyn DbConnection` with
   `'a` tied to `&'a Lua`, not to the real transaction. Soundness depends
   entirely on `TxContextGuard` removing the app_data before the tx drops, and
   on no caller stashing the borrow past the call. A future refactor that holds
   the reference a little too long is silent UB, not a compile error.
2. **Layout assumption.** `transmute::<*const dyn Trait, [usize; 2]>` assumes the
   `(data, vtable)` two-word fat-pointer layout. True on all supported targets
   today, but **not** a guaranteed ABI — a `transmute` the compiler can't
   validate.
3. **Type-keyed ambient state.** app_data is keyed by type, so the injected
   connection is effectively a dynamically-scoped mutable global. It works, but
   it's "spooky action at a distance": CRUD reads a connection nobody passed it,
   and the conn/pool-mode swap is bookkeeping on a shared key.

## 3. The root constraint (name it, so options are honest)

mlua closures registered on a VM are `'static` and cannot borrow a per-call
connection; `set_app_data` compounds this with `T: 'static`. **You cannot hand a
borrowed `&tx` to a Lua-invoked Rust closure by normal means.** Every option
below is a different way to live with that one fact. Anything that claims to be
"just safe" is really *relocating* the unsafe into an audited primitive or
*changing what is shared* so no borrow needs smuggling.

## 4. Options

### Option A — Encapsulate the borrow in a scoped primitive (smallest diff)
Replace the hand-rolled transmute with a **scoped-reference guard** whose public
API makes the borrow non-escapable — the pattern the `scoped-tls` crate
implements, or a ~30-line purpose-built `ScopedConn` (a thread-local set for the
dynamic extent of a closure):

```rust
scoped.set(conn, || {
    // Lua runs here; get_tx_conn = scoped.with(|c| …)
});   // borrow provably cannot outlive this call
```

- **Unsafe:** still exists *internally*, but concentrated in **one** audited
  primitive with a sound, non-escapable safe API — instead of transmutes spread
  across `types.rs` + `tx_conn.rs` and an `unsafe impl Send/Sync`.
- **Fat-pointer layout assumption:** gone (a scoped guard stores a real typed
  reference, not two `usize`s).
- **Breaking:** none — the Lua API is unchanged; `get_tx_conn`/`with_lua_db`
  keep their signatures.
- **Cost:** small, contained; existing hook/job tests are the regression net.

### Option B — Share an owned handle, zero `unsafe` (composes with Operation Core)
Make the active connection an **owned, shareable** handle rather than a borrow:
`Arc<dyn DbConnection>` for pooled ops, and for the shared-tx case an
`Arc<Mutex<BoxedTransaction>>` (or an enum over the two). It goes into app_data /
`OpContext` as a genuinely `'static` value — no transmute, no lifetime
laundering, no `unsafe impl`.

- **Unsafe:** **eliminated** from this seam.
- **Cost:** higher — the write path (service layer) currently *owns* the tx on
  its stack and lends a borrow to `run_hooks_with_conn`. This asks that path to
  hold the tx behind the shared handle instead. That is exactly the kind of
  ownership move the [Operation Core](operation-core-migration.md) `OpContext`
  already introduces (`tx: Option<…>`), so **do B as part of that refactor**, not
  standalone.
- **Watch:** `Mutex` on the shared tx must not deadlock under nested hooks; the
  existing `HookDepth` guard and conn-mode passthrough already serialize access,
  so the lock is uncontended-by-construction, but assert it in tests.

### Option C — Explicit connection handle in the Lua API (rejected)
Give Lua an explicit `db` handle object (userdata) so CRUD is `db:create(...)`
instead of ambient `crap.collections.create(...)`. Removes ambient state
entirely — but **breaks the public Lua API** and pushes tx-plumbing onto every
hook author. Against the no-breaking-changes commitment. Recorded and declined.

## 5. Recommendation

- **If hardening in isolation:** do **Option A**. Minimal diff, non-breaking,
  and it deletes the layout-assuming transmute and the `unsafe impl Send/Sync`
  while keeping the ambient ergonomics hook authors rely on.
- **If the Operation Core refactor is happening:** do **Option B** as part of
  it — the `OpContext` owned-connection handle removes the unsafe here for free
  and unifies "how an op gets its connection" across Rust surfaces and Lua.

Either way, fold the scattered ambient keys (`TxContext`/`PoolContext` +
`HookDepth` + `UserContext`) behind **one** typed `CallScope` set once per hook
invocation, so there is a single set/clear point and no type-keyed clobber risk.

## 6. What **not** to change

- **The conn-mode / pool-mode distinction is inherent** — hooks must join the
  caller's transaction (atomicity with the outer write); job handlers must open
  per-op `IMMEDIATE` transactions to avoid the `SQLITE_BUSY_SNAPSHOT` hazard.
  Whatever carries the connection, keep both modes.
- **The pooled-VM model** (`VmPool`, `vm_pool_size`) and the init-VM/pool-VM
  split are orthogonal to this seam; leave them unless separately motivated.
- **`LocalLease` / `LuaVmLease`** (the core↔hooks seam for custom
  email/storage providers) is already a clean, safe abstraction (weak handle,
  no transmute). Untouched.
