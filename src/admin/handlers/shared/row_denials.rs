//! Which values of an edit form's array/blocks rows its viewer may not read,
//! judged row by row.
//!
//! A field's `access.read` rule judges each row on its own — `ctx.data` is the
//! row — so a data-aware rule can hide a sub-field in one row and show it in
//! the next. The form follows the rule the read and the write follow: a row
//! renders the sub-fields its viewer may read there and no input for the rest,
//! and a new row — the form's template — offers what the rule allows on an
//! empty row, which is exactly what a write may fill into a new row.
//!
//! A stored row is found again by its `id`, so a form re-rendered from a
//! submission (reordered rows included) prunes each row by what it holds. A row
//! without an id is new and pruned like the template. Rows of a list nested
//! inside a row have no identity and are matched by position.

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value};

use crate::{
    admin::context::field::{BlockDefinition, FieldContext},
    core::{
        BLOCK_TYPE_KEY, DocumentFields, FieldChildren, FieldDefinition, FieldDenial,
        field_children, prefixed_name,
    },
    hooks::lifecycle::access::{collect_denials_flat, strip_read_access_data_aware},
    service::{RowSchema, TemplateRows, is_template_row, mark_absent},
};

/// Hidden values of one row, as bracketed paths within it: `[secret]`,
/// `[meta][note]`, `[inner][0][x]`, a nested template `[inner][__INDEX__]…`
/// (`[__INDEX__#type]` for a blocks template).
type RowPaths = HashSet<String>;

/// What an edit form may not render for its viewer: the document-level fields
/// ([`FieldDenial::Flat`], at the top level or in a group) and, row by row, the
/// array/blocks values ([`RowReadDenials`]).
#[derive(Default)]
pub struct FormReadDenials {
    pub flat: Vec<FieldDenial>,
    pub rows: RowReadDenials,
}

impl FormReadDenials {
    /// The document-level denials of `denied` (row-level ones are judged row
    /// by row in `rows`).
    #[must_use]
    pub fn new(denied: Vec<FieldDenial>, rows: RowReadDenials) -> Self {
        let flat = denied
            .into_iter()
            .filter(|denial| matches!(denial, FieldDenial::Flat(_)))
            .collect();

        Self { flat, rows }
    }

    /// Every read-gated field withheld, at every depth and in every row of
    /// `document` — the fail-closed answer when the viewer's access cannot be
    /// judged.
    #[must_use]
    pub fn deny_all(fields: &[FieldDefinition], document: &DocumentFields) -> Self {
        let mut denied = Vec::new();
        collect_denials_flat(
            fields,
            &|f: &FieldDefinition| f.access.read.is_some(),
            "",
            &mut denied,
        );

        let rows = RowReadDenials::judge(fields, document, |probe| {
            strip_read_access_data_aware(fields, probe, &|_, _| true);
        });

        Self::new(denied, rows)
    }
}

/// Per list (keyed by its form name, `items` or `seo__items`): the hidden
/// values of each stored row by its `id`, and of a new row by its block type
/// (`None` for an array).
#[derive(Default)]
pub struct RowReadDenials {
    rows: HashMap<String, HashMap<String, RowPaths>>,
    templates: HashMap<String, HashMap<Option<String>, RowPaths>>,
}

impl RowReadDenials {
    /// Judge the rows of `document` (as the form renders it: groups nested,
    /// rows with their ids): `strip` removes from a probe of the document
    /// every value the viewer may not read, and every row value it removes is
    /// hidden in that row. The probe carries every declared field and an
    /// empty template row in every list (see [`mark_absent`]).
    #[must_use]
    pub fn judge(
        fields: &[FieldDefinition],
        document: &DocumentFields,
        strip: impl FnOnce(&mut Map<String, Value>),
    ) -> Self {
        let mut probe: Map<String, Value> = document.clone().into_inner().into_iter().collect();
        mark_absent(fields, &mut probe, TemplateRows::EveryDepth);

        let mut stripped = probe.clone();
        strip(&mut stripped);

        let mut denials = Self::default();
        denials.collect_level(fields, &probe, &stripped, "");

        denials
    }

