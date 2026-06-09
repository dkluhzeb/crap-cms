//! Shared helpers for write operations.

use crate::core::{DocumentFields, FieldDenial};

/// Strip denied fields from the unified write-data map — flat columns, group
/// subfields, and fields nested inside array/blocks rows at any depth.
pub(crate) fn strip_denied_fields(denied: &[FieldDenial], data: &mut DocumentFields) {
    for denial in denied {
        denial.strip_from(data);
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
        strip_denied_fields(&["secret".into()], &mut d);

        assert!(!d.contains_key("secret"));
        assert!(d.contains_key("title"));
        assert!(d.contains_key("body"));
    }

    #[test]
    fn denied_name_not_present_is_a_no_op() {
        let mut d = data();
        strip_denied_fields(&["nonexistent".into()], &mut d);
        assert_eq!(d.len(), 3);
    }

    #[test]
    fn empty_denied_list_changes_nothing() {
        let mut d = data();
        strip_denied_fields(&[], &mut d);
        assert_eq!(d.len(), 3);
    }

    #[test]
    fn strips_field_from_every_array_row() {
        let mut d = DocumentFields::new();
        d.insert(
            "items".into(),
            json!([{ "name": "a", "secret": "1" }, { "name": "b", "secret": "2" }]),
        );

        strip_denied_fields(
            &[FieldDenial::Nested {
                array_key: "items".into(),
                array_block_type: None,
                row_path: vec![],
                leaf: "secret".into(),
            }],
            &mut d,
        );

        assert_eq!(
            d.get("items"),
            Some(&json!([{ "name": "a" }, { "name": "b" }]))
        );
    }
}
