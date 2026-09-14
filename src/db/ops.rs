//! Pool-based read-only wrappers around `query::*` functions for convenience.

use anyhow::{Context as _, Result};
use serde_json::{Map, Value};

use crate::{
    config::LocaleConfig,
    core::{
        CollectionDefinition, Document, DocumentFields, collection::GlobalDefinition,
        document::DocumentBuilder, field::FieldDefinition, flatten_group_fields, nest_group_fields,
        prefixed_name, walk_leaf_fields,
    },
    db::{
        DbConnection, DbPool, Filter, FilterClause, FilterOp, FindQuery, LocaleContext, LocaleMode,
        query,
        query::{
            filter::memory::matches_constraints_typed,
            helpers::{locale_column, tz_column},
        },
    },
};

/// Find documents (read-only, no transaction needed).
///
/// # Errors
///
/// Returns a backend error if the connection acquisition or query fails.
pub fn find_documents(
    pool: &DbPool,
    slug: &str,
    def: &CollectionDefinition,
    find_query: &FindQuery,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Vec<Document>> {
    let conn = pool.get().context("Failed to get DB connection")?;
    query::find(&conn, slug, def, find_query, locale_ctx)
}

/// Find a single document by ID (read-only, no transaction needed).
///
/// # Errors
///
/// Returns a backend error if the connection acquisition or query fails.
pub fn find_document_by_id(
    pool: &DbPool,
    slug: &str,
    def: &CollectionDefinition,
    id: &str,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Option<Document>> {
    let conn = pool.get().context("Failed to get DB connection")?;
    query::find_by_id(&conn, slug, def, id, locale_ctx)
}

/// Count documents (read-only, no transaction needed).
///
/// # Errors
///
/// Returns a backend error if the connection acquisition or COUNT query fails.
pub fn count_documents(
    pool: &DbPool,
    slug: &str,
    def: &CollectionDefinition,
    filters: &[FilterClause],
    locale_ctx: Option<&LocaleContext>,
) -> Result<i64> {
    let conn = pool.get().context("Failed to get DB connection")?;
    query::count(&conn, slug, def, filters, locale_ctx)
}

/// Get a global document (read-only, no transaction needed).
///
/// # Errors
///
/// Returns a backend error if the connection acquisition or query fails.
pub fn get_global(
    pool: &DbPool,
    slug: &str,
    def: &GlobalDefinition,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Document> {
    let conn = pool.get().context("Failed to get DB connection")?;
    query::get_global(&conn, slug, def, locale_ctx)
}

/// Parameters for [`find_by_id_full`].
pub struct FindByIdFullParams<'a> {
    pub conn: &'a dyn DbConnection,
    pub slug: &'a str,
    pub def: &'a CollectionDefinition,
    pub id: &'a str,
    pub locale_ctx: Option<&'a LocaleContext>,
    pub constraints: Option<Vec<FilterClause>>,
    /// Row constraints that a returned draft *snapshot* must satisfy (the draft
    /// view's filter for a live read, the trash view's filter for a trash read).
    /// Matched in memory against the snapshot fields — the snapshot bypasses the
    /// SQL `WHERE` path, so its access constraint must be enforced here.
    pub snapshot_constraints: Vec<FilterClause>,
    pub use_draft: bool,
    pub include_deleted: bool,
}

/// Resolve a draft snapshot's per-locale columns for the locale being read,
/// then drop them.
///
/// Snapshots store `title__en` / `title__de` alongside the value that was
/// resolved when the draft was saved. Without the resolve, a draft saved in
/// one locale would be served as the content of every other locale. Without
/// the drop, the caller would receive every locale's column beside the
/// resolved field — a shape no other read produces, and one that hands a
/// caller reading `de` the `en` translation it did not ask for.
///
/// Runs even when the locale context is absent or locales are off: a snapshot
/// written while locales were enabled still carries the decorated columns, and
/// they must not reach the caller either way.
///
/// An all-locales read maps each localized column field to `{ locale: value }`,
/// as the published read does.
pub(crate) fn resolve_snapshot_locale(
    doc: &mut Document,
    fields: &[FieldDefinition],
    locale_ctx: Option<&LocaleContext>,
) -> Result<()> {
    let read = SnapshotRead::new(locale_ctx.filter(|c| c.config.is_enabled()));

    // Snapshots keep groups nested while per-locale keys sit flat at the root:
    // resolve on the flat shape and nest again, so a group's localized sub-field
    // is resolved in place rather than beside a stale nested value.
    let mut flat = flatten_group_fields(&doc.fields, fields);

    walk_leaf_fields(fields, "", false, &mut |field, prefix, inherited| {
        if field.localized || inherited {
            resolve_field(&mut flat, &read, field, &prefixed_name(prefix, &field.name))?;
        }

        Ok(())
    })?;

    let mut nested = nest_group_fields(&flat, fields);
    keep_empty_groups(&doc.fields, &mut nested);
    doc.fields = nested;

    Ok(())
}

/// The locales a snapshot value is read in: the reading locale, then the
/// fallback.
#[derive(Clone, Copy)]
struct ReadLocales<'l> {
    reading: Option<&'l str>,
    fallback: Option<&'l str>,
}

impl<'l> ReadLocales<'l> {
    fn new(reading: Option<&'l str>, fallback: Option<&'l str>) -> Self {
        Self { reading, fallback }
    }
}

/// How a draft read resolves a snapshot's per-locale keys: into the value of the
/// reading locale — else the fallback — or, for an all-locales read, into a
/// `{ locale: value }` map as the published read returns.
struct SnapshotRead<'l> {
    config: Option<&'l LocaleConfig>,
    locales: ReadLocales<'l>,
    all: bool,
}