    /// Collect the lists of one document level (the document, or a group).
    fn collect_level(
        &mut self,
        fields: &[FieldDefinition],
        probe: &Map<String, Value>,
        stripped: &Map<String, Value>,
        prefix: &str,
    ) {
        for field in fields {
            let name = prefixed_name(prefix, &field.name);

            match field_children(field) {
                FieldChildren::Wrapper(sub) => self.collect_level(sub, probe, stripped, prefix),
                FieldChildren::Tabs(tabs) => {
                    for tab in tabs {
                        self.collect_level(&tab.fields, probe, stripped, prefix);
                    }
                }
                FieldChildren::Group(sub) => {
                    if let (Some(Value::Object(p)), Some(Value::Object(s))) =
                        (probe.get(&field.name), stripped.get(&field.name))
                    {
                        self.collect_level(sub, p, s, &name);
                    }
                }
                FieldChildren::Array(_) | FieldChildren::Blocks(_) => {
                    self.collect_list(field, probe, stripped, &name);
                }
                FieldChildren::Leaf => {}
            }
        }
    }

    /// Collect each row of a document-level list: a stored row by its id, a
    /// template row by its block type.
    fn collect_list(
        &mut self,
        field: &FieldDefinition,
        probe: &Map<String, Value>,
        stripped: &Map<String, Value>,
        list: &str,
    ) {
        let (Some(schema), Some(Value::Array(rows)), Some(Value::Array(kept))) = (
            RowSchema::of(field),
            probe.get(&field.name),
            stripped.get(&field.name),
        ) else {
            return;
        };

        for (row, kept) in rows.iter().zip(kept) {
            let (Value::Object(row), Value::Object(kept)) = (row, kept) else {
                continue;
            };

            let mut hidden = RowPaths::new();
            if let Some(fields) = schema.fields_of(row) {
                collect_row(fields, row, kept, "", &mut hidden);
            }

            if is_template_row(row) {
                let block_type = row.get(BLOCK_TYPE_KEY).and_then(Value::as_str);
                self.templates
                    .entry(list.to_string())
                    .or_default()
                    .insert(block_type.map(str::to_string), hidden);
            } else if let Some(id) = row.get("id").and_then(Value::as_str) {
                self.rows
                    .entry(list.to_string())
                    .or_default()
                    .insert(id.to_string(), hidden);
            }
        }
    }

    /// The hidden values of the row `id` of `list` — or, for a row the
    /// document does not hold, those of a new row of `block_type`.
    fn of_row(&self, list: &str, id: Option<&str>, block_type: Option<&str>) -> Option<&RowPaths> {
        let stored = id.and_then(|id| self.rows.get(list)?.get(id));

        stored.or_else(|| self.template(list, block_type))
    }

    /// The hidden values of a new row of `block_type` in `list`.
    fn template(&self, list: &str, block_type: Option<&str>) -> Option<&RowPaths> {
        self.templates
            .get(list)?
            .get(&block_type.map(str::to_string))
    }

    /// Remove, from the rendered fields of a form, every row input its viewer
    /// may not read in that row — and from each new-row template, every input
    /// its viewer may not read on an empty row.
    pub fn prune(&self, contexts: &mut [FieldContext]) {
        if self.rows.is_empty() && self.templates.is_empty() {
            return;
        }

        for context in contexts {
            self.prune_document_level(context);
        }
    }

    /// Prune one document-level field: descend layout wrappers and groups to
    /// the lists, then each list's rows and templates.
    fn prune_document_level(&self, context: &mut FieldContext) {
        match context {
            FieldContext::Group(f) | FieldContext::Collapsible(f) => self.prune(&mut f.sub_fields),
            FieldContext::Row(f) => self.prune(&mut f.sub_fields),
            FieldContext::Tabs(f) => {
                for tab in &mut f.tabs {
                    self.prune(&mut tab.sub_fields);
                }
            }
            FieldContext::Array(f) => {
                let list = f.base.name.clone();

                for row in f.rows.iter_mut().flatten() {
                    let hidden = self.of_row(&list, row.row_id.as_deref(), None);
                    prune_row(&mut row.sub_fields, hidden, "");
                }
                prune_row(&mut f.sub_fields, self.template(&list, None), "");
            }
            FieldContext::Blocks(f) => {
                let list = f.base.name.clone();

                for row in f.rows.iter_mut().flatten() {
                    let hidden =
                        self.of_row(&list, row.row_id.as_deref(), Some(row.block_type.as_str()));
                    prune_row(&mut row.sub_fields, hidden, "");
                }
                for block in &mut f.block_definitions {
                    let hidden = self.template(&list, Some(block.block_type.as_str()));
                    prune_row(&mut block.fields, hidden, "");
                }
            }
            _ => {}
        }
    }
}

