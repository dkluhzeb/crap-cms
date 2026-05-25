//! Recursive validation: layout dispatch + scalar field validation.
//!
//! `ValidationWalker` bundles the per-walk invariants (Lua VM, data, validation
//! context) so the recursive helpers stay at ≤ 4 args + receiver instead of
//! 7 positional args. The walker dispatches on `FieldType`: layout containers
//! recurse, scalar fields go through the `scalar` impl method.

mod dispatch;
mod scalar;

pub(super) use dispatch::ValidationWalker;
