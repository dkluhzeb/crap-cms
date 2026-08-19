//! Client SDK type-definition generators (TypeScript, Go, Python, Rust),
//! built on a shared schema walk feeding a per-language [`ClientPrinter`].
//!
//! ## Why a printer trait
//!
//! The four client generators all answer the same structural questions —
//! which collections/globals become types, which Array/Group fields become
//! named sub-types, how each field maps to a language type, what's optional —
//! and differ only in *syntax*. The old design re-implemented that walk four
//! times (`render_collection`/`render_global`/`write_field`/`field_to_*` in
//! each file), so a fix to the shared logic had to land in four places.
//!
//! Here the walk lives once in [`generate`], which resolves every field to a
//! language-neutral [`FieldTy`] and streams [`SubType`]/[`Document`] constructs
//! to a [`ClientPrinter`]. Each language is one focused impl of "render *this
//! construct*", using a [`writer::CodeWriter`] so an unbalanced brace is
//! structurally impossible, and [`super::idents`] for all naming/escaping.
//!
//! Lua is deliberately *not* here: it emits `LuaLS` annotations + runtime table
//! stubs, a different output shape, and keeps its own generator (`lua/`).
//!
//! ## Assumed-valid registry
//!
//! The walk assumes a complete registry: a relationship/upload whose target
//! collection isn't registered would emit a dangling type reference (e.g.
//! `Rel<Foo>` with no `Foo`). That's a schema error and should be rejected by
//! registry-level validation (a dangling relationship target), not papered over
//! here — silently degrading it to an id string would hide the bug. **TODO
//! (registry module):** reject relationships/uploads targeting unknown
//! collections at load. Same-name type collisions *are* caught here, at
//! generation time (see `driver::check_type_name_collisions`).

mod driver;
mod go;
mod ir;
mod python;
mod rust;
mod typescript;
mod writer;

pub(super) use driver::generate;
// `resolve_ty` + `FieldTy` are shared with `rust_proto` so both Rust generators
// agree on every field's type (see `driver::resolve_ty`).
pub(super) use driver::resolve_ty;
pub(super) use ir::{ClientPrinter, Document, EnumDef, Field, FieldTy, PolyDef, SubType};

// `drive` renders through a caller-chosen printer; production selects the
// printer by language via [`generate`], so only the per-language tests reach
// for it directly.
#[cfg(test)]
pub(super) use driver::drive;