/// Collect the hidden values of one row (or a group inside it) under `prefix`.
fn collect_row(
    fields: &[FieldDefinition],
    probe: &Map<String, Value>,
    stripped: &Map<String, Value>,
    prefix: &str,
    hidden: &mut RowPaths,
) {
    for field in fields {
        let path = format!("{prefix}[{}]", field.name);

        if !matches!(
            field_children(field),
            FieldChildren::Wrapper(_) | FieldChildren::Tabs(_)
        ) && probe.contains_key(&field.name)
            && !stripped.contains_key(&field.name)
        {
            hidden.insert(path);
            continue;
        }

        match field_children(field) {
            FieldChildren::Wrapper(sub) => collect_row(sub, probe, stripped, prefix, hidden),
            FieldChildren::Tabs(tabs) => {
                for tab in tabs {
                    collect_row(&tab.fields, probe, stripped, prefix, hidden);
                }
            }
            FieldChildren::Group(sub) => {
                if let (Some(Value::Object(p)), Some(Value::Object(s))) =
                    (probe.get(&field.name), stripped.get(&field.name))
                {
                    collect_row(sub, p, s, &path, hidden);
                }
            }
            FieldChildren::Array(_) | FieldChildren::Blocks(_) => {
                collect_nested_rows(field, probe, stripped, &path, hidden);
            }
            FieldChildren::Leaf => {}
        }
    }
}

/// Collect the rows of a list nested inside a row, by position — a template
/// row under `[__INDEX__]` (`[__INDEX__#type]` for a block).
fn collect_nested_rows(
    field: &FieldDefinition,
    probe: &Map<String, Value>,
    stripped: &Map<String, Value>,
    path: &str,
    hidden: &mut RowPaths,
) {
    let (Some(schema), Some(Value::Array(rows)), Some(Value::Array(kept))) = (
        RowSchema::of(field),
        probe.get(&field.name),
        stripped.get(&field.name),
    ) else {
        return;
    };

    for (index, (row, kept)) in rows.iter().zip(kept).enumerate() {
        let (Value::Object(row), Value::Object(kept)) = (row, kept) else {
            continue;
        };

        let Some(fields) = schema.fields_of(row) else {
            continue;
        };

        let block_type = row.get(BLOCK_TYPE_KEY).and_then(Value::as_str);
        let segment = if is_template_row(row) {
            template_segment(block_type)
        } else {
            index.to_string()
        };

        collect_row(fields, row, kept, &format!("{path}[{segment}]"), hidden);
    }
}

/// The path segment of a nested list's new-row template.
fn template_segment(block_type: Option<&str>) -> String {
    match block_type {
        Some(block_type) => format!("__INDEX__#{block_type}"),
        None => "__INDEX__".to_string(),
    }
}

/// Remove the inputs `hidden` names from one row's rendered fields (or a
/// group, or a nested row, inside it at `prefix`).
fn prune_row(contexts: &mut Vec<FieldContext>, hidden: Option<&RowPaths>, prefix: &str) {
    let Some(hidden) = hidden.filter(|hidden| !hidden.is_empty()) else {
        return;
    };

    contexts.retain(|context| {
        is_layout(context) || !hidden.contains(&format!("{prefix}[{}]", context.base().field_name))
    });

    for context in contexts {
        prune_row_children(context, hidden, prefix);
    }
}

