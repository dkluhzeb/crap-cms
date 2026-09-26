//! The read half of a snapshot write-back's strip: writing a stored snapshot
//! back over a document — publishing its pending draft, restoring a version —
//! never changes a value its writer cannot read.
//!
//! A snapshot carries a value per locale and the write-back writes each of
//! them, so the rule is judged per locale: each configured locale's values
//! against the document as it stands in that locale, with `ctx.locale` set to
//! it. A value the writer cannot read there is taken out of the snapshot —
//! its column, its per-locale key and, for the default locale, its bare key —
//! so the write-back leaves the stored value where it is, neither cleared nor
//! overwritten. Shared (non-localized) values are judged once, at the locale
//! the write runs under, exactly as the request's own data is. Inside a list,
//! a row matched to its stored row by `id` keeps the values its writer cannot
//! read there, and a row the document does not hold is judged against an
//! empty one — the same rule the request's own rows follow.

use anyhow::Result;
use serde_json::{Map, Value};

use crate::{
    core::{
        Builder, Document, DocumentFields, FieldDefinition, flatten_group_fields, nest_group_fields,
    },
    db::{LocaleContext, query, query::helpers::locale_column},
    hooks::lifecycle::access::has_any_field_access,
    service::hooks::FieldReadStrip,
};

use super::{
    shape::{LeafShape, drop_paths, replace_at, restore_stripped_checkboxes},
    unreadable::{ReadJudge, keep_unreadable},
};

/// The stored document a snapshot write-back lands on, read once per locale
/// it writes (fallback off, so each locale is judged on its own values) — or
/// once, without a locale, when localization is off. Empty when no field
/// carries an `access.read` rule: nothing is judged then.
#[derive(Default)]
pub struct StoredByLocale(Vec<(Option<String>, DocumentFields)>);

impl StoredByLocale {
    /// Read the stored document with `load` for every locale a write under
    /// `locale_ctx` writes back.
    ///
    /// # Errors
    ///
    /// The first error `load` returns.
    pub fn load<E, F>(
        fields: &[FieldDefinition],
        locale_ctx: Option<&LocaleContext>,
        mut load: F,
    ) -> Result<Self, E>
    where
        F: FnMut(Option<&LocaleContext>) -> Result<DocumentFields, E>,
    {
        if !has_any_field_access(fields, |f| f.access.read.as_ref()) {
            return Ok(Self::default());
        }

        let Some(ctx) = locale_ctx.filter(|ctx| ctx.config.is_enabled()) else {
            return Ok(Self(vec![(None, load(None)?)]));
        };

        let mut docs = Vec::with_capacity(ctx.config.locales.len());

        for code in &ctx.config.locales {
            let exact = LocaleContext::exact(&ctx.config, code);
            docs.push((Some(code.clone()), load(Some(&exact))?));
        }

        Ok(Self(docs))
    }

    /// The document as stored in `locale` (`None` without localization).
    fn get(&self, locale: Option<&str>) -> Option<&DocumentFields> {
        self.0
            .iter()
            .find(|(code, _)| code.as_deref() == locale)
            .map(|(_, doc)| doc)
    }
}

/// Whose read access a snapshot write-back is judged for: the stored document
/// per locale, the collection or global slug, the writer, and the locale the
/// write runs under (which judges the shared values).
#[derive(Builder)]
pub struct SnapshotReadKeep<'a> {
    #[builder(required)]
    pub stored: &'a StoredByLocale,
    #[builder(required)]
    pub collection: &'a str,
    pub user: Option<&'a Document>,
    pub locale_ctx: Option<&'a LocaleContext>,
}

/// What one pass decides for the snapshot: its values as the write-back
/// takes them (flat columns) and what is left of them once every value the
/// writer cannot read is kept.
struct Pass {
    /// The locale judged; `None` without localization.
    locale: Option<String>,
    /// Whether the columns are written back as per-locale keys (the localized
    /// ones) or as the shared value.
    localized: bool,
    view: DocumentFields,
    kept: DocumentFields,
}

/// What every pass judges with: the read strip, the schema and the keep.
/// Every field is required and it is built at the one call site, so a plain
/// literal stands in for a builder.
struct Judging<'j, S: ?Sized> {
    strip: &'j S,
    fields: &'j [FieldDefinition],
    keep: &'j SnapshotReadKeep<'j>,
}

