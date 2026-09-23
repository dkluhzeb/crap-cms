//! Decoding a result row into a document — the one path from a table's stored
//! columns to the values reads return.

use anyhow::Result;
use serde_json::{Map, Value};

use crate::{
    core::{
        BLOCK_TYPE_KEY, BlockDefinition, Document, DocumentFields, FieldChildren, FieldDefinition,
        JsonRoot, field_children,
    },
    db::{
        DbConnection, DbRow, LocaleContext, LocaleMode,
        document::row_to_document,
        query::{
            group_locale_fields,
            helpers::{
                ListPlace, decode_row_value, decode_value, decodes, prefixed_name, walk_leaf_fields,
            },
        },
    },
};

/// Decode a result row of a table with `fields` into a document: every column in
/// the form reads return it, then — for an all-locales read — the per-locale
/// columns grouped into `{ locale: value }` maps. Decoding comes first, so a
/// grouped value is already in its read form.
///
/// # Errors
///
/// Returns an error if the row has no `id` or a locale column name is invalid.
pub(crate) fn decode_row(
    conn: &dyn DbConnection,
    row: &DbRow,
    fields: &[FieldDefinition],
    locale_ctx: Option<&LocaleContext>,
) -> Result<Document> {
    let mut doc = row_to_document(conn, row)?;
    decode_columns(fields, &mut doc);

    if let Some(ctx) = locale_ctx
        && ctx.config.is_enabled()
        && let LocaleMode::All = ctx.mode
    {
        group_locale_fields(&mut doc, fields, &ctx.config)?;
    }

    Ok(doc)
}

/// Decode each column of `doc` — bare, or per locale (`{col}__{locale}`) — by
/// its field. Array and blocks rows are decoded by their own hydration.
fn decode_columns(fields: &[FieldDefinition], doc: &mut Document) {
    let _ = walk_leaf_fields(fields, "", false, &mut |field, prefix, _| {
        if field.has_parent_column() {
            let name = prefixed_name(prefix, &field.name);
            decode_column(&mut doc.fields, &name, field, ListPlace::Column);
        }

        Ok(())
    });
}

/// Decode the column `base` of `level` — and its per-locale columns
/// (`{base}__{locale}`) — by `field`, when the field decodes at all. `place`
/// says whether `level` is a document or a row inside one.
fn decode_column<R: JsonRoot>(
    level: &mut R,
    base: &str,
    field: &FieldDefinition,
    place: ListPlace,
) {
    if !decodes(field) {
        return;
    }

    for key in column_keys(&*level, base) {
        if let Some(value) = level.root_get(&key) {
            let decoded = match place {
                ListPlace::Column => decode_value(field, value),
                ListPlace::Row => decode_row_value(field, value),
            };
            level.root_insert(key, decoded);
        }
    }
}

/// The keys of `level` that hold the column `base`: the bare key and every
/// per-locale one.
fn column_keys<R: JsonRoot>(level: &R, base: &str) -> Vec<String> {
    let per_locale = format!("{base}__");

    level
        .root_keys()
        .into_iter()
        .filter(|key| key == base || key.starts_with(&per_locale))
        .collect()
}

/// Decode a document held outside its table — a version or draft snapshot —
/// the way a read of the table decodes it, at every depth: a column (bare or
/// per locale), a group's leaves (nested as snapshots keep them, or flat), an
/// array row's columns and a blocks row's fields, so a snapshot written before
/// a column's read form changed reads like the live row.
pub(crate) fn decode_document_values(data: &mut DocumentFields, fields: &[FieldDefinition]) {
    decode_level(data, fields, "", ListPlace::Column);
}

/// Decode every field of `fields` found at `level` under `prefix`; `place` says
/// whether `level` is the document or a row inside it.
fn decode_level<R: JsonRoot>(
    level: &mut R,
    fields: &[FieldDefinition],
    prefix: &str,
    place: ListPlace,
) {
    for field in fields {
        let name = prefixed_name(prefix, &field.name);

        match field_children(field) {
            FieldChildren::Leaf => {
                if field.has_parent_column() {
                    decode_column(level, &name, field, place);
                }
            }
            FieldChildren::Group(sub) => decode_group(level, &name, sub, place),
            FieldChildren::Wrapper(sub) => decode_level(level, sub, prefix, place),
            FieldChildren::Tabs(tabs) => {
                for tab in tabs {
                    decode_level(level, &tab.fields, prefix, place);
                }
            }
            FieldChildren::Array(sub) => {
                decode_rows(level, &name, |row| {
                    decode_level(row, sub, "", ListPlace::Row);
                });
            }
            FieldChildren::Blocks(defs) => {
                decode_rows(level, &name, |row| decode_block_row(row, defs));
            }
        }
    }
}