/// [`prune_row`] inside one kept field of a row.
fn prune_row_children(context: &mut FieldContext, hidden: &RowPaths, prefix: &str) {
    let path = format!("{prefix}[{}]", context.base().field_name);

    match context {
        FieldContext::Collapsible(f) => prune_row(&mut f.sub_fields, Some(hidden), prefix),
        FieldContext::Row(f) => prune_row(&mut f.sub_fields, Some(hidden), prefix),
        FieldContext::Tabs(f) => {
            for tab in &mut f.tabs {
                prune_row(&mut tab.sub_fields, Some(hidden), prefix);
            }
        }
        FieldContext::Group(f) => prune_row(&mut f.sub_fields, Some(hidden), &path),
        FieldContext::Array(f) => {
            for row in f.rows.iter_mut().flatten() {
                prune_row(
                    &mut row.sub_fields,
                    Some(hidden),
                    &format!("{path}[{}]", row.index),
                );
            }
            prune_row(
                &mut f.sub_fields,
                Some(hidden),
                &format!("{path}[__INDEX__]"),
            );
        }
        FieldContext::Blocks(f) => {
            for row in f.rows.iter_mut().flatten() {
                prune_row(
                    &mut row.sub_fields,
                    Some(hidden),
                    &format!("{path}[{}]", row.index),
                );
            }
            prune_block_templates(&mut f.block_definitions, hidden, &path);
        }
        _ => {}
    }
}

/// [`prune_row`] over a nested blocks field's new-row templates.
fn prune_block_templates(blocks: &mut [BlockDefinition], hidden: &RowPaths, path: &str) {
    for block in blocks {
        let segment = template_segment(Some(block.block_type.as_str()));
        prune_row(
            &mut block.fields,
            Some(hidden),
            &format!("{path}[{segment}]"),
        );
    }
}

/// Whether `context` is a layout wrapper, which holds no value of its own.
fn is_layout(context: &FieldContext) -> bool {
    matches!(
        context,
        FieldContext::Row(_) | FieldContext::Collapsible(_) | FieldContext::Tabs(_)
    )
}

#[cfg(test)]
mod tests {
    use std::slice;

    use serde_json::json;

    use super::*;
    use crate::{
        admin::context::field::{
            ArrayField, ArrayRow, BaseFieldData, BlockRow, BlocksField, TextField,
        },
        core::{BlockDefinition as BlockDef, FieldAccess, FieldType, HookRef},
    };

