//! Reporting a refused reference on the fields that hold it.
//!
//! A write referencing a document it may not point at is refused while its
//! reference counts are applied (see [`UnavailableReferences`]) — after the
//! write's data became (collection, id) pairs. [`anchor_to_fields`] maps the
//! refusal back onto the write's data, so every surface reports it as a field
//! validation error on the key holding the reference, in the key form every
//! other field error takes: `seo__author` in a group, `items[0][author]` in
//! an array row, `items[0][info__author]` in a group in a row,
//! `items[0][children][1][author]` in a nested row.
//!
//! Like validation, this walk keeps its own recursion: the keys it builds
//! need the row indices the shared data walkers do not carry.

use anyhow::Error;
use serde_json::{Map, Value};

use crate::core::{
    BLOCK_TYPE_KEY, BlockDefinition, DocumentFields, FieldChildren, FieldDefinition, FieldError,
    ValidationError, field_children, flatten_group_fields, prefixed_name,
};

use super::{delta::UnavailableReferences, walk::for_each_ref};

/// The translation key of a refused reference's field error.
const REFERENCE_UNAVAILABLE_KEY: &str = "validation.reference_unavailable";

/// `err` as a field validation error when it refuses references `data` holds
/// (see the module docs); any other error, or a refusal of references `data`
/// does not hold (e.g. ones a publish brought from its pending draft), is
/// returned unchanged. `fields` are the written document's definitions.
#[must_use]
pub fn anchor_to_fields(err: Error, fields: &[FieldDefinition], data: &DocumentFields) -> Error {
    let Some(refused) = err.downcast_ref::<UnavailableReferences>() else {
        return err;
    };

    // The columns walk reads flat `group__sub` keys; the canonical write data
    // nests groups (flattening is idempotent).
    let data = flatten_group_fields(data, fields);

    let mut anchor = Anchor {
        refused,
        errors: Vec::new(),
    };
    anchor.columns(fields, &data, "");

    let errors = anchor.errors;

    if errors.is_empty() {
        return err;
    }

    ValidationError::new(errors).into()
}

/// One level inside an array/blocks row: the row's key (`items[0]`) and the
/// group prefix accumulated within the row (`info__`).
struct RowLevel {
    row: String,
    group_prefix: String,
}

impl RowLevel {
    fn new(row: String) -> Self {
        Self {
            row,
            group_prefix: String::new(),
        }
    }

    /// The level inside group `name` of this level.
    fn group(&self, name: &str) -> Self {
        Self {
            row: self.row.clone(),
            group_prefix: format!("{}{name}__", self.group_prefix),
        }
    }

    /// The error key of field `name` at this level.
    fn key(&self, name: &str) -> String {
        format!("{}[{}{name}]", self.row, self.group_prefix)
    }
}

/// The sub-fields of one row: an array's, or a block's matched by its type.
#[derive(Clone, Copy)]
enum RowFields<'a> {
    Array(&'a [FieldDefinition]),
    Blocks(&'a [BlockDefinition]),
}

impl<'a> RowFields<'a> {
    fn of(self, row: &Map<String, Value>) -> Option<&'a [FieldDefinition]> {
        match self {
            Self::Array(fields) => Some(fields),
            Self::Blocks(defs) => {
                let block_type = row.get(BLOCK_TYPE_KEY).and_then(Value::as_str)?;

                defs.iter()
                    .find(|def| def.block_type == block_type)
                    .map(|def| def.fields.as_slice())
            }
        }
    }
}

/// The walk: collects a field error for every key holding a refused reference.
struct Anchor<'r> {
    refused: &'r UnavailableReferences,
    errors: Vec<FieldError>,
}

