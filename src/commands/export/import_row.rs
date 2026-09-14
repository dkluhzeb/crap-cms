//! One imported document's row: its parent columns and join rows, collected
//! from the export in the shape a write stores.

use std::{collections::BTreeMap, slice};

use anyhow::{Result, bail};
use serde_json::{Map, Value};

use crate::{
    config::LocaleConfig,
    core::{
        Builder, CollectionDefinition, DocumentFields, FieldChildren, FieldDefinition, FieldType,
        canonicalize_text_values, field_children, nest_group_fields,
    },
    db::{
        DbConnection, DbValue,
        query::{
            self,
            helpers::{locale_column, prefixed_name, tz_column},
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
    fn push(&mut self, col: String, val: &Value, field_type: &FieldType) {
        self.parent_cols.push(col);
        self.parent_vals
            .push(json_to_db_value(val, field_type).unwrap_or(DbValue::Null));
    }

    /// Push one field's column, or one column per locale when `localized`.
    fn push_field(
        &mut self,
        col: &str,
        val: &Value,
        field_type: &FieldType,
        localized: bool,
    ) -> Result<()> {
        if !localized {
            self.push(col.to_string(), val, field_type);
            return Ok(());
        }

        for (loc, v) in self.by_locale(col, val)? {
            self.push(locale_column(col, loc)?, v, field_type);
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

/// Convert a JSON value to a typed `DbValue` based on the field type.
fn json_to_db_value(val: &Value, field_type: &FieldType) -> Option<DbValue> {
    match val {
        Value::Null => None,
        Value::String(s) => Some(DbValue::Text(s.clone())),
        Value::Number(n) => match field_type {
            FieldType::Number => n.as_f64().map(DbValue::Real),
            _ => n
                .as_i64()
                .map(DbValue::Integer)
                .or_else(|| n.as_f64().map(DbValue::Real)),
        },
        Value::Bool(b) => Some(DbValue::Integer(i64::from(*b))),
        other => Some(DbValue::Text(other.to_string())),
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

    if let Some(val) = obj.get(&field.name) {
        row.push_field(&col, val, &field.field_type, scoped)?;
    }

    if field.has_tz_companion()
        && let Some(tz) = obj.get(&tz_column(&field.name))
    {
        row.push_field(&tz_column(&col), tz, &FieldType::Text, scoped)?;
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
            row.push(col.to_string(), val, &FieldType::Text);
        }
    }

    // A trashed document imports trashed; an explicit null imports it live.
    if def.soft_delete
        && let Some(val) = doc_obj.get("_deleted_at")
    {
        row.push("_deleted_at".to_string(), val, &FieldType::Text);
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

    use super::*;

    #[test]
    fn json_to_db_value_null() {
        assert!(json_to_db_value(&Value::Null, &FieldType::Text).is_none());
    }

    #[test]
    fn json_to_db_value_string() {
        let val = json_to_db_value(&json!("hello"), &FieldType::Text);
        assert!(matches!(val, Some(DbValue::Text(s)) if s == "hello"));
    }

    #[test]
    fn json_to_db_value_integer() {
        let val = json_to_db_value(&json!(42), &FieldType::Text);
        assert!(matches!(val, Some(DbValue::Integer(42))));
    }

    #[test]
    fn json_to_db_value_number_field_gives_real() {
        let val = json_to_db_value(&json!(42), &FieldType::Number);
        assert!(matches!(val, Some(DbValue::Real(v)) if (v - 42.0).abs() < f64::EPSILON));
    }

    #[test]
    fn json_to_db_value_float() {
        let val = json_to_db_value(&json!(2.5), &FieldType::Text);
        assert!(matches!(val, Some(DbValue::Real(v)) if (v - 2.5).abs() < f64::EPSILON));
    }

    #[test]
    fn json_to_db_value_bool_true() {
        let val = json_to_db_value(&json!(true), &FieldType::Checkbox);
        assert!(matches!(val, Some(DbValue::Integer(1))));
    }

    #[test]
    fn json_to_db_value_bool_false() {
        let val = json_to_db_value(&json!(false), &FieldType::Checkbox);
        assert!(matches!(val, Some(DbValue::Integer(0))));
    }

    #[test]
    fn json_to_db_value_object_becomes_text() {
        let val = json_to_db_value(&json!({"key": "val"}), &FieldType::Json);
        assert!(matches!(val, Some(DbValue::Text(_))));
    }
}
