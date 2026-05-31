//! Field-type catalogues used by the parser, writer, and wizard.

/// Valid field types for collection definitions.
pub const VALID_FIELD_TYPES: &[&str] = &[
    "text",
    "number",
    "textarea",
    "select",
    "radio",
    "checkbox",
    "date",
    "email",
    "json",
    "richtext",
    "code",
    "relationship",
    "array",
    "group",
    "upload",
    "blocks",
    "row",
    "collapsible",
    "tabs",
    "join",
];

/// Container field types that support nested subfields.
pub const CONTAINER_TYPES: &[&str] = &["group", "array", "row", "collapsible"];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::FieldType;

    #[test]
    fn container_types_are_a_subset_of_valid_types() {
        for c in CONTAINER_TYPES {
            assert!(
                VALID_FIELD_TYPES.contains(c),
                "container '{c}' missing from VALID_FIELD_TYPES"
            );
        }
    }

    #[test]
    fn every_valid_type_string_round_trips_through_field_type() {
        // A typo'd entry (e.g. "numbr") would parse_lossy to Text, whose
        // as_str ("text") wouldn't match the entry — caught here.
        for &t in VALID_FIELD_TYPES {
            assert_eq!(
                FieldType::parse_lossy(t).as_str(),
                t,
                "field-type string '{t}' does not round-trip"
            );
        }
    }
}
