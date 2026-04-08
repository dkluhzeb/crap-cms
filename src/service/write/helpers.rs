//! Shared helpers for write operations.

use std::{borrow::Cow, collections::HashMap};

use serde_json::Value;

/// Strip denied field names from flat data and return a (potentially cloned) join_data map
/// with denied keys removed. If no fields are denied, returns the original join_data unchanged.
pub(crate) fn strip_denied_fields<'a>(
    denied: &[String],
    data: &mut HashMap<String, String>,
    join_data: &'a HashMap<String, Value>,
) -> Cow<'a, HashMap<String, Value>> {
    if denied.is_empty() {
        return Cow::Borrowed(join_data);
    }

    for name in denied {
        data.remove(name);
    }

    let mut filtered = join_data.clone();
    for name in denied {
        filtered.remove(name);
    }
    Cow::Owned(filtered)
}