/// A group's leaves sit in a nested object (as snapshots keep them) or flat
/// under `group__leaf` (as a row reads them); both forms are decoded.
fn decode_group<R: JsonRoot>(level: &mut R, name: &str, sub: &[FieldDefinition], place: ListPlace) {
    if let Some(Value::Object(nested)) = level.root_get_mut(name) {
        decode_level(nested, sub, "", place);
    }

    decode_level(level, sub, &format!("{name}__"), place);
}

/// Apply `decode_row` to every object row of the list stored under `name`,
/// bare or per locale.
fn decode_rows<R: JsonRoot>(
    level: &mut R,
    name: &str,
    mut decode_row: impl FnMut(&mut Map<String, Value>),
) {
    for key in column_keys(&*level, name) {
        if let Some(Value::Array(rows)) = level.root_get_mut(&key) {
            for row in rows.iter_mut().filter_map(Value::as_object_mut) {
                decode_row(row);
            }
        }
    }
}

/// Decode a blocks row against its block's fields — or, when the row names
/// no known block, against every block's fields.
fn decode_block_row(row: &mut Map<String, Value>, defs: &[BlockDefinition]) {
    let block_type = row
        .get(BLOCK_TYPE_KEY)
        .and_then(Value::as_str)
        .map(str::to_owned);
    let matched = block_type
        .as_deref()
        .and_then(|ty| defs.iter().find(|def| def.block_type == ty));

    match matched {
        Some(def) => decode_level(row, &def.fields, "", ListPlace::Row),
        None => {
            for def in defs {
                decode_level(row, &def.fields, "", ListPlace::Row);
            }
        }
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        config::LocaleConfig,
        core::FieldType,
        db::{DbValue, InMemoryConn},
    };

    /// Regression: an all-locales read grouped a scalar has-many list's
    /// per-locale columns before decoding them, and the decode read the grouped
    /// map as a malformed list — every locale's list came back empty.
    #[test]
    fn an_all_locales_row_groups_decoded_lists() {
        let conn = InMemoryConn::open();
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Text)
                .has_many(true)
                .localized(true)
                .build(),
        ];
        let row = DbRow::new(
            vec!["id".into(), "tags__en".into(), "tags__de".into()],
            vec![
                DbValue::Text("d1".into()),
                DbValue::Text(r#"["a","b"]"#.into()),
                DbValue::Text(r#"["c"]"#.into()),
            ],
        );
        let ctx = LocaleContext {
            mode: LocaleMode::All,
            config: LocaleConfig {
                default_locale: "en".to_string(),
                locales: vec!["en".to_string(), "de".to_string()],
                fallback: false,
            },
        };

        let doc = decode_row(&conn, &row, &fields, Some(&ctx)).unwrap();

        assert_eq!(
            doc.fields.get("tags"),
            Some(&json!({ "en": ["a", "b"], "de": ["c"] }))
        );
    }

    /// Regression: a checkbox column read as the `1`/`0` its column holds, and
    /// a JSON column as its text, while the same fields inside a row read as a
    /// boolean and a parsed value. A row decodes every column — a group's
    /// prefixed column and a per-locale column included.
    #[test]
    fn a_row_decodes_checkbox_and_json_columns() {
        let conn = InMemoryConn::open();
        let fields = vec![
            FieldDefinition::builder("done", FieldType::Checkbox).build(),
            FieldDefinition::builder("meta", FieldType::Json).build(),
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("index", FieldType::Checkbox).build(),
                ])
                .build(),
            FieldDefinition::builder("flag", FieldType::Checkbox)
                .localized(true)
                .build(),
        ];
        let row = DbRow::new(
            vec![
                "id".into(),
                "done".into(),
                "meta".into(),
                "seo__index".into(),
                "flag__en".into(),
                "flag__de".into(),
            ],
            vec![
                DbValue::Text("d1".into()),
                DbValue::Integer(1),
                DbValue::Text(r#"{"n":1}"#.into()),
                DbValue::Integer(0),
                DbValue::Integer(1),
                DbValue::Null,
            ],
        );
        let ctx = LocaleContext {
            mode: LocaleMode::All,
            config: LocaleConfig {
                default_locale: "en".to_string(),
                locales: vec!["en".to_string(), "de".to_string()],
                fallback: false,
            },
        };

        let doc = decode_row(&conn, &row, &fields, Some(&ctx)).unwrap();

        assert_eq!(doc.fields.get("done"), Some(&json!(true)));
        assert_eq!(doc.fields.get("meta"), Some(&json!({ "n": 1 })));
        assert_eq!(doc.fields.get("seo__index"), Some(&json!(false)));
        assert_eq!(
            doc.fields.get("flag"),
            Some(&json!({ "en": true, "de": false })),
            "an unset checkbox column reads as false"
        );
    }

    /// A snapshot written before a column's read form changed — a checkbox as
    /// `1`, JSON as text, in a column, a per-locale column and an array row —
    /// decodes as a table read does. Blocks rows are stored typed already.
    #[test]
    fn a_snapshot_decodes_its_columns_and_array_rows() {
        let fields = vec![
            FieldDefinition::builder("done", FieldType::Checkbox)
                .localized(true)
                .build(),
            FieldDefinition::builder("meta", FieldType::Json).build(),
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("flag", FieldType::Checkbox).build(),
                    FieldDefinition::builder("extra", FieldType::Json).build(),
                    FieldDefinition::builder("sub", FieldType::Group)
                        .fields(vec![
                            FieldDefinition::builder("deep", FieldType::Checkbox).build(),
                        ])
                        .build(),
                ])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("done".to_string(), json!(1));
        data.insert("done__en".to_string(), json!(1));
        data.insert("done__de".to_string(), json!(0));
        data.insert("meta".to_string(), json!("{\"n\":1}"));
        data.insert(
            "items".to_string(),
            json!([{ "id": "r1", "flag": 1, "extra": "[1]", "sub": { "deep": true } }]),
        );

        decode_document_values(&mut data, &fields);

        assert_eq!(data.get("done"), Some(&json!(true)));
        assert_eq!(data.get("done__en"), Some(&json!(true)));
        assert_eq!(data.get("done__de"), Some(&json!(false)));
        assert_eq!(data.get("meta"), Some(&json!({ "n": 1 })));
        assert_eq!(
            data.get("items"),
            Some(&json!([{ "id": "r1", "flag": true, "extra": [1], "sub": { "deep": true } }]))
        );

        let once = data.clone();
        decode_document_values(&mut data, &fields);
        assert_eq!(data, once, "decoding a decoded snapshot changes nothing");
    }
    /// A snapshot kept from before a field switched to `has_many` reads its
    /// text as a write at the same place stores it: a document column's
    /// `"Hello, world"` is one value, the comma list an earlier admin form
    /// stored in an array row or a block its values.
    #[test]
    fn a_snapshot_reads_list_text_by_where_it_was_stored() {
        let tags = || {
            FieldDefinition::builder("tags", FieldType::Text)
                .has_many(true)
                .build()
        };
        let fields = vec![
            tags(),
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![tags()])
                .build(),
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![tags()])
                .build(),
            FieldDefinition::builder("content", FieldType::Blocks)
                .blocks(vec![BlockDefinition::new("quote", vec![tags()])])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("tags".to_string(), json!("Hello, world"));
        data.insert("seo".to_string(), json!({ "tags": "Hello, world" }));
        data.insert("items".to_string(), json!([{ "tags": "a, b" }]));
        data.insert(
            "content".to_string(),
            json!([{ "_block_type": "quote", "tags": "a,b" }]),
        );

        decode_document_values(&mut data, &fields);

        assert_eq!(data.get("tags"), Some(&json!(["Hello, world"])));
        assert_eq!(data.get("seo"), Some(&json!({ "tags": ["Hello, world"] })));
        assert_eq!(data.get("items"), Some(&json!([{ "tags": ["a", "b"] }])));
        assert_eq!(
            data.get("content"),
            Some(&json!([{ "_block_type": "quote", "tags": ["a", "b"] }]))
        );
    }
}
