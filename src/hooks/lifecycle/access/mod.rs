//! Access control checks executed within the Lua VM. Collection-level checks
//! run a single hook and may return `Allowed`, `Denied`, or `Constrained` (a
//! WHERE-filter table). Field-level checks recurse through the field tree to
//! produce the list of denied field names.

mod collection;
mod field;
#[cfg(test)]
mod test_helpers;

pub(crate) use collection::check_access_with_lua;
pub(crate) use field::{
    check_field_read_access_with_lua, check_field_write_access_with_lua, has_any_field_access,
};
