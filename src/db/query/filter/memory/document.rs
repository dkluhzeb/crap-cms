//! The row a [`Document`] is judged as by the in-memory evaluator.
//!
//! A `Document` keeps `id` and the `created_at` / `updated_at` timestamps
//! outside its field map, while a row constraint names them like any other
//! column (`{ id = user.id }`). Every caller that judges a document — populated
//! relationship targets, draft snapshots, live-event gate snapshots — builds the
//! evaluator's input here, so none of them can hand the evaluator a row missing
//! a column SQL would see (which makes the constraint fail closed where SQL
//! passes the row).
//!
//! The remaining system columns a constraint may reference (`_status` for the
//! injected published-only rule, `_deleted_at` for the trash view) are already
//! in the field map of a row read from the database.

use serde_json::Value;

use crate::{
    core::{Document, DocumentFields, FieldDefinition, flatten_group_fields},
    db::{FilterClause, query::filter::memory::matches_flat},
};

/// The document's stored row as the evaluator reads it: its fields plus the
/// `id` and timestamp columns. A timestamp the document does not carry is left
/// absent rather than invented, so a constraint on it fails closed.
#[must_use]
pub fn constraint_row(doc: &Document) -> DocumentFields {
    let mut row = doc.fields.clone();

    insert_row_columns(&mut row, doc);

    row
}

/// Whether `doc` satisfies `constraints` (AND), judged on its full row — the
/// in-memory counterpart of the SQL `WHERE` a read applies. `fields` is the
/// owning definition's field list. Returns `true` for empty constraints.
#[must_use]
pub fn matches_document(
    doc: &Document,
    constraints: &[FilterClause],
    fields: &[FieldDefinition],
) -> bool {
    if constraints.is_empty() {
        return true;
    }

    let mut flat = flatten_group_fields(&doc.fields, fields);

    insert_row_columns(&mut flat, doc);

    matches_flat(&flat, constraints, fields)
}

/// Add the columns a `Document` keeps outside its field map. The document's
/// own `id` wins over any `id` key the field map happens to carry.
fn insert_row_columns(row: &mut DocumentFields, doc: &Document) {
    row.insert("id".to_string(), Value::String(doc.id.to_string()));

    let timestamps = [
        ("created_at", &doc.created_at),
        ("updated_at", &doc.updated_at),
    ];

    for (key, value) in timestamps {
        let Some(value) = value else { continue };

        row.insert(key.to_string(), Value::String(value.clone()));
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        core::FieldType,
        db::{Filter, FilterOp, query::filter::memory::matches_constraints_typed},
    };

    fn clause(field: &str, op: FilterOp) -> Vec<FilterClause> {
        vec![FilterClause::Single(Filter {
            field: field.to_string(),
            op,
        })]
    }

    fn doc() -> Document {
        let mut doc = Document::new("doc-1");
        doc.fields.insert("owner".into(), json!("u1"));
        doc.fields.insert("seo".into(), json!({ "author": "u1" }));
        doc.created_at = Some("2026-01-01T00:00:00Z".into());
        doc
    }

    fn seo_fields() -> Vec<FieldDefinition> {
        vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("author", FieldType::Text).build(),
                ])
                .build(),
        ]
    }

    /// A constraint on `id` sees the document's id, which lives outside its
    /// field map — without it the constraint hid every row.
    #[test]
    fn an_id_constraint_sees_the_document_id() {
        let doc = doc();

        assert!(matches_document(
            &doc,
            &clause("id", FilterOp::Equals("doc-1".into())),
            &[]
        ));
        assert!(matches_document(
            &doc,
            &clause("id", FilterOp::In(vec!["x".into(), "doc-1".into()])),
            &[]
        ));
        assert!(!matches_document(
            &doc,
            &clause("id", FilterOp::Equals("other".into())),
            &[]
        ));
        assert!(!matches_document(
            &doc,
            &clause("id", FilterOp::NotEquals("doc-1".into())),
            &[]
        ));
    }

    /// Timestamps are judged as columns; an absent one is not invented.
    #[test]
    fn timestamp_constraints_see_the_document_timestamps() {
        let doc = doc();

        assert!(matches_document(
            &doc,
            &clause("created_at", FilterOp::Exists),
            &[]
        ));
        assert!(matches_document(
            &doc,
            &clause(
                "created_at",
                FilterOp::Equals("2026-01-01T00:00:00Z".into())
            ),
            &[]
        ));
        assert!(!matches_document(
            &doc,
            &clause("updated_at", FilterOp::Exists),
            &[]
        ));
    }

    /// The document's own id is authoritative over a stray `id` field key.
    #[test]
    fn the_document_id_wins_over_a_field_key() {
        let mut doc = doc();
        doc.fields.insert("id".into(), json!("spoofed"));

        assert!(matches_document(
            &doc,
            &clause("id", FilterOp::Equals("doc-1".into())),
            &[]
        ));
        assert_eq!(constraint_row(&doc).get("id"), Some(&json!("doc-1")));
    }

    /// Group sub-fields are still matched by their flat path, and the snapshot
    /// form judges exactly as the direct form does.
    #[test]
    fn group_paths_and_the_snapshot_form_agree() {
        let doc = doc();
        let fields = seo_fields();

        for (constraint, expected) in [
            (clause("seo__author", FilterOp::Equals("u1".into())), true),
            (clause("seo__author", FilterOp::Equals("u2".into())), false),
            (clause("id", FilterOp::Equals("doc-1".into())), true),
        ] {
            assert_eq!(matches_document(&doc, &constraint, &fields), expected);
            assert_eq!(
                matches_constraints_typed(&constraint_row(&doc), &constraint, &fields),
                expected
            );
        }
    }

    #[test]
    fn empty_constraints_match() {
        assert!(matches_document(&Document::new("x"), &[], &[]));
    }
}