impl<S: FieldReadStrip + ?Sized> Judging<'_, S> {
    /// Judge `view` — values the write-back writes for `locale` — against the
    /// document as stored there. `None` when that document was not read.
    fn pass(&self, view: DocumentFields, locale: Option<&str>, localized: bool) -> Option<Pass> {
        let stored = self.keep.stored.get(locale)?;

        let judge = ReadJudge::builder(stored, self.keep.collection)
            .user(self.keep.user)
            .locale(locale)
            .build();

        let mut data: Map<String, Value> = nest_group_fields(&view, self.fields)
            .into_inner()
            .into_iter()
            .collect();
        keep_unreadable(self.strip, self.fields, &mut data, &judge);

        let kept = flatten_group_fields(&data.into_iter().collect::<DocumentFields>(), self.fields);

        Some(Pass {
            locale: locale.map(str::to_string),
            localized,
            view,
            kept,
        })
    }
}

/// Take out of `snapshot` (as stored: groups flat or nested) every value the writer
/// cannot read where the write-back lands it (see the module docs).
///
/// # Errors
///
/// Returns an error if a configured locale code has no column form.
pub(super) fn keep_unreadable_snapshot<S: FieldReadStrip + ?Sized>(
    strip: &S,
    fields: &[FieldDefinition],
    snapshot: &mut Map<String, Value>,
    keep: &SnapshotReadKeep<'_>,
) -> Result<()> {
    if !has_any_field_access(fields, |f| f.access.read.as_ref()) {
        return Ok(());
    }

    let judging = Judging {
        strip,
        fields,
        keep,
    };

    let passes = if let Some(ctx) = keep.locale_ctx.filter(|ctx| ctx.config.is_enabled()) {
        locale_passes(&judging, snapshot, ctx)?
    } else {
        let view = query::snapshot_write_fields(snapshot, fields, None)?;
        judging.pass(view, None, false).into_iter().collect()
    };

    // Decided on the snapshot as it came, then applied: a pass never sees
    // another's edits.
    for pass in &passes {
        apply_pass(fields, snapshot, pass, keep)?;
    }

    Ok(())
}

/// One pass over the shared values, at the locale the write runs under, and
/// one per configured locale over its localized values.
fn locale_passes<S: FieldReadStrip + ?Sized>(
    judging: &Judging<'_, S>,
    snapshot: &Map<String, Value>,
    ctx: &LocaleContext,
) -> Result<Vec<Pass>> {
    let shared = query::shared_field_columns(judging.fields);
    let view_in = |code: &str, keep: &dyn Fn(&str) -> bool| -> Result<DocumentFields> {
        let exact = LocaleContext::exact(&ctx.config, code);
        let view = query::snapshot_write_fields(snapshot, judging.fields, Some(&exact))?;

        Ok(columns(view, keep))
    };

    // The shared values a snapshot carries are its default-locale values.
    let shared_view = view_in(&ctx.config.default_locale, &|c| shared.contains(c))?;
    let mut passes: Vec<Pass> = judging
        .pass(shared_view, Some(ctx.access_locale()), false)
        .into_iter()
        .collect();

    for code in &ctx.config.locales {
        let view = view_in(code, &|c| !shared.contains(c))?;
        passes.extend(judging.pass(view, Some(code.as_str()), true));
    }

    Ok(passes)
}

/// The columns of `view` that `keep` accepts.
fn columns(view: DocumentFields, keep: &dyn Fn(&str) -> bool) -> DocumentFields {
    view.into_inner()
        .into_iter()
        .filter(|(column, _)| keep(column))
        .collect()
}

