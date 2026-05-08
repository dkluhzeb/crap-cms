//! Shared helpers for write operations.

use crate::core::DocumentFields;

/// Strip denied field names from the unified write-data map.
pub(crate) fn strip_denied_fields(denied: &[String], data: &mut DocumentFields) {
    for name in denied {
        data.remove(name);
    }
}