impl<'l> SnapshotRead<'l> {
    fn new(active: Option<&'l LocaleContext>) -> Self {
        let fallback = active
            .filter(|ctx| ctx.config.fallback)
            .map(|ctx| ctx.config.default_locale.as_str());

        Self {
            config: active.map(|ctx| &ctx.config),
            locales: ReadLocales::new(active.map(LocaleContext::access_locale), fallback),
            all: active.is_some_and(|ctx| matches!(ctx.mode, LocaleMode::All)),
        }
    }
}

/// Resolve one localized field of `flat` — and a timezone date's companion.
fn resolve_field(
    flat: &mut DocumentFields,
    read: &SnapshotRead<'_>,
    field: &FieldDefinition,
    name: &str,
) -> Result<()> {
    let rows = !field.has_parent_column();

    // An all-locales read maps a column field's locales; join rows are read in
    // the default locale there, as the published read hydrates them.
    if let (true, false, Some(config)) = (read.all, rows, read.config) {
        regroup_locales(flat, name, config)?;

        if field.has_tz_companion() {
            regroup_locales(flat, &tz_column(name), config)?;
        }

        return Ok(());
    }

    let source = resolve_locale_value(flat, name, read.locales, rows)?;

    // A timezone date's companion takes the zone of the locale the date came from.
    if field.has_tz_companion() {
        let locales = source.map_or(read.locales, |s| ReadLocales::new(Some(s), None));
        resolve_locale_value(flat, &tz_column(name), locales, false)?;
    }

    Ok(())
}

/// Replace the per-locale keys of `name` with a `{ locale: value }` map — the
/// shape an all-locales read returns. A snapshot without them keeps its value.
fn regroup_locales(flat: &mut DocumentFields, name: &str, config: &LocaleConfig) -> Result<()> {
    let mut by_locale = Map::new();

    for locale in &config.locales {
        if let Some(value) = flat.remove(&locale_column(name, locale)?) {
            by_locale.insert(locale.clone(), value);
        }
    }

    drop_decorated(flat, name);

    if !by_locale.is_empty() {
        flat.insert(name.to_string(), Value::Object(by_locale));
    }

    Ok(())
}