/// Write one pass's decisions into the snapshot: a value left out loses the
/// keys the write-back would take it from, and a list whose rows kept values
/// is replaced by the kept rows.
fn apply_pass(
    fields: &[FieldDefinition],
    snapshot: &mut Map<String, Value>,
    pass: &Pass,
    keep: &SnapshotReadKeep<'_>,
) -> Result<()> {
    let mut dropped = Vec::new();

    for (column, value) in &pass.view {
        match pass.kept.get(column) {
            Some(kept) if kept == value => {}
            Some(kept) => {
                for key in snapshot_keys(pass, column, keep)? {
                    replace_key(snapshot, &key, column, kept.clone());
                }
            }
            None => dropped.push(column.clone()),
        }
    }

    let mut keys = Vec::new();
    for column in &dropped {
        keys.extend(snapshot_keys(pass, column, keep)?);
    }
    drop_paths(snapshot, "", &keys);

    if !pass.localized {
        let stored = keep.stored.get(pass.locale.as_deref());
        restore_shared_checkboxes(fields, snapshot, stored, &dropped);
    }

    Ok(())
}

/// A shared value is written back through the row UPDATE, which stores an
/// absent checkbox as unchecked: a checkbox left out gets its `stored` value
/// back instead.
fn restore_shared_checkboxes(
    fields: &[FieldDefinition],
    snapshot: &mut Map<String, Value>,
    stored: Option<&DocumentFields>,
    dropped: &[String],
) {
    let Some(stored) = stored else {
        return;
    };

    let stored: Map<String, Value> = nest_group_fields(stored, fields)
        .into_inner()
        .into_iter()
        .collect();
    let shape = LeafShape::of(fields, false);

    restore_stripped_checkboxes(&shape, dropped, snapshot, &stored);
}

/// Put a list's kept rows under one of the keys the write-back reads it from,
/// wherever the snapshot holds that key — flat or inside its group object. A
/// per-locale key the snapshot does not hold yet goes to the snapshot root,
/// where per-locale keys live.
fn replace_key(snapshot: &mut Map<String, Value>, key: &str, column: &str, kept: Value) {
    if replace_at(snapshot, key, kept.clone()) || key == column {
        return;
    }

    snapshot.insert(key.to_string(), kept);
}

