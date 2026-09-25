//! Build a single typed `FieldContext` for template rendering: the entry
//! point and base data (`entry`), and the per-variant constructors for
//! scalar, reference, and composite fields.

mod composites;
mod entry;
mod references;
mod scalars;

pub use entry::build_single_field_context;