    fn text(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text).build()
    }

    fn gated(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text)
            .access(FieldAccess {
                read: Some(HookRef::from("unless_locked")),
                ..Default::default()
            })
            .build()
    }

    fn schema() -> Vec<FieldDefinition> {
        vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("locked", FieldType::Checkbox).build(),
                    gated("secret"),
                    FieldDefinition::builder("meta", FieldType::Group)
                        .fields(vec![text("label"), gated("note")])
                        .build(),
                ])
                .build(),
            FieldDefinition::builder("body", FieldType::Blocks)
                .blocks(vec![BlockDef::new(
                    "quote",
                    vec![text("text"), gated("source")],
                )])
                .build(),
        ]
    }

    /// A strip whose rule `unless_locked` hides a value on a level whose
    /// `locked` is true — and on a template row, which is never locked, shows it.
    fn unless_locked(fields: &[FieldDefinition]) -> impl FnOnce(&mut Map<String, Value>) + '_ {
        move |probe| {
            strip_read_access_data_aware(fields, probe, &|_, data| {
                data.get("locked") == Some(&json!(true))
            });
        }
    }

    fn document() -> DocumentFields {
        [(
            "items".to_string(),
            json!([
                { "id": "r1", "locked": true, "secret": "s1", "meta": { "label": "a" } },
                { "id": "r2", "locked": false, "secret": "s2", "meta": { "label": "b" } },
            ]),
        )]
        .into_iter()
        .collect()
    }

    fn paths(values: &[&str]) -> RowPaths {
        values.iter().map(|v| (*v).to_string()).collect()
    }

    /// Regression: a data-aware rule on a row sub-field that hides it in
    /// some rows was judged once for the whole document, so the form dropped
    /// the field from every row — or showed an empty input where the value
    /// was hidden. It is judged per row: hidden in the locked row only, and
    /// offered on a new row. A group inside the row is judged on its own
    /// level, as the read strip judges it.
    #[test]
    fn a_row_sub_field_is_judged_row_by_row() {
        let fields = schema();
        let denials = RowReadDenials::judge(&fields, &document(), unless_locked(&fields));

        assert_eq!(
            denials.of_row("items", Some("r1"), None),
            Some(&paths(&["[secret]"]))
        );
        assert_eq!(denials.of_row("items", Some("r2"), None), Some(&paths(&[])));
        assert_eq!(denials.template("items", None), Some(&paths(&[])));
        assert_eq!(
            denials.of_row("items", Some("unknown"), None),
            Some(&paths(&[])),
            "a row the document does not hold is judged like a new one"
        );
    }

    #[test]
    fn deny_all_withholds_every_gated_row_value_and_template_input() {
        let fields = schema();
        let denials = FormReadDenials::deny_all(&fields, &document());

        assert!(denials.flat.is_empty(), "no document-level field is gated");
        assert_eq!(
            denials.rows.of_row("items", Some("r2"), None),
            Some(&paths(&["[secret]", "[meta][note]"]))
        );
        assert_eq!(
            denials.rows.template("body", Some("quote")),
            Some(&paths(&["[source]"]))
        );
    }

    fn base(name: &str) -> BaseFieldData {
        let field_name = name
            .rsplit('[')
            .next()
            .unwrap_or(name)
            .trim_end_matches(']');

        BaseFieldData {
            name: name.to_string(),
            field_name: field_name.to_string(),
            ..Default::default()
        }
    }

    fn input(name: &str) -> FieldContext {
        FieldContext::Text(TextField {
            base: base(name),
            has_many: None,
            tags: None,
        })
    }

    fn array_row(index: usize, id: Option<&str>, names: &[&str]) -> ArrayRow {
        ArrayRow {
            index,
            sub_fields: names.iter().map(|name| input(name)).collect(),
            row_id: id.map(str::to_string),
            ..Default::default()
        }
    }

    fn names(contexts: &[FieldContext]) -> Vec<String> {
        contexts.iter().map(|c| c.base().name.clone()).collect()
    }

    /// Each rendered row is pruned by what its stored row hides — found by
    /// id, so a reordered re-render prunes the right row — and the template
    /// by what a new row hides.
    #[test]
    fn each_rendered_row_is_pruned_by_its_own_row() {
        let fields = schema();
        let denials = FormReadDenials::deny_all(&fields, &document());

        let mut array = ArrayField::empty(base("items"), String::new());
        array.sub_fields = vec![
            input("items[__INDEX__][secret]"),
            input("items[__INDEX__][locked]"),
        ];
        array.rows = Some(vec![
            array_row(0, Some("r2"), &["items[0][secret]", "items[0][locked]"]),
            array_row(1, None, &["items[1][secret]", "items[1][locked]"]),
        ]);
        let mut items = FieldContext::Array(array);

        denials.rows.prune(slice::from_mut(&mut items));

        let FieldContext::Array(items) = items else {
            unreachable!("built as an array");
        };
        let rows = items.rows.unwrap();
        assert_eq!(names(&rows[0].sub_fields), vec!["items[0][locked]"]);
        assert_eq!(names(&rows[1].sub_fields), vec!["items[1][locked]"]);
        assert_eq!(names(&items.sub_fields), vec!["items[__INDEX__][locked]"]);
    }

    /// A blocks row is pruned by its block type's template when it is new.
    #[test]
    fn a_new_block_row_is_pruned_by_its_block_types_template() {
        let fields = schema();
        let denials = FormReadDenials::deny_all(&fields, &DocumentFields::new());

        let mut blocks = BlocksField::empty(base("body"), String::new());
        blocks.rows = Some(vec![BlockRow {
            block_type: "quote".to_string(),
            sub_fields: vec![input("body[0][text]"), input("body[0][source]")],
            ..Default::default()
        }]);
        let mut body = FieldContext::Blocks(blocks);

        denials.rows.prune(slice::from_mut(&mut body));

        let FieldContext::Blocks(body) = body else {
            unreachable!("built as blocks");
        };
        assert_eq!(
            names(&body.rows.unwrap()[0].sub_fields),
            vec!["body[0][text]"]
        );
    }
}
