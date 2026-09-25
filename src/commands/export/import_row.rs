//! One imported document's row: its parent columns and join rows, collected
//! from the export in the shape a write stores.

use std::{collections::BTreeMap, slice};

use anyhow::{Result, bail};
use serde_json::{Map, Value};

use crate::{
    config::LocaleConfig,
    core::{
        Builder, CollectionDefinition, DocumentFields, FieldChildren, FieldDefinition, FieldType,
        Registry, canonicalize_text_values, field_children, nest_group_fields,
    },
    db::{
        DbConnection, DbValue,
        query::{
            self,
            helpers::{
                coerce_json_value, column_value, companion_value, locale_column, prefixed_name,
            },
        },
    },
};

/// The collection documents are imported into.
#[derive(Builder)]
pub(super) struct ImportTarget<'a> {
    #[builder(required)]
    pub(super) slug: &'a str,
    #[builder(required)]
    pub(super) def: &'a CollectionDefinition,
    #[builder(required)]
    pub(super) locale: &'a LocaleConfig,
    /// The credential columns the collection's table has — none unless it is
    /// an auth collection.
    #[builder(default = Vec::new())]
    pub(super) credential_columns: Vec<&'static str>,
    /// Resolves rich text custom nodes' `searchable_attrs` for the search
    /// index, as a write through the service layer resolves them.
    pub(super) registry: Option<&'a Registry>,
}

impl<'a> ImportTarget<'a> {
    pub(super) fn resolve(
        conn: &dyn DbConnection,
        slug: &'a str,
        def: &'a CollectionDefinition,
        locale: &'a LocaleConfig,
    ) -> Result<Self> {
        let credential_columns = if def.is_auth_collection() {
            query::credential_columns(conn, slug)?
        } else {
            Vec::new()
        };

        Ok(Self::builder(slug, def, locale)
            .credential_columns(credential_columns)
            .build())
    }

    /// Index imported documents with `registry`'s rich text custom nodes.
    #[must_use]
    pub(super) fn with_registry(mut self, registry: &'a Registry) -> Self {
        self.registry = Some(registry);
        self
    }
}

/// Collected columns for a single document import row.
pub(super) struct ImportRow<'a> {
    locale: &'a LocaleConfig,
    pub(super) parent_cols: Vec<String>,
    pub(super) parent_vals: Vec<DbValue>,
    /// Join-backed fields every locale shares, by storage key.
    pub(super) join_data: DocumentFields,
    /// Localized join-backed fields: locale → storage key → rows.
    pub(super) localized_joins: BTreeMap<String, DocumentFields>,
}

impl ImportRow<'_> {
    /// Push a column/value pair.
    ///
    /// A field PRESENT in the export — including an explicit `null` — is
    /// written. `null` clears the column so the upsert restores the exported
    /// state exactly (nulls included); an ABSENT field is never routed here
    /// (callers gate on `obj.get`), so it stays unchanged under
    /// `ON CONFLICT … DO UPDATE` rather than being reset.
    fn push(&mut self, col: String, value: DbValue) {
        self.parent_cols.push(col);
        self.parent_vals.push(value);
    }

    /// Push one field's column, or one column per locale when `localized`,
    /// each value encoded by `encode`.
    fn push_field(
        &mut self,
        col: &str,
        val: &Value,
        localized: bool,
        encode: impl Fn(&Value) -> DbValue,
    ) -> Result<()> {
        if !localized {
            self.push(col.to_string(), encode(val));
            return Ok(());
        }

        for (loc, v) in self.by_locale(col, val)? {
            self.push(locale_column(col, loc)?, encode(v));
        }

        Ok(())
    }

    /// The `locale → value` pairs of a localized field's value. Fail-fast on
    /// anything else — importing a bare value into a localized field is
    /// ambiguous, and an unknown locale key would write where no locale exists.
    fn by_locale<'v>(&self, col: &str, val: &'v Value) -> Result<&'v Map<String, Value>> {
        let Value::Object(by_locale) = val else {
            bail!(
                "field '{col}' is localized — expected an object of locale → value \
                 (e.g. {{\"{}\": ...}}), got {val}",
                self.locale.default_locale
            );
        };

        let unknown = by_locale
            .keys()
            .find(|loc| *loc != &self.locale.default_locale && !self.locale.locales.contains(loc));

        if let Some(loc) = unknown {
            bail!(
                "field '{col}': unknown locale '{loc}' (configured: {:?})",
                self.locale.locales
            );
        }

        Ok(by_locale)
    }
}

/// Where a field sits: the column prefix of its enclosing groups, and whether
/// one of them is localized.
#[derive(Clone, Copy)]
struct Scope<'p> {
    prefix: &'p str,
    localized: bool,
}

impl<'p> Scope<'p> {
    fn new(prefix: &'p str, localized: bool) -> Self {
        Self { prefix, localized }
    }
}

