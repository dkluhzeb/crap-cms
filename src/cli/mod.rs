//! CLI output primitives: colors, themed prompts, tables, spinners.
//!
//! All three rendering surfaces (`output`, `spinner`, `theme`) share
//! the [`glyphs`] module for `✓ / ⚠ / ✗ / →` selection — same
//! Unicode/ASCII fallback story everywhere, honouring
//! `CRAP_NO_UNICODE=1` / `CRAP_FORCE_UNICODE=1` consistently.

mod glyphs;
pub mod output;
pub mod spinner;
pub mod table;
pub mod theme;

pub use output::{dim, error, header, hint, info, kv, kv_status, step, success, warning};
pub use spinner::Spinner;
pub use table::Table;
pub use theme::crap_theme;
