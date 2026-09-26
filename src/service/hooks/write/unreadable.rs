//! The read half of an update's field strip: a write never changes a value its
//! writer cannot read.
//!
//! Field `access.read` and `access.update` are independent rules, so a writer
//! may be allowed to update a field it is not allowed to see. Such a writer
//! works blind: whatever it sends for the field — an empty input, an unchecked
//! box, an empty list — replaces a value it never saw. The update strip
//! therefore judges every `access.read` rule against what the write replaces
//! — the stored document, or for a draft save the pending draft its form
//! showed — and keeps each value the writer cannot read exactly as it is, at
//! every depth. A row the write adds holds nothing stored and is judged
//! against an empty row, so a writer cannot fill in blind what it could not
//! read there either.

use serde_json::{Map, Value};

use crate::{
    core::{
        BLOCK_TYPE_KEY, Builder, Document, DocumentFields, FieldChildren, FieldDefinition,
        field_children,
    },
    hooks::lifecycle::access::has_any_field_access,
    service::hooks::{
        FieldReadStrip,
        read_probe::{RowSchema, TemplateRows, mark_absent, template_row_index},
    },
};

/// The stored documents an update's field strip judges.
///
/// Two documents, because a draft save does not replace the row: it replaces
/// the pending draft, the content its draft edit form showed. The
/// `access.update` rules judge the row as stored whatever the write; which
/// values the writer cannot read — and so keeps as they are — is judged
/// against what the write replaces.
#[derive(Clone, Copy)]
pub struct UpdateStored<'a> {
    /// The row as stored: every `access.update` rule's `ctx.document`.
    pub row: &'a DocumentFields,
    /// What the write replaces: the pending draft for a draft save that has
    /// one, else the row.
    pub replaced: &'a DocumentFields,
}

impl<'a> UpdateStored<'a> {
    /// A write over `replaced`, judged by the update rules against `row`.
    #[must_use]
    pub fn new(row: &'a DocumentFields, replaced: &'a DocumentFields) -> Self {
        Self { row, replaced }
    }

    /// A write that replaces the stored row itself.
    #[must_use]
    pub fn of_row(row: &'a DocumentFields) -> Self {
        Self::new(row, row)
    }
}

/// Whom the stored document is judged for: the stored document itself (each
/// `access.read` rule's `ctx.document`, and the source of every `ctx.data`
/// level), the collection or global slug, the writer, and the locale the rules
/// are evaluated in.
#[derive(Builder)]
pub(super) struct ReadJudge<'a> {
    #[builder(required)]
    pub stored: &'a DocumentFields,
    #[builder(required)]
    pub collection: &'a str,
    pub user: Option<&'a Document>,
    pub locale: Option<&'a str>,
}

/// Where a data level sits in storage, which decides how a value the writer
/// cannot read is kept.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Level {
    /// The document row, or a group on it: every key is a column, and a column
    /// the write leaves out keeps its stored value.
    Columns,
    /// An array or blocks row matched to its stored row by `id`: every key is
    /// a column of the row (or a top-level block field), kept when left out.
    Row,
    /// Inside a JSON value (a group or a list within a row): the value is
    /// written as a whole, so a kept value has to be put back, not left out.
    Json,
}

/// One data level of the stored document, before and after the read strip.
#[derive(Clone, Copy)]
struct Views<'a> {
    /// The stored level, every declared field present (absent ones as `null`).
    stored: &'a Map<String, Value>,
    /// The same level with every value the writer cannot read removed.
    readable: &'a Map<String, Value>,
}

impl Views<'_> {
    /// Whether the read strip hid the stored value at `key`.
    fn hides(&self, key: &str) -> bool {
        self.stored.contains_key(key) && !self.readable.contains_key(key)
    }
}

/// Keep, in the incoming update `data`, every stored value the writer cannot
/// read: on a row or a column level the value is left out of the write (its
/// column keeps the stored value); inside a JSON value it is put back from the
/// stored document.
///
/// Rows of a top-level array/blocks field are matched to their stored rows by
/// `id`; a row without a known `id` is new and holds nothing stored, so it is
/// judged against an empty row and may not fill in what the writer cannot read
/// there. A list nested inside a row has no row identity, so when it holds a
/// value the writer cannot read it is kept as stored as a whole.
pub(super) fn keep_unreadable<S: FieldReadStrip + ?Sized>(
    strip: &S,
    fields: &[FieldDefinition],
    data: &mut Map<String, Value>,
    judge: &ReadJudge<'_>,
) {
    if !has_any_field_access(fields, |f| f.access.read.as_ref()) {
        return;
    }

    let mut stored: Map<String, Value> = judge.stored.clone().into_inner().into_iter().collect();
    mark_absent(fields, &mut stored, TemplateRows::TopLevel);

    let mut readable = stored.clone();
    strip.strip_read_access_map(
        fields,
        &mut readable,
        judge.stored,
        judge.collection,
        judge.user,
        judge.locale,
    );

    let views = Views {
        stored: &stored,
        readable: &readable,
    };
    keep_level(fields, data, views, Level::Columns);
}

