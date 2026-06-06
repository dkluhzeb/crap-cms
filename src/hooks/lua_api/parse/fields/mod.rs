//! Parse Lua field-definition tables into `FieldDefinition` values. Three steps:
//!
//! 1. **constraints** — parse `min`/`max`/`min_length`/`max_length`/`min_rows`/
//!    `max_rows` numeric and length bounds, default values, and date config
//! 2. **single** — orchestrate the per-field parser (name, type, admin, access,
//!    hooks, sub-fields, blocks, tabs, relationship)
//! 3. **top** — drive `parse_single_field` over the field array and reject
//!    duplicate field names

mod constraints;
mod single;
mod top;

pub(crate) use single::parse_required_locales;
pub(crate) use top::parse_fields;