/// The snapshot keys a pass's `column` is written back from: the bare column
/// for a shared value; the column's per-locale key for a localized one, and
/// for the default locale its bare key as well.
fn snapshot_keys(pass: &Pass, column: &str, keep: &SnapshotReadKeep<'_>) -> Result<Vec<String>> {
    let (true, Some(locale)) = (pass.localized, pass.locale.as_deref()) else {
        return Ok(vec![column.to_string()]);
    };

    let mut keys = vec![locale_column(column, locale)?];

    let is_default = keep
        .locale_ctx
        .is_some_and(|ctx| ctx.config.default_locale == locale);
    if is_default {
        keys.push(column.to_string());
    }

    Ok(keys)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        config::LocaleConfig,
        core::{FieldAccess, FieldType, HookRef},
        db::LocaleMode,
        hooks::lifecycle::access::strip_read_access_data_aware,
    };

    /// A read strip that denies every rule named `deny`, a rule named
    /// `deny_de` under the `de` locale, and a rule named `deny_when_locked` on
    /// a level whose `locked` is true.
    struct RuleStrip;

    impl FieldReadStrip for RuleStrip {
        fn strip_read_access_map(
            &self,
            fields: &[FieldDefinition],
            level: &mut Map<String, Value>,
            _document: &DocumentFields,
            _collection: &str,
            _user: Option<&Document>,
            locale: Option<&str>,
        ) {
            let is_denied = |hook: &HookRef, data: &DocumentFields| match hook.reference() {
                "deny" => true,
                "deny_de" => locale == Some("de"),
                "deny_when_locked" => data.get("locked") == Some(&json!(true)),
                _ => false,
            };

            strip_read_access_data_aware(fields, level, &is_denied);
        }
    }

    fn read_gated(name: &str, field_type: FieldType, rule: &str) -> FieldDefinition {
        FieldDefinition::builder(name, field_type)
            .access(FieldAccess {
                read: Some(rule.into()),
                ..Default::default()
            })
            .build()
    }

    fn object(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            _ => unreachable!("fixture is an object"),
        }
    }

    fn fields(value: Value) -> DocumentFields {
        object(value).into_iter().collect()
    }

    fn en_de(request: &str) -> LocaleContext {
        LocaleContext {
            mode: LocaleMode::Single(request.to_string()),
            config: LocaleConfig {
                default_locale: "en".to_string(),
                locales: vec!["en".to_string(), "de".to_string()],
                fallback: true,
            },
        }
    }

    /// Run the keep over `snapshot` with `stored` per locale.
    fn keep(
        schema: &[FieldDefinition],
        snapshot: Value,
        stored: Vec<(Option<&str>, Value)>,
        locale_ctx: Option<&LocaleContext>,
    ) -> Value {
        let stored = StoredByLocale(
            stored
                .into_iter()
                .map(|(code, doc)| (code.map(str::to_string), fields(doc)))
                .collect(),
        );
        let mut snapshot = object(snapshot);

        keep_unreadable_snapshot(
            &RuleStrip,
            schema,
            &mut snapshot,
            &SnapshotReadKeep::builder(&stored, "posts")
                .locale_ctx(locale_ctx)
                .build(),
        )
        .unwrap();

        Value::Object(snapshot)
    }

    /// Regression: a restore wrote the snapshot's value into every field,
    /// read-denied ones included — a restorer changed values they could not
    /// see. A shared checkbox is put back as stored: the row write stores an
    /// absent one as unchecked.
    #[test]
    fn an_unlocalized_snapshot_leaves_out_what_its_writer_cannot_read() {
        let schema = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            read_gated("note", FieldType::Text, "deny"),
            read_gated("flag", FieldType::Checkbox, "deny"),
        ];

        let kept = keep(
            &schema,
            json!({ "title": "old", "note": "old note", "flag": false }),
            vec![(None, json!({ "title": "t", "note": "live", "flag": true }))],
            None,
        );

        assert_eq!(kept, json!({ "title": "old", "flag": true }));
    }

    /// A localized value is judged per locale: denied in `de` only, its `de`
    /// key goes and its `en` keys stay. A shared value is judged at the locale
    /// the write runs under.
    #[test]
    fn a_localized_value_is_judged_per_locale() {
        let schema = vec![
            FieldDefinition::builder("title", FieldType::Text)
                .localized(true)
                .access(FieldAccess {
                    read: Some("deny_de".into()),
                    ..Default::default()
                })
                .build(),
            read_gated("slug", FieldType::Text, "deny_de"),
        ];
        let stored = || {
            vec![
                (Some("en"), json!({ "title": "live en", "slug": "s" })),
                (Some("de"), json!({ "title": "live de", "slug": "s" })),
            ]
        };
        let snapshot = json!({
            "title": "draft en",
            "title__en": "draft en",
            "title__de": "draft de",
            "slug": "drafted",
        });

        let en = en_de("en");
        assert_eq!(
            keep(&schema, snapshot.clone(), stored(), Some(&en)),
            json!({ "title": "draft en", "title__en": "draft en", "slug": "drafted" }),
            "a publish in `en` judges the shared slug in `en`"
        );

        let de = en_de("de");
        assert_eq!(
            keep(&schema, snapshot, stored(), Some(&de)),
            json!({ "title": "draft en", "title__en": "draft en" }),
            "a publish in `de` judges the shared slug in `de`, and in `de` the title too"
        );
    }

    /// A list's rows are judged per row: a row the writer cannot read keeps
    /// its stored value, a row it can read takes the snapshot's.
    #[test]
    fn a_row_value_is_kept_where_its_row_denies_it() {
        let schema = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("locked", FieldType::Checkbox).build(),
                    read_gated("secret", FieldType::Text, "deny_when_locked"),
                ])
                .build(),
        ];

        let kept = keep(
            &schema,
            json!({ "items": [
                { "id": "r1", "locked": true, "secret": "old" },
                { "id": "r2", "locked": false, "secret": "old" },
            ] }),
            vec![(
                None,
                json!({ "items": [
                    { "id": "r1", "locked": true, "secret": "live" },
                    { "id": "r2", "locked": false, "secret": "live" },
                ] }),
            )],
            None,
        );

        assert_eq!(
            kept,
            json!({ "items": [
                { "id": "r1", "locked": true },
                { "id": "r2", "locked": false, "secret": "old" },
            ] })
        );
    }

    #[test]
    fn without_read_rules_nothing_is_touched() {
        let schema = vec![FieldDefinition::builder("title", FieldType::Text).build()];
        let snapshot = json!({ "title": "old" });

        assert_eq!(keep(&schema, snapshot.clone(), Vec::new(), None), snapshot);
    }
}