/// Keep the unreadable values of one data level of the write.
fn keep_level(
    fields: &[FieldDefinition],
    data: &mut Map<String, Value>,
    views: Views<'_>,
    level: Level,
) {
    for field in fields {
        match field_children(field) {
            FieldChildren::Wrapper(sub) => {
                keep_level(sub, data, views, level);
                continue;
            }
            FieldChildren::Tabs(tabs) => {
                for tab in tabs {
                    keep_level(&tab.fields, data, views, level);
                }
                continue;
            }
            _ => {}
        }

        if views.hides(&field.name) {
            for column in field.columns_with_companions(&field.name) {
                keep_value(data, &column, views.stored, level);
            }

            continue;
        }

        keep_children(field, data, views, level);
    }
}

/// Keep the stored value at `key`: leave it out of a column write, put it back
/// into a JSON value.
fn keep_value(data: &mut Map<String, Value>, key: &str, stored: &Map<String, Value>, level: Level) {
    if level != Level::Json {
        data.remove(key);
        return;
    }

    match stored.get(key) {
        Some(value) if !value.is_null() => {
            data.insert(key.to_string(), value.clone());
        }
        _ => {
            data.remove(key);
        }
    }
}

/// Descend into a readable composite to keep the unreadable values inside it.
fn keep_children(
    field: &FieldDefinition,
    data: &mut Map<String, Value>,
    views: Views<'_>,
    level: Level,
) {
    let schema = match field_children(field) {
        FieldChildren::Group(sub) => {
            let inner = if level == Level::Columns {
                Level::Columns
            } else {
                Level::Json
            };

            return keep_group(sub, &field.name, data, views, inner);
        }
        FieldChildren::Array(sub) => RowSchema::Array(sub),
        FieldChildren::Blocks(blocks) => RowSchema::Blocks(blocks),
        FieldChildren::Leaf | FieldChildren::Wrapper(_) | FieldChildren::Tabs(_) => return,
    };

    if level == Level::Columns {
        keep_rows(&field.name, schema, data, views);
    } else {
        keep_whole_list(&field.name, data, views);
    }
}

/// [`keep_level`] inside the group object stored under `name`.
fn keep_group(
    fields: &[FieldDefinition],
    name: &str,
    data: &mut Map<String, Value>,
    views: Views<'_>,
    level: Level,
) {
    let (Some(Value::Object(data)), Some(Value::Object(stored)), Some(Value::Object(readable))) = (
        data.get_mut(name),
        views.stored.get(name),
        views.readable.get(name),
    ) else {
        return;
    };

    let views = Views { stored, readable };
    keep_level(fields, data, views, level);
}

/// The rows of a top-level list: each incoming row matched to its stored row
/// by `id` keeps that row's unreadable values. A row the list does not hold —
/// a new one, or a block row whose type changed, which replaces the stored row
/// wholesale — holds nothing stored, so it is judged like an empty stored
/// value: against the list's empty template row of its block type, leaving
/// out whatever the writer could not read there.
fn keep_rows(name: &str, schema: RowSchema<'_>, data: &mut Map<String, Value>, views: Views<'_>) {
    let (Some(Value::Array(rows)), Some(Value::Array(stored)), Some(Value::Array(readable))) = (
        data.get_mut(name),
        views.stored.get(name),
        views.readable.get(name),
    ) else {
        return;
    };

    for row in rows.iter_mut() {
        let Value::Object(row) = row else {
            continue;
        };

        let Some(index) = matched_row_index(stored, row) else {
            continue;
        };

        let (Some(Value::Object(stored_row)), Some(Value::Object(readable_row))) =
            (stored.get(index), readable.get(index))
        else {
            continue;
        };

        let Some(fields) = schema.fields_of(stored_row) else {
            continue;
        };

        let views = Views {
            stored: stored_row,
            readable: readable_row,
        };
        keep_level(fields, row, views, Level::Row);
    }
}

