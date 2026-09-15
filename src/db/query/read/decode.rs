//! Decoding a result row into a document — the one path from a table's stored
//! columns to the values reads return.

use anyhow::Result;

use crate::{
    core::{Document, FieldDefinition},
    db::{
        DbConnection, DbRow, LocaleContext, LocaleMode,
        document::row_to_document,
        query::{
            group_locale_fields,
            helpers::{decode_value, decodes, prefixed_name, walk_leaf_fields},
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
        if !field.has_parent_column() || !decodes(field) {
            return Ok(());
        }

        let base = prefixed_name(prefix, &field.name);
        let per_locale = format!("{base}__");
        let keys: Vec<String> = doc
            .fields
            .keys()
            .filter(|k| **k == base || k.starts_with(&per_locale))
            .cloned()
            .collect();

        for key in keys {
            if let Some(value) = doc.fields.get(&key) {
                let decoded = decode_value(field, value);
                doc.fields.insert(key, decoded);
            }
        }

        Ok(())
    });
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
}