/// Set `name` to its value in `locales` from the decorated snapshot key, and
/// drop every decorated key of `name`. `rows` marks a join field (array, blocks,
/// has-many), whose locale without rows counts as empty, as the published read
/// treats it. Returns the locale the value came from.
fn resolve_locale_value<'l>(
    data: &mut DocumentFields,
    name: &str,
    locales: ReadLocales<'l>,
    rows: bool,
) -> Result<Option<&'l str>> {
    let source = take_first_value(data, name, locales, rows)?;

    // A snapshot that records the field per locale but has no value for the
    // reading (or fallback) locale holds nothing there — not the value of the
    // locale the draft was saved under, which the bare key carries.
    if source.is_none() && locales.reading.is_some() && has_decorated(data, name) {
        let empty = if rows {
            Value::Array(Vec::new())
        } else {
            Value::Null
        };
        data.insert(name.to_string(), empty);
    }

    drop_decorated(data, name);

    Ok(source)
}

/// Set `name` to its first non-empty per-locale value among `locales`, returning
/// the locale it came from.
fn take_first_value<'l>(
    data: &mut DocumentFields,
    name: &str,
    locales: ReadLocales<'l>,
    rows: bool,
) -> Result<Option<&'l str>> {
    for candidate in [locales.reading, locales.fallback].into_iter().flatten() {
        let value = data.get(&locale_column(name, candidate)?);

        if let Some(value) = value.filter(|v| !is_empty_value(v, rows)).cloned() {
            data.insert(name.to_string(), value);
            return Ok(Some(candidate));
        }
    }

    Ok(None)
}

/// Whether a snapshot value holds nothing: null, or a join field without rows.
fn is_empty_value(value: &Value, rows: bool) -> bool {
    value.is_null() || (rows && value.as_array().is_some_and(Vec::is_empty))
}

/// Whether the snapshot carries a per-locale key of `name`.
fn has_decorated(data: &DocumentFields, name: &str) -> bool {
    let decorated = format!("{name}__");
    data.keys().any(|k| k.starts_with(&decorated))
}

/// Drop every per-locale key of `name`, whichever locale it names — including
/// locales no longer in the config, which a snapshot taken before a config
/// change can still carry.
fn drop_decorated(data: &mut DocumentFields, name: &str) {
    let decorated = format!("{name}__");
    data.retain(|k, _| !k.starts_with(&decorated));
}

/// Put back the empty objects of `original` that the flat round trip has no key
/// for — an empty group — so resolution keeps the snapshot's shape.
fn keep_empty_groups(original: &DocumentFields, nested: &mut DocumentFields) {
    for (key, value) in original {
        let Some(obj) = value.as_object() else {
            continue;
        };

        match nested.remove(key) {
            Some(Value::Object(mut inner)) => {
                keep_empty_objects(obj, &mut inner);
                nested.insert(key.clone(), Value::Object(inner));
            }
            Some(other) => {
                nested.insert(key.clone(), other);
            }
            None if obj.is_empty() => {
                nested.insert(key.clone(), Value::Object(Map::new()));
            }
            None => {}
        }
    }
}

/// [`keep_empty_groups`] one level down.
fn keep_empty_objects(original: &Map<String, Value>, nested: &mut Map<String, Value>) {
    for (key, value) in original {
        let Some(obj) = value.as_object() else {
            continue;
        };

        match nested.get_mut(key) {
            Some(Value::Object(inner)) => keep_empty_objects(obj, inner),
            None if obj.is_empty() => {
                nested.insert(key.clone(), Value::Object(Map::new()));
            }
            _ => {}
        }
    }
}