/// Collect the columns and join data of `fields` from `obj` — the document,
/// or a group object inside it — sitting in `scope`. Layout wrappers and tabs
/// are transparent; a group at any depth extends the prefix.
fn collect_fields(
    fields: &[FieldDefinition],
    obj: &Map<String, Value>,
    scope: Scope<'_>,
    row: &mut ImportRow<'_>,
) -> Result<()> {
    for field in fields {
        match field_children(field) {
            FieldChildren::Group(sub) => {
                if let Some(Value::Object(inner)) = obj.get(&field.name) {
                    let prefix = prefixed_name(scope.prefix, &field.name);
                    let group = Scope::new(&prefix, scope.localized || field.localized);
                    collect_fields(sub, inner, group, row)?;
                }
            }
            FieldChildren::Wrapper(sub) => collect_fields(sub, obj, scope, row)?,
            FieldChildren::Tabs(tabs) => {
                for tab in tabs {
                    collect_fields(&tab.fields, obj, scope, row)?;
                }
            }
            _ if field.has_parent_column() => collect_column(field, obj, scope, row)?,
            _ => collect_join(field, obj, scope.prefix, row)?,
        }
    }

    Ok(())
}

/// Write one parent-column field (scalar or has-one), and the timezone
/// companion of a timezone date.
fn collect_column(
    field: &FieldDefinition,
    obj: &Map<String, Value>,
    scope: Scope<'_>,
    row: &mut ImportRow<'_>,
) -> Result<()> {
    let col = prefixed_name(scope.prefix, &field.name);
    // With locales off a localized field has a single column, and its export
    // holds a bare value.
    let scoped = field.is_locale_scoped(scope.localized) && row.locale.is_enabled();

    // Encoded as a write encodes: exported values are already in their stored
    // form, so no zone is applied again — a hand-edited one is canonicalized.
    if let Some(val) = obj.get(&field.name) {
        row.push_field(&col, val, scoped, |v| column_value(field, v, None))?;
    }

    // Every companion the field stores (a date's zone, a code field's language)
    // is imported beside it, under the same prefix.
    for (key, companion_col) in field
        .companion_columns(&field.name)
        .zip(field.companion_columns(&col))
    {
        if let Some(val) = obj.get(&key) {
            row.push_field(&companion_col, val, scoped, |v| companion_value(Some(v)))?;
        }
    }

    Ok(())
}

/// Carry a join-backed field (array, blocks, has-many) into the join data
/// under its storage key — a localized one per locale, as it is stored.
fn collect_join(
    field: &FieldDefinition,
    obj: &Map<String, Value>,
    prefix: &str,
    row: &mut ImportRow<'_>,
) -> Result<()> {
    let Some(val) = obj.get(&field.name).filter(|v| !v.is_null()) else {
        return Ok(());
    };
    let key = prefixed_name(prefix, &field.name);

    if field.localized && !row.locale.is_enabled() && val.is_object() {
        bail!(
            "field '{key}' holds rows per locale, but locales are off in this installation — \
             enable the locales the export was made with, then import again"
        );
    }

    if !field.localized || !row.locale.is_enabled() {
        row.join_data.insert(key, val.clone());
        return Ok(());
    }

    for (locale, rows) in row.by_locale(&key, val)? {
        // The document was canonicalized before its localized rows were split
        // out per locale, which that walk can't see into — canonicalize them here.
        let mut data = DocumentFields::new();
        data.insert(field.name.clone(), rows.clone());
        canonicalize_text_values(&mut data, slice::from_ref(field));

        if let Some(rows) = data.remove(&field.name) {
            row.localized_joins
                .entry(locale.clone())
                .or_default()
                .insert(key.clone(), rows);
        }
    }

    Ok(())
}

/// Carry a document's stored system state: timestamps, publication status and
/// trash state.
fn collect_system_columns(
    doc_obj: &Map<String, Value>,
    def: &CollectionDefinition,
    row: &mut ImportRow<'_>,
) {
    // Carry the publication status so an exported draft imports as a draft;
    // without it the row would take the column default ('published').
    let mut text_columns = Vec::new();
    if def.timestamps {
        text_columns.extend(["created_at", "updated_at"]);
    }
    if def.has_drafts() {
        text_columns.push("_status");
    }

    for col in text_columns {
        if let Some(val) = doc_obj.get(col).filter(|v| v.is_string()) {
            row.push(col.to_string(), coerce_json_value(&FieldType::Text, val));
        }
    }

    // A trashed document imports trashed; an explicit null imports it live.
    if def.soft_delete
        && let Some(val) = doc_obj.get("_deleted_at")
    {
        row.push(
            "_deleted_at".to_string(),
            coerce_json_value(&FieldType::Text, val),
        );
    }
}

