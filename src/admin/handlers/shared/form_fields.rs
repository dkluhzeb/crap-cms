//! What the admin form renders — one answer for the builder that lays the form
//! out and for the submit-side normalizers that read a missing key as an edit.

use crate::core::{FieldDefinition, prefixed_name, walk_leaf_fields_where};

/// Whether the admin form renders `field` as an input the editor can change.
///
/// A `hidden` field is stripped from every read, so there is no value to show;
/// an `admin.hidden` one is deliberately kept out of the form and keeps its
/// stored value. The form builder and the normalizers that turn an absent key
/// into an explicit value both ask this, so "what the form rendered" and "what
/// absence means" cannot disagree: a key the form never rendered is not an edit.
pub(crate) fn renders_in_admin_form(field: &FieldDefinition) -> bool {
    !field.hidden && !field.admin.hidden
}

/// The sub-fields of a composite the admin form renders, in definition order.
///
/// Every pass that turns a definition list into field contexts — the builder,
/// the enrichment phase, the display-condition walk — and every pass that zips
/// contexts back against their definitions filters here. One filter for both
/// sides keeps the two lists pairing the same entries at every depth, and keeps
/// a hidden field out of the form at every depth: the normalizers read a key the
/// form never rendered as "not an edit", so a rendered hidden input would let the
/// editor clear a checkbox that then silently keeps its stored value.
pub(crate) fn admin_form_fields(
    fields: &[FieldDefinition],
) -> impl Iterator<Item = &FieldDefinition> {
    fields.iter().filter(|f| renders_in_admin_form(f))
}

/// Visit every leaf column the admin form renders, with its flat
/// (`group__sub`) column name. A container the form does not render is not
/// descended into, so a leaf inside a hidden group is never visited.
pub(crate) fn for_each_admin_form_leaf<F>(fields: &[FieldDefinition], mut visit: F)
where
    F: FnMut(&FieldDefinition, String),
{
    let _ = walk_leaf_fields_where(
        fields,
        "",
        false,
        &renders_in_admin_form,
        &mut |field, prefix, _| {
            visit(field, prefixed_name(prefix, &field.name));

            Ok(())
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{FieldAdmin, FieldType};

    fn field(name: &str, ft: FieldType) -> FieldDefinition {
        FieldDefinition::builder(name, ft).build()
    }

    fn admin_hidden(name: &str, ft: FieldType) -> FieldDefinition {
        FieldDefinition::builder(name, ft)
            .admin(FieldAdmin::builder().hidden(true).build())
            .build()
    }

    fn leaf_columns(fields: &[FieldDefinition]) -> Vec<String> {
        let mut seen = Vec::new();
        for_each_admin_form_leaf(fields, |_, column| seen.push(column));

        seen
    }

    #[test]
    fn a_hidden_field_is_not_rendered() {
        assert!(renders_in_admin_form(&field("title", FieldType::Text)));
        assert!(!renders_in_admin_form(&admin_hidden(
            "url",
            FieldType::Text
        )));
        assert!(!renders_in_admin_form(
            &FieldDefinition::builder("secret", FieldType::Text)
                .hidden(true)
                .build()
        ));
    }

    /// The walk prefixes group columns and stops at a hidden container: a leaf
    /// inside one is never rendered, so its absence from a submission is not an
    /// edit.
    #[test]
    fn the_walk_skips_hidden_fields_and_hidden_groups() {
        let fields = vec![
            field("title", FieldType::Text),
            admin_hidden("url", FieldType::Text),
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![
                    field("flag", FieldType::Checkbox),
                    admin_hidden("internal", FieldType::Checkbox),
                ])
                .build(),
            FieldDefinition::builder("system", FieldType::Group)
                .admin(FieldAdmin::builder().hidden(true).build())
                .fields(vec![field("flag", FieldType::Checkbox)])
                .build(),
        ];

        assert_eq!(leaf_columns(&fields), vec!["title", "meta__flag"]);
    }

    /// The sub-field filter keeps definition order and drops both flavors of
    /// hidden, so a builder and the pass that zips its output back against the
    /// defs see the same entries.
    #[test]
    fn the_sub_field_filter_keeps_order_and_drops_hidden() {
        let fields = vec![
            field("author", FieldType::Text),
            admin_hidden("internal", FieldType::Checkbox),
            FieldDefinition::builder("secret", FieldType::Text)
                .hidden(true)
                .build(),
            field("note", FieldType::Text),
        ];

        let kept: Vec<&str> = admin_form_fields(&fields)
            .map(|f| f.name.as_str())
            .collect();

        assert_eq!(kept, vec!["author", "note"]);
    }

    /// Layout wrappers stay transparent — they add no column segment, and a
    /// leaf inside one is rendered like any other.
    #[test]
    fn the_walk_sees_through_layout_wrappers() {
        let fields = vec![
            FieldDefinition::builder("row", FieldType::Row)
                .fields(vec![field("featured", FieldType::Checkbox)])
                .build(),
        ];

        assert_eq!(leaf_columns(&fields), vec!["featured"]);
    }
}