/// Find a document by ID with full hydration and optional draft overlay.
///
/// Unified read path used by admin UI, gRPC, and Lua. Handles:
/// - Draft overlay: if `use_draft` is true and the latest version is a draft,
///   returns the document from the version snapshot (blocks/arrays included).
/// - Access constraints: if `constraints` is Some, uses a filtered find instead
///   of a direct `find_by_id`.
/// - Hydration: join table data (blocks, arrays, has-many) is hydrated unless
///   a draft snapshot was used (snapshots already contain everything).
///
/// # Errors
///
/// Returns a backend error if any of the underlying queries fails.
pub fn find_by_id_full(p: FindByIdFullParams<'_>) -> Result<Option<Document>> {
    // The overlay bypasses the SQL `WHERE` path, so the lifecycle filter has
    // to be applied by hand: without this a soft-deleted document's pending
    // draft is served as a live document (and the admin form opens it as
    // editable, only to fail on save).
    let lifecycle_ok = !p.use_draft
        || !p.def.has_drafts()
        || !p.def.soft_delete
        || p.include_deleted
        || query::versions::document_is_live(p.conn, p.slug, p.id)?;

    if p.use_draft
        && lifecycle_ok
        && p.def.has_drafts()
        && let Some(version) = query::find_latest_version(p.conn, p.slug, p.id)?
        && version.status == "draft"
        && let Some(mut doc) = document_from_snapshot(p.id, &version.snapshot)
    {
        // A snapshot carries every locale's decorated column plus the value
        // resolved at save time. Resolve for the READING locale, so a draft
        // saved under `en` doesn't surface as the `de` value (and vice versa).
        resolve_snapshot_locale(&mut doc, &p.def.fields, p.locale_ctx)?;

        // SECURITY: the snapshot bypasses the SQL `WHERE` path, so the view's
        // row constraint (e.g. a `draft = { author = me }` rule) must be enforced
        // here against the snapshot fields. Without this, a caller with a
        // *constrained* draft/trash rule could fetch ANY draft by id. A snapshot
        // that fails the constraint falls through to the (constrained) main-row
        // find below — which returns the published row or nothing.
        if matches_constraints_typed(&doc.fields, &p.snapshot_constraints, &p.def.fields) {
            // `_status` is the DOCUMENT's workflow status, and the row is its
            // authority — snapshots can carry a stale value (historically the
            // create path snapshotted before the draft stamp landed, and
            // pre-alpha.10 databases keep such snapshots forever). A draft-only
            // document must read as "draft"; a published document with a
            // pending draft edit reads as "published".
            if let Some(row_status) = query::versions::get_document_status(p.conn, p.slug, p.id)? {
                doc.fields
                    .insert("_status".to_string(), Value::String(row_status));
            }

            return Ok(Some(doc));
        }
    }

    let mut doc = if let Some(constraint_filters) = p.constraints {
        let mut filters = constraint_filters;
        filters.push(FilterClause::Single(Filter {
            field: "id".to_string(),
            op: FilterOp::Equals(p.id.to_string()),
        }));
        let fq = FindQuery::builder()
            .filters(filters)
            .include_deleted(p.include_deleted)
            .build();
        query::find(p.conn, p.slug, p.def, &fq, p.locale_ctx)?
            .into_iter()
            .next()
    } else {
        query::find_by_id_raw(p.conn, p.slug, p.def, p.id, p.locale_ctx, p.include_deleted)?
    };

    if let Some(ref mut d) = doc {
        query::hydrate_document(p.conn, p.slug, &p.def.fields, d, None, p.locale_ctx)?;
    }

    Ok(doc)
}