/// Carry an account's credentials — the columns the target collection has.
fn collect_credentials(
    doc_obj: &Map<String, Value>,
    target: &ImportTarget<'_>,
    row: &mut ImportRow<'_>,
) -> Result<()> {
    let Some(credentials) = doc_obj.get("_credentials") else {
        return Ok(());
    };
    let Value::Object(credentials) = credentials else {
        bail!("'_credentials' must be an object, got {credentials}");
    };

    for (col, val) in query::credential_values(credentials, &target.credential_columns)? {
        row.parent_cols.push(col);
        row.parent_vals.push(val);
    }

    Ok(())
}

/// Collect parent columns and join data for a single document from its JSON representation.
pub(super) fn collect_import_columns<'a>(
    doc_obj: &Map<String, Value>,
    target: &ImportTarget<'a>,
    id: &str,
) -> Result<ImportRow<'a>> {
    let mut row = ImportRow {
        locale: target.locale,
        parent_cols: vec!["id".to_string()],
        parent_vals: vec![DbValue::Text(id.to_string())],
        join_data: DocumentFields::new(),
        localized_joins: BTreeMap::new(),
    };

    collect_system_columns(doc_obj, target.def, &mut row);
    collect_credentials(doc_obj, target, &mut row)?;
    collect_fields(&target.def.fields, doc_obj, Scope::new("", false), &mut row)?;

    Ok(row)
}

/// The document in the shape a write stores: groups nested (an export may
/// spell them flat as `group__field`) and email and text values canonical.
pub(super) fn canonical_document(
    doc_obj: &Map<String, Value>,
    fields: &[FieldDefinition],
) -> Map<String, Value> {
    let flat: DocumentFields = doc_obj.clone().into_iter().collect();
    let mut doc = nest_group_fields(&flat, fields);
    canonicalize_text_values(&mut doc, fields);

    doc.into_inner().into_iter().collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::{core::FieldAdmin, db::query::helpers::coerce_value};

    use super::*;

    /// Regression: import encoded values on its own, so a hand-edited export
    /// was stored differently from a normal write: `"42"` in a number field
    /// stayed text, a date kept the form it was typed in, and `""` stayed an
    /// empty string.
    #[test]
    fn import_encodes_values_as_a_write_does() {
        let locale = LocaleConfig::default();
        let fields = vec![
            FieldDefinition::builder("n", FieldType::Number).build(),
            FieldDefinition::builder("d", FieldType::Date).build(),
            FieldDefinition::builder("t", FieldType::Text).build(),
        ];
        let obj = json!({ "n": "42", "d": "2026-01-01", "t": "" });
        let mut row = ImportRow {
            locale: &locale,
            parent_cols: Vec::new(),
            parent_vals: Vec::new(),
            join_data: DocumentFields::new(),
            localized_joins: BTreeMap::new(),
        };

        collect_fields(
            &fields,
            obj.as_object().unwrap(),
            Scope::new("", false),
            &mut row,
        )
        .unwrap();

        let value = |col: &str| {
            let i = row.parent_cols.iter().position(|c| c == col).unwrap();
            &row.parent_vals[i]
        };
        let DbValue::Text(date) = coerce_value(&FieldType::Date, "2026-01-01") else {
            panic!("a date encodes as text");
        };

        assert!(matches!(value("n"), DbValue::Real(n) if (n - 42.0).abs() < 1e-9));
        assert!(matches!(value("d"), DbValue::Text(d) if *d == date));
        assert!(matches!(value("t"), DbValue::Null));
    }

    /// Regression: import carried a timezone date's zone but not a code field's
    /// language pick, so an export/import round trip dropped every `_lang`.
    /// Every companion a field stores is imported.
    #[test]
    fn import_carries_every_companion() {
        let locale = LocaleConfig::default();
        let fields = vec![
            FieldDefinition::builder("starts", FieldType::Date)
                .timezone(true)
                .build(),
            FieldDefinition::builder("snippet", FieldType::Code)
                .admin(
                    FieldAdmin::builder()
                        .languages(vec!["python".to_string()])
                        .build(),
                )
                .build(),
        ];
        let obj = json!({
            "starts": "2026-01-01T10:00:00.000Z",
            "starts_tz": "Europe/Berlin",
            "snippet": "print(1)",
            "snippet_lang": "python"
        });
        let mut row = ImportRow {
            locale: &locale,
            parent_cols: Vec::new(),
            parent_vals: Vec::new(),
            join_data: DocumentFields::new(),
            localized_joins: BTreeMap::new(),
        };

        collect_fields(
            &fields,
            obj.as_object().unwrap(),
            Scope::new("", false),
            &mut row,
        )
        .unwrap();

        let value = |col: &str| {
            let i = row.parent_cols.iter().position(|c| c == col);
            i.map(|i| &row.parent_vals[i])
        };

        assert!(matches!(value("starts_tz"), Some(DbValue::Text(z)) if z == "Europe/Berlin"));
        assert!(
            matches!(value("snippet_lang"), Some(DbValue::Text(l)) if l == "python"),
            "{:?}",
            row.parent_cols
        );
    }
}
