//! Field-type catalogues used by the parser, writer, and wizard.

use crate::core::FieldType;

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

/// Whether a field of `field_type` holds a value of its own, so `required`
/// and `localized` can apply to it. A layout wrapper (its children sit at its
/// level) and a join (a virtual list) hold none, and the schema loader refuses
/// both flags on them.
pub fn holds_value(field_type: &str) -> bool {
    let ft = FieldType::parse_lossy(field_type);

    !ft.is_layout_wrapper() && ft != FieldType::Join
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_types_are_a_subset_of_valid_types() {
        for c in CONTAINER_TYPES {
            assert!(
                VALID_FIELD_TYPES.contains(c),
                "container '{c}' missing from VALID_FIELD_TYPES"
            );
        }
    }

    /// The wrappers and the join hold no value; every other type does.
    #[test]
    fn only_wrappers_and_joins_hold_no_value() {
        let valueless: Vec<&str> = VALID_FIELD_TYPES
            .iter()
            .copied()
            .filter(|t| !holds_value(t))
            .collect();

        assert_eq!(valueless, ["row", "collapsible", "tabs", "join"]);
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