/// Reconstruct a Document from a version snapshot JSON object.
pub(crate) fn document_from_snapshot(id: &str, snapshot: &Value) -> Option<Document> {
    let obj = snapshot.as_object()?;
    let mut fields: DocumentFields = obj.clone().into_iter().collect();

    let created_at = fields
        .remove("created_at")
        .and_then(|v| v.as_str().map(str::to_string));
    let updated_at = fields
        .remove("updated_at")
        .and_then(|v| v.as_str().map(str::to_string));

    Some(
        DocumentBuilder::new(id)
            .fields(fields)
            .created_at(created_at)
            .updated_at(updated_at)
            .build(),
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::core::FieldType;

    /// A draft read resolves a hyphenated locale from its column-form key
    /// (`title__pt_BR`), a timezone date's companion follows the locale its
    /// date came from, and no decorated key — the companion's included —
    /// reaches the caller.
    #[test]
    fn snapshot_resolves_a_hyphenated_locale_and_its_timezone() {
        let fields = vec![
            FieldDefinition::builder("title", FieldType::Text)
                .localized(true)
                .build(),
            FieldDefinition::builder("starts", FieldType::Date)
                .timezone(true)
                .localized(true)
                .build(),
        ];
        let ctx = LocaleContext {
            mode: LocaleMode::Single("pt-BR".to_string()),
            config: LocaleConfig {
                default_locale: "en".to_string(),
                locales: vec!["en".to_string(), "pt-BR".to_string()],
                fallback: true,
            },
        };
        let snapshot = json!({
            "title": "Hello",
            "title__en": "Hello",
            "title__pt_BR": "Ol\u{e1}",
            "starts": "2026-01-01T10:00:00.000Z",
            "starts__en": "2026-01-01T10:00:00.000Z",
            "starts__pt_BR": "2026-01-01T13:00:00.000Z",
            "starts_tz": "Europe/London",
            "starts_tz__en": "Europe/London",
            "starts_tz__pt_BR": "America/Sao_Paulo",
        });
        let mut doc = document_from_snapshot("d1", &snapshot).unwrap();

        resolve_snapshot_locale(&mut doc, &fields, Some(&ctx)).unwrap();

        assert_eq!(doc.fields.get_str("title"), Some("Ol\u{e1}"));
        assert_eq!(
            doc.fields.get_str("starts"),
            Some("2026-01-01T13:00:00.000Z")
        );
        assert_eq!(doc.fields.get_str("starts_tz"), Some("America/Sao_Paulo"));
        for key in [
            "title__pt_BR",
            "starts__en",
            "starts_tz__en",
            "starts_tz__pt_BR",
        ] {
            assert!(!doc.fields.contains_key(key), "{key} must be dropped");
        }
    }

    /// Regression: a draft read with `locale = "all"` resolved to the default
    /// locale's value, where the published read returns every locale's.
    #[test]
    fn an_all_locales_draft_read_maps_every_locale() {
        let fields = vec![
            FieldDefinition::builder("title", FieldType::Text)
                .localized(true)
                .build(),
        ];
        let ctx = LocaleContext {
            mode: LocaleMode::All,
            config: LocaleConfig {
                default_locale: "en".to_string(),
                locales: vec!["en".into(), "de".into()],
                fallback: false,
            },
        };
        let snapshot = json!({ "title": "Hallo", "title__en": "Hello", "title__de": "Hallo" });
        let mut doc = document_from_snapshot("d1", &snapshot).unwrap();

        resolve_snapshot_locale(&mut doc, &fields, Some(&ctx)).unwrap();

        assert_eq!(
            doc.fields.get("title"),
            Some(&json!({ "en": "Hello", "de": "Hallo" }))
        );
    }

    /// Regression: resolution dropped an empty group object the snapshot
    /// carried.
    #[test]
    fn an_empty_group_survives_resolution() {
        let fields = vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("title", FieldType::Text).build(),
                ])
                .build(),
        ];
        let mut doc = document_from_snapshot("d1", &json!({ "seo": {} })).unwrap();

        resolve_snapshot_locale(&mut doc, &fields, None).unwrap();

        assert_eq!(doc.fields.get("seo"), Some(&json!({})));
    }

    /// Regression: a draft read resolved a localized field inside a group into a
    /// stray flat key (`seo__title`), leaving the nested value of the locale the
    /// draft was saved under — the one a reader saw, and the read strip missed.
    #[test]
    fn snapshot_resolves_a_localized_group_field_in_place() {
        let fields = vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("title", FieldType::Text)
                        .localized(true)
                        .build(),
                    FieldDefinition::builder("starts", FieldType::Date)
                        .timezone(true)
                        .localized(true)
                        .build(),
                ])
                .build(),
        ];
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: LocaleConfig {
                default_locale: "en".to_string(),
                locales: vec!["en".into(), "de".into()],
                fallback: false,
            },
        };
        let snapshot = json!({
            "seo": {
                "title": "Hello",
                "starts": "2026-01-01T10:00:00.000Z",
                "starts_tz": "Europe/London",
            },
            "seo__title__en": "Hello",
            "seo__title__de": "Hallo",
            "seo__starts__en": "2026-01-01T10:00:00.000Z",
            "seo__starts__de": "2026-01-01T09:00:00.000Z",
            "seo__starts_tz__en": "Europe/London",
            "seo__starts_tz__de": "Europe/Berlin",
        });
        let mut doc = document_from_snapshot("d1", &snapshot).unwrap();

        resolve_snapshot_locale(&mut doc, &fields, Some(&ctx)).unwrap();

        assert_eq!(
            doc.fields.get("seo"),
            Some(&json!({
                "title": "Hallo",
                "starts": "2026-01-01T09:00:00.000Z",
                "starts_tz": "Europe/Berlin",
            }))
        );
        assert!(!doc.fields.contains_key("seo__title"), "{:?}", doc.fields);
    }

    /// A locale without rows of a localized join field counts as empty: with
    /// fallback on the draft read takes the default locale's rows, as the
    /// published read does; with nothing to take it reads `[]`.
    #[test]
    fn snapshot_join_rows_fall_back_like_the_published_read() {
        let fields = vec![
            FieldDefinition::builder("slides", FieldType::Array)
                .localized(true)
                .build(),
        ];
        let config = |fallback| LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".into(), "de".into()],
            fallback,
        };
        let snapshot = json!({
            "slides": [{ "id": "r1" }],
            "slides__en": [{ "id": "r1" }],
            "slides__de": [],
        });

        for (fallback, expected) in [(true, json!([{ "id": "r1" }])), (false, json!([]))] {
            let ctx = LocaleContext {
                mode: LocaleMode::Single("de".to_string()),
                config: config(fallback),
            };
            let mut doc = document_from_snapshot("d1", &snapshot).unwrap();

            resolve_snapshot_locale(&mut doc, &fields, Some(&ctx)).unwrap();

            assert_eq!(
                doc.fields.get("slides"),
                Some(&expected),
                "fallback {fallback}"
            );
        }
    }

    /// Regression: a draft read in a locale the snapshot has no value for
    /// returned the bare key — the value of the locale the draft was saved
    /// under — where the published read returns nothing.
    #[test]
    fn snapshot_without_a_value_in_the_reading_locale_reads_null() {
        let fields = vec![
            FieldDefinition::builder("title", FieldType::Text)
                .localized(true)
                .build(),
        ];
        let ctx = LocaleContext {
            mode: LocaleMode::Single("fr".to_string()),
            config: LocaleConfig {
                default_locale: "en".to_string(),
                locales: vec!["en".into(), "de".into(), "fr".into()],
                fallback: false,
            },
        };
        let snapshot = json!({
            "title": "Hallo",
            "title__en": "Hello",
            "title__de": "Hallo",
            "title__fr": null,
        });
        let mut doc = document_from_snapshot("d1", &snapshot).unwrap();

        resolve_snapshot_locale(&mut doc, &fields, Some(&ctx)).unwrap();

        assert_eq!(doc.fields.get("title"), Some(&Value::Null));
    }

    #[test]
    fn snapshot_object_lifts_timestamps_out_of_fields() {
        let snapshot = json!({
            "title": "Hello",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-02T00:00:00Z"
        });

        let doc = document_from_snapshot("doc-1", &snapshot).unwrap();

        assert_eq!(&*doc.id, "doc-1");
        assert_eq!(doc.created_at.as_deref(), Some("2026-01-01T00:00:00Z"));
        assert_eq!(doc.updated_at.as_deref(), Some("2026-01-02T00:00:00Z"));

        // Timestamps are lifted into Document metadata, not left in `fields`.
        assert_eq!(doc.fields.get_str("title"), Some("Hello"));
        assert!(!doc.fields.contains_key("created_at"));
        assert!(!doc.fields.contains_key("updated_at"));
    }

    #[test]
    fn snapshot_without_timestamps_yields_none_metadata() {
        let doc = document_from_snapshot("doc-2", &json!({ "title": "x" })).unwrap();
        assert!(doc.created_at.is_none());
        assert!(doc.updated_at.is_none());
        assert_eq!(doc.fields.get_str("title"), Some("x"));
    }

    #[test]
    fn non_object_snapshot_is_rejected() {
        assert!(document_from_snapshot("doc-3", &json!(null)).is_none());
        assert!(document_from_snapshot("doc-3", &json!([1, 2, 3])).is_none());
        assert!(document_from_snapshot("doc-3", &json!("scalar")).is_none());
    }
}