/// The stored row the incoming `row` is judged against: the one with its `id`
/// and block type, else the template row of its block type.
fn matched_row_index(stored: &[Value], row: &Map<String, Value>) -> Option<usize> {
    stored_row_index(stored, row)
        .filter(|&index| stored[index].get(BLOCK_TYPE_KEY) == row.get(BLOCK_TYPE_KEY))
        .or_else(|| template_row_index(stored, row))
}

/// The position of the stored row the incoming `row` updates, by its `id`.
fn stored_row_index(stored: &[Value], row: &Map<String, Value>) -> Option<usize> {
    let id = row.get("id").and_then(Value::as_str)?;

    stored
        .iter()
        .position(|candidate| candidate.get("id").and_then(Value::as_str) == Some(id))
}

/// A list nested inside a row: its rows have no identity to match an incoming
/// row to a stored one, so a list that holds a value the writer cannot read is
/// kept as stored, as a whole.
fn keep_whole_list(name: &str, data: &mut Map<String, Value>, views: Views<'_>) {
    if !data.contains_key(name) || views.stored.get(name) == views.readable.get(name) {
        return;
    }

    match views.stored.get(name) {
        Some(value) if !value.is_null() => {
            data.insert(name.to_string(), value.clone());
        }
        _ => {
            data.remove(name);
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        core::{BlockDefinition, FieldAccess, FieldType, HookRef},
        hooks::lifecycle::access::strip_read_access_data_aware,
    };

    /// A read strip that denies every rule named `deny`, and every rule named
    /// `deny_when_locked` on a level whose stored `locked` is true.
    struct RuleStrip;

    impl FieldReadStrip for RuleStrip {
        fn strip_read_access_map(
            &self,
            fields: &[FieldDefinition],
            level: &mut Map<String, Value>,
            _document: &DocumentFields,
            _collection: &str,
            _user: Option<&Document>,
            _locale: Option<&str>,
        ) {
            let is_denied = |hook: &HookRef, data: &DocumentFields| match hook.reference() {
                "deny" => true,
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

    fn text(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text).build()
    }

    fn object(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            _ => unreachable!("fixture is an object"),
        }
    }

    /// Run the strip for `data` against `stored`, returning the kept write.
    fn keep(fields: &[FieldDefinition], stored: Value, data: Value) -> Value {
        let stored: DocumentFields = object(stored).into_iter().collect();
        let mut data = object(data);

        keep_unreadable(
            &RuleStrip,
            fields,
            &mut data,
            &ReadJudge::builder(&stored, "posts").build(),
        );

        Value::Object(data)
    }

    /// Regression: the admin form submits an unchecked box, an empty list and
    /// an empty input for fields its user may update but not read, and the
    /// write stored them — flipping, clearing and blanking values nobody saw.
    #[test]
    fn a_write_leaves_top_level_values_its_writer_cannot_read_out() {
        let fields = vec![
            text("title"),
            read_gated("verified", FieldType::Checkbox, "deny"),
            read_gated("tags", FieldType::Select, "deny"),
            read_gated("note", FieldType::Text, "deny"),
        ];

        let kept = keep(
            &fields,
            json!({ "title": "Old", "verified": true, "tags": ["a"], "note": "x" }),
            json!({ "title": "New", "verified": false, "tags": [], "note": "" }),
        );

        assert_eq!(kept, json!({ "title": "New" }));
    }

    #[test]
    fn a_hidden_group_sub_field_is_left_out_of_the_group_write() {
        let fields = vec![
            FieldDefinition::builder("internal", FieldType::Group)
                .fields(vec![
                    text("label"),
                    read_gated("note", FieldType::Text, "deny"),
                ])
                .build(),
        ];

        let kept = keep(
            &fields,
            json!({ "internal": { "label": "a", "note": "secret" } }),
            json!({ "internal": { "label": "b", "note": "" } }),
        );

        assert_eq!(kept, json!({ "internal": { "label": "b" } }));
    }

    /// The rule judges the stored row, not the write: a row the writer cannot
    /// read keeps its value although the write claims otherwise, and a row it
    /// can read takes the new value.
    #[test]
    fn an_array_row_sub_field_is_judged_against_its_stored_row() {
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("locked", FieldType::Checkbox).build(),
                    read_gated("secret", FieldType::Text, "deny_when_locked"),
                ])
                .build(),
        ];

        let kept = keep(
            &fields,
            json!({ "items": [
                { "id": "r1", "locked": true, "secret": "s1" },
                { "id": "r2", "locked": false, "secret": "s2" },
            ] }),
            json!({ "items": [
                { "id": "r1", "locked": false, "secret": "" },
                { "id": "r2", "locked": false, "secret": "new" },
                { "secret": "fresh" },
            ] }),
        );

        assert_eq!(
            kept,
            json!({ "items": [
                { "id": "r1", "locked": false },
                { "id": "r2", "locked": false, "secret": "new" },
                { "secret": "fresh" },
            ] })
        );
    }

    /// A row the list does not hold yet has nothing stored, so it is judged
    /// like an empty stored value: a writer who may not read a row field on an
    /// empty row cannot fill it in on a new one. A block row whose type
    /// changed replaces its stored row, so it is judged as new too.
    #[test]
    fn a_new_row_cannot_fill_in_what_its_writer_cannot_read() {
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    text("label"),
                    read_gated("secret", FieldType::Text, "deny"),
                ])
                .build(),
            FieldDefinition::builder("body", FieldType::Blocks)
                .blocks(vec![
                    BlockDefinition::new("quote", vec![text("text")]),
                    BlockDefinition::new(
                        "note",
                        vec![text("text"), read_gated("secret", FieldType::Text, "deny")],
                    ),
                ])
                .build(),
        ];

        let kept = keep(
            &fields,
            json!({ "body": [{ "id": "b1", "_block_type": "quote", "text": "q" }] }),
            json!({
                "items": [{ "label": "new", "secret": "planted" }],
                "body": [{ "id": "b1", "_block_type": "note", "text": "n", "secret": "planted" }],
            }),
        );

        assert_eq!(
            kept,
            json!({
                "items": [{ "label": "new" }],
                "body": [{ "id": "b1", "_block_type": "note", "text": "n" }],
            })
        );
    }

    /// Inside a row a group is one JSON value written whole, so leaving the
    /// hidden leaf out would erase it: it is put back from the stored row.
    #[test]
    fn a_hidden_leaf_inside_a_row_group_is_put_back() {
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("meta", FieldType::Group)
                        .fields(vec![
                            text("label"),
                            read_gated("secret", FieldType::Text, "deny"),
                        ])
                        .build(),
                ])
                .build(),
        ];

        let kept = keep(
            &fields,
            json!({ "items": [{ "id": "r1", "meta": { "label": "a", "secret": "s" } }] }),
            json!({ "items": [{ "id": "r1", "meta": { "label": "b" } }] }),
        );

        assert_eq!(
            kept,
            json!({ "items": [{ "id": "r1", "meta": { "label": "b", "secret": "s" } }] })
        );
    }

    /// A list nested in a row has no row identity, so one holding a hidden
    /// value is kept as stored.
    #[test]
    fn a_nested_list_holding_a_hidden_value_is_kept_whole() {
        let fields = vec![
            FieldDefinition::builder("outer", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("inner", FieldType::Array)
                        .fields(vec![
                            text("label"),
                            read_gated("secret", FieldType::Text, "deny"),
                        ])
                        .build(),
                ])
                .build(),
        ];

        let stored_inner = json!([{ "label": "a", "secret": "s" }]);
        let kept = keep(
            &fields,
            json!({ "outer": [{ "id": "o1", "inner": stored_inner.clone() }] }),
            json!({ "outer": [{ "id": "o1", "inner": [{ "label": "b" }] }] }),
        );

        assert_eq!(kept["outer"][0]["inner"], stored_inner);
    }

    /// An empty stored value is judged too: a writer who may not read the
    /// field cannot fill it in blind.
    #[test]
    fn a_hidden_field_without_a_stored_value_cannot_be_filled_in() {
        let fields = vec![text("title"), read_gated("note", FieldType::Text, "deny")];

        let kept = keep(
            &fields,
            json!({ "title": "t" }),
            json!({ "title": "t", "note": "planted" }),
        );

        assert_eq!(kept, json!({ "title": "t" }));
    }

    #[test]
    fn a_readable_write_is_left_untouched() {
        let fields = vec![
            text("title"),
            read_gated("note", FieldType::Text, "allow"),
            FieldDefinition::builder("layout", FieldType::Row)
                .fields(vec![read_gated("side", FieldType::Text, "allow")])
                .build(),
        ];
        let data = json!({ "title": "t", "note": "n", "side": "s" });

        let kept = keep(
            &fields,
            json!({ "title": "o", "note": "o", "side": "o" }),
            data.clone(),
        );

        assert_eq!(kept, data);
    }
}
