//! Shared helpers for write operations.

use crate::core::DocumentFields;

/// Strip denied field names from the unified write-data map.
pub(crate) fn strip_denied_fields(denied: &[String], data: &mut DocumentFields) {
    for name in denied {
        data.remove(name);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn data() -> DocumentFields {
        let mut d = DocumentFields::new();
        d.insert("title".into(), json!("hi"));
        d.insert("secret".into(), json!("x"));
        d.insert("body".into(), json!("text"));
        d
    }

    #[test]
    fn removes_denied_and_keeps_the_rest() {
        let mut d = data();
        strip_denied_fields(&["secret".to_string()], &mut d);

        assert!(!d.contains_key("secret"));
        assert!(d.contains_key("title"));
        assert!(d.contains_key("body"));
    }

    #[test]
    fn denied_name_not_present_is_a_no_op() {
        let mut d = data();
        strip_denied_fields(&["nonexistent".to_string()], &mut d);
        assert_eq!(d.len(), 3);
    }

    #[test]
    fn empty_denied_list_changes_nothing() {
        let mut d = data();
        strip_denied_fields(&[], &mut d);
        assert_eq!(d.len(), 3);
    }
}