impl Anchor<'_> {
    /// The document's own columns: groups prefix their children, layout
    /// wrappers are transparent, arrays and blocks hold rows.
    fn columns(&mut self, fields: &[FieldDefinition], data: &DocumentFields, prefix: &str) {
        for field in fields {
            let key = prefixed_name(prefix, &field.name);

            match field_children(field) {
                FieldChildren::Group(subs) => self.columns(subs, data, &key),
                FieldChildren::Wrapper(subs) => self.columns(subs, data, prefix),
                FieldChildren::Tabs(tabs) => {
                    for tab in tabs {
                        self.columns(&tab.fields, data, prefix);
                    }
                }
                FieldChildren::Array(subs) => {
                    self.rows(data.get(&key), &key, RowFields::Array(subs));
                }
                FieldChildren::Blocks(defs) => {
                    self.rows(data.get(&key), &key, RowFields::Blocks(defs));
                }
                FieldChildren::Leaf => self.leaf(field, data.get(&key), key),
            }
        }
    }

    /// Each row of an array/blocks value stored under `path`.
    fn rows(&mut self, value: Option<&Value>, path: &str, fields: RowFields<'_>) {
        let Some(Value::Array(rows)) = value else {
            return;
        };

        for (idx, row) in rows.iter().enumerate() {
            let Value::Object(obj) = row else {
                continue;
            };

            let Some(subs) = fields.of(obj) else {
                continue;
            };

            self.row(subs, obj, &RowLevel::new(format!("{path}[{idx}]")));
        }
    }

    /// One object level of a row: composites nest as JSON.
    fn row(&mut self, fields: &[FieldDefinition], obj: &Map<String, Value>, level: &RowLevel) {
        for field in fields {
            let value = obj.get(&field.name);

            match field_children(field) {
                FieldChildren::Group(subs) => {
                    if let Some(Value::Object(inner)) = value {
                        self.row(subs, inner, &level.group(&field.name));
                    }
                }
                FieldChildren::Wrapper(subs) => self.row(subs, obj, level),
                FieldChildren::Tabs(tabs) => {
                    for tab in tabs {
                        self.row(&tab.fields, obj, level);
                    }
                }
                FieldChildren::Array(subs) => {
                    self.rows(value, &level.key(&field.name), RowFields::Array(subs));
                }
                FieldChildren::Blocks(defs) => {
                    self.rows(value, &level.key(&field.name), RowFields::Blocks(defs));
                }
                FieldChildren::Leaf => self.leaf(field, value, level.key(&field.name)),
            }
        }
    }

    /// A relationship/upload value stored under `key`: an error on `key` when
    /// it holds a refused reference.
    fn leaf(&mut self, field: &FieldDefinition, value: Option<&Value>, key: String) {
        let (Some(rc), Some(value)) = (&field.relationship, value) else {
            return;
        };

        if !field.field_type.is_reference() {
            return;
        }

        let refused = self.refused;
        let mut holds_refused = false;

        for_each_ref(rc, value, &mut |collection, id, _| {
            holds_refused |=
                collection == refused.collection && refused.ids.iter().any(|r| r == id);
        });

        if !holds_refused {
            return;
        }

        self.errors.push(
            FieldError::with_key(
                key,
                format!("{} references a document that does not exist", field.name),
                REFERENCE_UNAVAILABLE_KEY,
            )
            .with_param("field", field.name.clone()),
        );
    }
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;
    use serde_json::json;

    use super::*;
    use crate::core::{FieldType, RelationshipConfig};

    fn author(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Relationship)
            .relationship(RelationshipConfig::new("users", false))
            .build()
    }

    fn container(name: &str, ty: FieldType, fields: Vec<FieldDefinition>) -> FieldDefinition {
        FieldDefinition::builder(name, ty).fields(fields).build()
    }

    fn refusal(ids: &[&str]) -> Error {
        UnavailableReferences {
            collection: "users".to_string(),
            ids: ids.iter().map(|id| (*id).to_string()).collect(),
        }
        .into()
    }

    /// The error keys `data` anchors a refusal of `ids` to.
    fn keys(fields: &[FieldDefinition], data: &Value, ids: &[&str]) -> Vec<String> {
        let data: DocumentFields = data.as_object().unwrap().clone().into_iter().collect();
        let err = anchor_to_fields(refusal(ids), fields, &data);

        let ve = err
            .downcast_ref::<ValidationError>()
            .expect("a field error");
        let mut keys: Vec<String> = ve.errors.iter().map(|e| e.field.clone()).collect();
        keys.sort();
        keys
    }

    #[test]
    fn a_document_reference_is_anchored_to_its_key() {
        let fields = vec![author("author"), author("editor")];

        assert_eq!(
            keys(&fields, &json!({ "author": "u1", "editor": "u2" }), &["u1"]),
            vec!["author"]
        );
    }

    #[test]
    fn a_reference_in_a_group_or_wrapper_uses_the_column_key() {
        let fields = vec![
            container("seo", FieldType::Group, vec![author("author")]),
            container("side", FieldType::Row, vec![author("editor")]),
        ];

        assert_eq!(
            keys(
                &fields,
                &json!({ "seo": { "author": "u1" }, "editor": "u1" }),
                &["u1"]
            ),
            vec!["editor", "seo__author"]
        );
    }

    #[test]
    fn a_reference_in_a_row_is_anchored_to_its_row_path() {
        let fields = vec![container(
            "items",
            FieldType::Array,
            vec![
                author("author"),
                container("info", FieldType::Group, vec![author("owner")]),
                container("children", FieldType::Array, vec![author("author")]),
            ],
        )];

        let data = json!({ "items": [
            { "author": "u2" },
            { "author": "u1", "info": { "owner": "u1" },
              "children": [{ "author": "u2" }, { "author": "u1" }] }
        ] });

        assert_eq!(
            keys(&fields, &data, &["u1"]),
            vec![
                "items[1][author]",
                "items[1][children][1][author]",
                "items[1][info__owner]"
            ]
        );
    }

    #[test]
    fn a_reference_in_a_block_is_anchored_to_its_row_path() {
        let block = BlockDefinition::new("hero", vec![author("author")]);
        let fields = vec![
            FieldDefinition::builder("layout", FieldType::Blocks)
                .blocks(vec![block])
                .build(),
        ];

        let data = json!({ "layout": [
            { "_block_type": "hero", "author": "u1" },
            { "_block_type": "unknown", "author": "u1" }
        ] });

        assert_eq!(keys(&fields, &data, &["u1"]), vec!["layout[0][author]"]);
    }

    #[test]
    fn has_many_and_polymorphic_values_are_read_as_references() {
        let tags = FieldDefinition::builder("readers", FieldType::Relationship)
            .relationship(RelationshipConfig::new("users", true))
            .build();
        let mut poly = RelationshipConfig::new("users", false);
        poly.polymorphic = vec!["users".into(), "posts".into()];
        let subject = FieldDefinition::builder("subject", FieldType::Relationship)
            .relationship(poly)
            .build();

        let data = json!({ "readers": ["u2", "u1"], "subject": "users/u1" });

        assert_eq!(
            keys(&[tags, subject], &data, &["u1"]),
            vec!["readers", "subject"]
        );
    }

    /// Any other error — and a refusal the data holds no reference for —
    /// passes through unchanged.
    #[test]
    fn errors_it_cannot_anchor_pass_through() {
        let fields = vec![author("author")];
        let data: DocumentFields = [("author".to_string(), json!("u2"))].into_iter().collect();

        let other = anchor_to_fields(anyhow!("boom"), &fields, &data);
        assert_eq!(other.to_string(), "boom");

        let unheld = anchor_to_fields(refusal(&["u1"]), &fields, &data);
        assert!(unheld.downcast_ref::<UnavailableReferences>().is_some());
    }
}
