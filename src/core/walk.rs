//! The canonical field-tree walkers. Every traversal of a `FieldDefinition`
//! tree (or of document data against one) routes through a primitive here, so
//! the container-recursion rules live in exactly one place per traversal family
//! and can't drift between consumers. Three families:
//!
//! - **Data-against-defs** — [`walk_nested`] / [`walk_nested_mut`]: walk JSON
//!   data keyed by bare field names (array rows / block data), where composites
//!   nest as JSON. Used by ref-counting, back-references, the populated-target
//!   read strip.
//! - **Predicate** — [`any_field`]: "does any field, nested anywhere (including
//!   inside Array/Blocks), satisfy a predicate". Used by the field-hook,
//!   field-access, back-ref-target, and FTS-control `has_any` checks.
//! - **Flat-column defs** — [`walk_leaf_fields`]: visit each leaf with its
//!   `group__child` column prefix; Group accumulates the prefix, Row/Collapsible/
//!   Tabs are transparent, Array/Blocks are leaf columns (their sub-fields live
//!   in join tables, not on the parent row). Used by migration DDL, column
//!   collection, locale expansion, FTS field resolution, snapshot keys.
//!   [`flatten_array_sub_fields`] is the wrapper-only flatten used inside arrays.
//! - **Lookup** — [`find_field`]: find one field by name, descending transparent
//!   wrappers only (a Group/Array/Blocks sub-field is addressed through its
//!   container, never by bare name). Used by filter resolution and the
//!   form-flatten Group check.
//! - **Schema shape** — [`walk_all_fields`]: visit every field, containers
//!   included, descending uniformly into sub-fields/blocks/tabs with the
//!   ancestor chain ([`SchemaStep`]). No column semantics. Used by startup
//!   schema checks (name collisions, hook-ref validation) and the
//!   nesting-depth warning.
//!
//! Every walker above dispatches on one shared classifier, [`field_children`],
//! which maps a [`FieldType`] to its structural sub-tree ([`FieldChildren`]) —
//! so the "which field types have which children" knowledge lives in exactly one
//! place and each walker's match is **exhaustive**: adding a new composite field
//! type is a compile error in every walker rather than a silently-skipped
//! subtree. What differs per walker is only what it *does* with each kind (the
//! column walker treats Array/Blocks as leaves; the lookup walker treats
//! Group/Array/Blocks as opaque; the data walkers pull the matching JSON value).
//!
//! Two consumers deliberately keep bespoke recursion — validation and field
//! hooks — because they need *container-level* context a leaf visitor can't hand
//! back: validation builds qualified error paths with array indices and a
//! per-row validate context; field hooks need a per-row `ctx.data` snapshot and
//! conditional write-back. They follow the same rules, guarded by their tests.
//!
//! ---
//!
//! The data walkers ([`walk_nested`] / [`walk_nested_mut`]) follow the Rust
//! ecosystem's settled answer to "one traversal, read or mutate" — `syn`'s
//! `Visit` / `VisitMut`, `serde`'s `Serialize` / `Deserialize`: a **pair**
//! sharing [`NestStep`], the container rules, and a lockstep test
//! (`read_and_mutate_walkers_visit_identically`) that proves they can't drift.
//! Read takes `&R`, mutate takes `&mut R` with the visitor returning a
//! [`VisitAction`]. Trying to unify `&`/`&mut` behind one generic walker needs
//! generics-over-mutability and buys nothing. Both are generic over the root map
//! ([`JsonRoot`]) so the entry can be a [`crate::core::DocumentFields`] map, a
//! `serde_json::Map`, or an event payload; nested composites are always
//! `serde_json::Map`.

use anyhow::Result;
use serde_json::{Map, Value};

use super::field::{BLOCK_TYPE_KEY, BlockDefinition, FieldDefinition, FieldTab, FieldType};
use super::field_denial::JsonRoot;

/// One segment of the path from a walk's root to the field being visited.
///
/// Transparent layout wrappers (Row/Collapsible/Tabs) contribute no segment;
/// Group/Array push their own [`FieldDefinition`]; Blocks push their field then
/// the matched [`BlockDefinition`].
#[derive(Debug, Clone, Copy)]
pub enum NestStep<'a> {
    Field(&'a FieldDefinition),
    Block(&'a BlockDefinition),
}

/// What [`walk_nested_mut`] should do with the visited field's value.
#[derive(Debug, Clone)]
pub enum VisitAction {
    /// Leave the value as-is.
    Keep,
    /// Remove the field from its containing object. A removed container is not
    /// recursed into.
    Remove,
    /// Replace the field's value (e.g. a field hook transformed it).
    Replace(Value),
}

/// The structural children of a field — the one classification of "what kind of
/// sub-tree does this field own", derived from its [`FieldType`].
///
/// Every walker below matches on this instead of re-spelling
/// `match field.field_type { Group | Array | Blocks | Row | Collapsible | Tabs }`,
/// so the "which field types have which children" mapping lives in exactly one
/// place ([`field_children`]) and — because the matches are **exhaustive** — a
/// new composite `FieldType` becomes a compile error in every walker rather than
/// a silently-skipped subtree. The variants distinguish everything any walker
/// needs: `Group` (own key, prefixes columns), `Wrapper` (Row/Collapsible —
/// transparent), `Tabs` (transparent, per-tab), `Array`/`Blocks` (repeatable
/// rows — a relational-spine leaf to the column walker, descended by the others).
enum FieldChildren<'a> {
    /// A scalar field — no sub-tree.
    Leaf,
    /// A `Group`: its sub-fields nest under the group's key (`group__child`).
    Group(&'a [FieldDefinition]),
    /// A `Row`/`Collapsible` layout wrapper: transparent — its sub-fields live in
    /// the same object / column namespace as the wrapper itself.
    Wrapper(&'a [FieldDefinition]),
    /// A `Tabs` wrapper: transparent, with the sub-fields split across tabs.
    Tabs(&'a [FieldTab]),
    /// An `Array`: repeatable rows over a shared sub-field set.
    Array(&'a [FieldDefinition]),
    /// A `Blocks` field: repeatable rows, each matched to a block definition.
    Blocks(&'a [BlockDefinition]),
}

/// Classify a field's structural children — the single source of the
/// `FieldType` → sub-tree mapping shared by every walker in this module.
fn field_children(field: &FieldDefinition) -> FieldChildren<'_> {
    match field.field_type {
        FieldType::Group => FieldChildren::Group(&field.fields),
        FieldType::Row | FieldType::Collapsible => FieldChildren::Wrapper(&field.fields),
        FieldType::Tabs => FieldChildren::Tabs(&field.tabs),
        FieldType::Array => FieldChildren::Array(&field.fields),
        FieldType::Blocks => FieldChildren::Blocks(&field.blocks),
        _ => FieldChildren::Leaf,
    }
}

/// Read-walk a nested-composite JSON object against its field defs.
///
/// Calls `visit(field, value, path)` for EVERY field — leaf and container — with
/// the field's current value (if present) and the path of ancestor containers,
/// then recurses containers per the canonical rules: Group → nested object;
/// Array → each row; Blocks → each instance matched to its definition by
/// `_block_type`; Row/Collapsible/Tabs → transparent (same object, no path
/// segment); leaves → nothing further. Side outputs (collected refs, validation
/// errors) flow through the visitor's own captured state.
///
/// Generic over the root map ([`JsonRoot`]) so the entry can be a
/// [`crate::core::DocumentFields`] map, a `serde_json::Map`, or a live-event
/// payload; nested composites are always `serde_json::Map`.
pub fn walk_nested<'a, R, V>(
    obj: &R,
    fields: &'a [FieldDefinition],
    path: &mut Vec<NestStep<'a>>,
    visit: &mut V,
) where
    R: JsonRoot + ?Sized,
    V: FnMut(&'a FieldDefinition, Option<&Value>, &[NestStep<'a>]),
{
    for field in fields {
        visit(field, obj.root_get(&field.name), path);

        match field_children(field) {
            FieldChildren::Leaf => {}
            FieldChildren::Group(subs) => {
                if let Some(Value::Object(inner)) = obj.root_get(&field.name) {
                    path.push(NestStep::Field(field));
                    walk_nested(inner, subs, path, visit);
                    path.pop();
                }
            }
            FieldChildren::Array(subs) => {
                if let Some(Value::Array(rows)) = obj.root_get(&field.name) {
                    path.push(NestStep::Field(field));
                    for row in rows {
                        if let Value::Object(row_obj) = row {
                            walk_nested(row_obj, subs, path, visit);
                        }
                    }
                    path.pop();
                }
            }
            FieldChildren::Blocks(defs) => {
                if let Some(Value::Array(blocks)) = obj.root_get(&field.name) {
                    path.push(NestStep::Field(field));
                    for block in blocks {
                        if let Some((def, block_obj)) = match_block(block, defs) {
                            path.push(NestStep::Block(def));
                            walk_nested(block_obj, &def.fields, path, visit);
                            path.pop();
                        }
                    }
                    path.pop();
                }
            }
            FieldChildren::Wrapper(subs) => {
                walk_nested(obj, subs, path, visit);
            }
            FieldChildren::Tabs(tabs) => {
                for tab in tabs {
                    walk_nested(obj, &tab.fields, path, visit);
                }
            }
        }
    }
}

/// Mutate-walk a nested-composite JSON object against its field defs.
///
/// The mutating twin of [`walk_nested`]: same traversal and path semantics, but
/// `visit` returns a [`VisitAction`] that is applied before recursion. A field
/// the visitor `Remove`s (or `Replace`s with a shape the container no longer
/// matches) is not recursed into. Generic over the root map ([`JsonRoot`]) just
/// like [`walk_nested`].
pub fn walk_nested_mut<'a, R, V>(
    obj: &mut R,
    fields: &'a [FieldDefinition],
    path: &mut Vec<NestStep<'a>>,
    visit: &mut V,
) where
    R: JsonRoot + ?Sized,
    V: FnMut(&'a FieldDefinition, Option<&Value>, &[NestStep<'a>]) -> VisitAction,
{
    for field in fields {
        match visit(field, obj.root_get(&field.name), path) {
            VisitAction::Keep => {}
            VisitAction::Remove => {
                obj.root_remove(&field.name);
                continue;
            }
            VisitAction::Replace(value) => {
                obj.root_insert(field.name.clone(), value);
            }
        }

        match field_children(field) {
            FieldChildren::Leaf => {}
            FieldChildren::Group(subs) => {
                if let Some(Value::Object(inner)) = obj.root_get_mut(&field.name) {
                    path.push(NestStep::Field(field));
                    walk_nested_mut(inner, subs, path, visit);
                    path.pop();
                }
            }
            FieldChildren::Array(subs) => {
                if let Some(Value::Array(rows)) = obj.root_get_mut(&field.name) {
                    path.push(NestStep::Field(field));
                    for row in rows.iter_mut() {
                        if let Value::Object(row_obj) = row {
                            walk_nested_mut(row_obj, subs, path, visit);
                        }
                    }
                    path.pop();
                }
            }
            FieldChildren::Blocks(defs) => {
                if let Some(Value::Array(blocks)) = obj.root_get_mut(&field.name) {
                    path.push(NestStep::Field(field));
                    walk_block_instances_mut(blocks, defs, path, visit);
                    path.pop();
                }
            }
            FieldChildren::Wrapper(subs) => {
                walk_nested_mut(obj, subs, path, visit);
            }
            FieldChildren::Tabs(tabs) => {
                for tab in tabs {
                    walk_nested_mut(obj, &tab.fields, path, visit);
                }
            }
        }
    }
}

/// Resolve a block instance to its `(definition, object)` by matching
/// `_block_type`, or `None` if the value isn't an object, carries no
/// `_block_type`, or names an unknown block. Shared by the read walker.
fn match_block<'a, 'v>(
    block: &'v Value,
    defs: &'a [BlockDefinition],
) -> Option<(&'a BlockDefinition, &'v Map<String, Value>)> {
    let block_obj = block.as_object()?;
    let block_type = block_obj.get(BLOCK_TYPE_KEY).and_then(Value::as_str)?;
    let def = defs.iter().find(|d| d.block_type == block_type)?;

    Some((def, block_obj))
}

/// Mutate-walk a list of block instances, matching each to its definition by
/// `_block_type` and recursing into its fields. The `&mut` counterpart of the
/// read walker's inline [`match_block`] loop.
fn walk_block_instances_mut<'a, V>(
    blocks: &mut [Value],
    defs: &'a [BlockDefinition],
    path: &mut Vec<NestStep<'a>>,
    visit: &mut V,
) where
    V: FnMut(&'a FieldDefinition, Option<&Value>, &[NestStep<'a>]) -> VisitAction,
{
    for block in blocks.iter_mut() {
        let Value::Object(block_obj) = block else {
            continue;
        };

        let Some(block_type) = block_obj.get(BLOCK_TYPE_KEY).and_then(Value::as_str) else {
            continue;
        };
        // Resolve the def first so `block_obj` is free to borrow mutably below.
        let Some(def) = defs.iter().find(|d| d.block_type == block_type) else {
            continue;
        };

        path.push(NestStep::Block(def));
        walk_nested_mut(block_obj, &def.fields, path, visit);
        path.pop();
    }
}

/// Returns `true` if `pred` holds for any field in the tree, descending every
/// composite (Group/Array/Row/Collapsible → `fields`, Tabs → `tabs`, Blocks →
/// `blocks`) at any depth.
///
/// The single source of truth for "does any field, nested anywhere, satisfy a
/// predicate" — shared by the field-hook, field-access, back-reference, and
/// read-control `has_any` checks so they cannot disagree on which composites to
/// descend. `pred` is checked on every field (leaf and container); a container
/// for which `pred` is false is still descended.
#[must_use]
pub fn any_field<F: Fn(&FieldDefinition) -> bool>(fields: &[FieldDefinition], pred: &F) -> bool {
    fields.iter().any(|field| {
        pred(field)
            || match field_children(field) {
                FieldChildren::Leaf => false,
                FieldChildren::Group(subs)
                | FieldChildren::Wrapper(subs)
                | FieldChildren::Array(subs) => any_field(subs, pred),
                FieldChildren::Tabs(tabs) => tabs.iter().any(|tab| any_field(&tab.fields, pred)),
                FieldChildren::Blocks(defs) => defs.iter().any(|b| any_field(&b.fields, pred)),
            }
    })
}

/// Recursively flatten layout wrappers (Row, Collapsible, Tabs) to extract leaf
/// fields. Used by Array join-table DDL, read, write, and form parsing — layout
/// wrappers are transparent inside arrays, so their children are promoted as
/// individual columns. Does NOT descend Group/Array/Blocks (those are opaque
/// from the flattening site).
#[must_use]
pub fn flatten_array_sub_fields(fields: &[FieldDefinition]) -> Vec<&FieldDefinition> {
    let mut result = Vec::new();
    for f in fields {
        match field_children(f) {
            FieldChildren::Wrapper(subs) => {
                result.extend(flatten_array_sub_fields(subs));
            }
            FieldChildren::Tabs(tabs) => {
                for tab in tabs {
                    result.extend(flatten_array_sub_fields(&tab.fields));
                }
            }
            // Group/Array/Blocks are opaque to the array flatten — pushed whole.
            FieldChildren::Leaf
            | FieldChildren::Group(_)
            | FieldChildren::Array(_)
            | FieldChildren::Blocks(_) => result.push(f),
        }
    }
    result
}

/// Build a prefixed column name: `"prefix__name"`, or just `"name"` when prefix
/// is empty. The `__`-joining used everywhere a group's sub-field maps to a flat
/// parent-table column.
#[must_use]
pub(crate) fn prefixed_name(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}__{name}")
    }
}

/// Walk a field tree, calling `visit` for each leaf (non-layout-container) field
/// with its flat-column prefix and inherited localization.
///
/// The flat-column traversal: Group recurses with a `parent__` column prefix and
/// propagates `localized` to its children; Row/Collapsible/Tabs are transparent
/// (same prefix); Array/Blocks are visited as **leaf columns** (their sub-fields
/// live in join tables, not on the parent row), so the visitor decides whether
/// to skip them. The visitor receives `(field, prefix, inherited_localized)`.
///
/// # Errors
///
/// Propagates any error the visitor returns.
pub(crate) fn walk_leaf_fields<'a, F>(
    fields: &'a [FieldDefinition],
    prefix: &str,
    inherited_localized: bool,
    visit: &mut F,
) -> Result<()>
where
    F: FnMut(&'a FieldDefinition, &str, bool) -> Result<()>,
{
    for field in fields {
        match field_children(field) {
            FieldChildren::Group(subs) => {
                let new_prefix = prefixed_name(prefix, &field.name);

                walk_leaf_fields(
                    subs,
                    &new_prefix,
                    inherited_localized || field.localized,
                    visit,
                )?;
            }
            FieldChildren::Wrapper(subs) => {
                walk_leaf_fields(subs, prefix, inherited_localized, visit)?;
            }
            FieldChildren::Tabs(tabs) => {
                for tab in tabs {
                    walk_leaf_fields(&tab.fields, prefix, inherited_localized, visit)?;
                }
            }
            // Array/Blocks are leaf columns here (their sub-fields live in join
            // tables, not on the parent row), so they are visited, not descended.
            FieldChildren::Leaf | FieldChildren::Array(_) | FieldChildren::Blocks(_) => {
                visit(field, prefix, inherited_localized)?;
            }
        }
    }

    Ok(())
}

/// One segment of the ancestor chain in a [`walk_all_fields`] schema walk.
///
/// Unlike [`NestStep`] (data walks, where wrappers are transparent and blocks
/// are matched by `_block_type`), a schema walk records EVERY container it
/// descends through: the container field itself, plus the block definition or
/// tab it entered.
#[derive(Debug, Clone, Copy)]
pub(crate) enum SchemaStep<'a> {
    /// The container field being descended into (Group/Array/Blocks/wrapper —
    /// any field with sub-fields).
    Field(&'a FieldDefinition),
    /// A block definition entered under a Blocks field.
    Block(&'a BlockDefinition),
    /// A tab entered under a Tabs field.
    Tab { index: usize, tab: &'a FieldTab },
}

/// Visit EVERY field in a definition tree — containers and leaves alike —
/// descending uniformly into sub-fields, every block's fields, and every tab's
/// fields. The visitor receives the field and the ancestor chain
/// ([`SchemaStep`]s) leading to it; a field's nesting depth is
/// `1 + path.iter().filter(Field).count()` (block/tab steps share their
/// owning field's level).
///
/// This walk has NO column semantics — no `__` prefixing, no wrapper
/// transparency, no join-table leaf cutoff. It is for schema-shape checks
/// (name collisions, hook-ref validation, nesting-depth limits); use
/// [`walk_leaf_fields`] for anything that maps fields to columns.
pub(crate) fn walk_all_fields<'a, V>(
    fields: &'a [FieldDefinition],
    path: &mut Vec<SchemaStep<'a>>,
    visit: &mut V,
) where
    V: FnMut(&'a FieldDefinition, &[SchemaStep<'a>]),
{
    for field in fields {
        visit(field, path);

        match field_children(field) {
            FieldChildren::Leaf => {}
            // Every field with a flat sub-field list (Group, wrapper, Array) is
            // descended under a single container step.
            FieldChildren::Group(subs)
            | FieldChildren::Wrapper(subs)
            | FieldChildren::Array(subs) => {
                path.push(SchemaStep::Field(field));
                walk_all_fields(subs, path, visit);
                path.pop();
            }
            FieldChildren::Blocks(defs) => {
                for block in defs {
                    path.push(SchemaStep::Field(field));
                    path.push(SchemaStep::Block(block));
                    walk_all_fields(&block.fields, path, visit);
                    path.pop();
                    path.pop();
                }
            }
            FieldChildren::Tabs(tabs) => {
                for (index, tab) in tabs.iter().enumerate() {
                    path.push(SchemaStep::Field(field));
                    path.push(SchemaStep::Tab { index, tab });
                    walk_all_fields(&tab.fields, path, visit);
                    path.pop();
                    path.pop();
                }
            }
        }
    }
}

/// Find a field by name, descending transparent layout wrappers
/// (Row/Collapsible/Tabs). Group/Array/Blocks are NOT descended — a sub-field
/// is addressed through its container (`group__child` column, join-table row),
/// never by bare name at the parent level.
pub(crate) fn find_field<'a>(
    name: &str,
    fields: &'a [FieldDefinition],
) -> Option<&'a FieldDefinition> {
    for field in fields {
        if field.name == name {
            return Some(field);
        }

        // Only transparent wrappers are descended; a Group/Array/Blocks sub-field
        // is addressed through its container, never by bare name at this level.
        let found = match field_children(field) {
            FieldChildren::Wrapper(subs) => find_field(name, subs),
            FieldChildren::Tabs(tabs) => tabs.iter().find_map(|tab| find_field(name, &tab.fields)),
            FieldChildren::Leaf
            | FieldChildren::Group(_)
            | FieldChildren::Array(_)
            | FieldChildren::Blocks(_) => None,
        };

        if found.is_some() {
            return found;
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::core::field::{FieldTab, RelationshipConfig};

    fn rel(name: &str, has_many: bool) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Relationship)
            .relationship(RelationshipConfig::new("tags", has_many))
            .build()
    }

    fn dotted(field: &FieldDefinition, path: &[NestStep<'_>]) -> String {
        let mut parts: Vec<String> = path
            .iter()
            .map(|s| match s {
                NestStep::Field(f) => f.name.clone(),
                NestStep::Block(b) => b.block_type.clone(),
            })
            .collect();
        parts.push(field.name.clone());
        parts.join(".")
    }

    /// Collect `(dotted_path, value)` for every field the READ walker visits.
    fn trace_read(value: &Value, fields: &[FieldDefinition]) -> Vec<(String, Value)> {
        let obj = value.as_object().unwrap().clone();
        let mut seen = Vec::new();
        let mut path = Vec::new();
        walk_nested(&obj, fields, &mut path, &mut |field, val, path| {
            seen.push((dotted(field, path), val.cloned().unwrap_or(Value::Null)));
        });
        seen
    }

    /// Same, for the MUTATE walker driven as a pure reader (`Keep` everywhere).
    fn trace_mut(value: &Value, fields: &[FieldDefinition]) -> Vec<(String, Value)> {
        let mut obj = value.as_object().unwrap().clone();
        let mut seen = Vec::new();
        let mut path = Vec::new();
        walk_nested_mut(&mut obj, fields, &mut path, &mut |field, val, path| {
            seen.push((dotted(field, path), val.cloned().unwrap_or(Value::Null)));
            VisitAction::Keep
        });
        seen
    }

    /// A tree exercising every container shape, including array-in-array.
    fn nesting_matrix() -> (Vec<FieldDefinition>, Value) {
        let fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![rel("tag", false)])
                .build(),
            FieldDefinition::builder("row", FieldType::Row)
                .fields(vec![FieldDefinition::builder("x", FieldType::Text).build()])
                .build(),
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("label", FieldType::Text).build(),
                    FieldDefinition::builder("inner", FieldType::Array)
                        .fields(vec![rel("deep", true)])
                        .build(),
                ])
                .build(),
            FieldDefinition::builder("content", FieldType::Blocks)
                .blocks(vec![BlockDefinition::new(
                    "hero",
                    vec![FieldDefinition::builder("heading", FieldType::Text).build()],
                )])
                .build(),
        ];
        let data = json!({
            "title": "Hi",
            "seo": { "tag": "t1" },
            "x": "v",
            "items": [
                { "label": "a", "inner": [{ "deep": ["d1", "d2"] }] },
                { "label": "b", "inner": [] }
            ],
            "content": [
                { "_block_type": "hero", "heading": "H" },
                { "_block_type": "unknown", "heading": "skip" }
            ]
        });
        (fields, data)
    }

    #[test]
    fn visits_every_field_with_path_through_group_array_blocks() {
        let (fields, data) = nesting_matrix();
        let seen = trace_read(&data, &fields);
        assert_eq!(
            seen.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(),
            vec![
                "title",
                "seo",
                "seo.tag",
                "row", // visited (no own data key → Null) then transparent
                "x",
                "items",
                "items.label",
                "items.inner",
                "items.inner.deep", // array-in-array reaches the deep leaf
                "items.label",
                "items.inner", // second row's empty inner array: visited, no rows
                "content",
                "content.hero.heading", // only the matching block def recurses
            ]
        );
    }

    #[test]
    fn read_and_mutate_walkers_visit_identically() {
        // The lockstep guarantee: the read and mutate twins traverse the same
        // tree in the same order with the same paths and values. If a future
        // edit changes one walker's container rules without the other, this
        // fails — they cannot silently drift.
        let (fields, data) = nesting_matrix();
        assert_eq!(trace_read(&data, &fields), trace_mut(&data, &fields));
    }

    #[test]
    fn layout_wrappers_are_transparent() {
        let fields = vec![
            FieldDefinition::builder("row", FieldType::Row)
                .fields(vec![FieldDefinition::builder("x", FieldType::Text).build()])
                .build(),
        ];
        let data = json!({ "x": "v" });
        let seen = trace_read(&data, &fields);
        assert_eq!(
            seen,
            vec![("row".into(), Value::Null), ("x".into(), json!("v"))]
        );
    }

    #[test]
    fn remove_action_deletes_field_and_skips_its_recursion() {
        let fields = vec![
            FieldDefinition::builder("keep", FieldType::Text).build(),
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("secret", FieldType::Text).build(),
                ])
                .build(),
        ];
        let mut obj = json!({ "keep": "k", "seo": { "secret": "s" } })
            .as_object()
            .unwrap()
            .clone();
        let mut path = Vec::new();
        let mut visited_secret = false;
        walk_nested_mut(&mut obj, &fields, &mut path, &mut |field, _v, _p| {
            if field.name == "seo" {
                return VisitAction::Remove;
            }
            if field.name == "secret" {
                visited_secret = true;
            }
            VisitAction::Keep
        });

        assert!(!obj.contains_key("seo"), "removed container is gone");
        assert!(
            !visited_secret,
            "a removed container's children must not be visited"
        );
        assert_eq!(obj.get("keep"), Some(&json!("k")));
    }

    #[test]
    fn replace_action_swaps_value() {
        let fields = vec![FieldDefinition::builder("n", FieldType::Number).build()];
        let mut obj = json!({ "n": 1 }).as_object().unwrap().clone();
        let mut path = Vec::new();
        walk_nested_mut(&mut obj, &fields, &mut path, &mut |_f, _v, _p| {
            VisitAction::Replace(json!(42))
        });
        assert_eq!(obj.get("n"), Some(&json!(42)));
    }

    // ── flatten_array_sub_fields ─────────────────────────────────────

    fn text_field(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text).build()
    }

    #[test]
    fn flatten_array_sub_fields_flattens_row_and_tabs() {
        let fields = vec![
            text_field("title"),
            FieldDefinition::builder("layout", FieldType::Row)
                .fields(vec![text_field("slug"), text_field("author")])
                .build(),
            FieldDefinition::builder("settings", FieldType::Tabs)
                .tabs(vec![
                    FieldTab::new("General", vec![text_field("color")]),
                    FieldTab::new("Advanced", vec![text_field("cache")]),
                ])
                .build(),
        ];
        let names: Vec<&str> = flatten_array_sub_fields(&fields)
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert_eq!(names, vec!["title", "slug", "author", "color", "cache"]);
    }

    #[test]
    fn flatten_array_sub_fields_does_not_descend_group() {
        // A Group is opaque to the array-flatten — it's pushed whole, not split.
        let fields = vec![
            FieldDefinition::builder("g", FieldType::Group)
                .fields(vec![text_field("inner")])
                .build(),
        ];
        let names: Vec<&str> = flatten_array_sub_fields(&fields)
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert_eq!(names, vec!["g"]);
    }

    // ── any_field ────────────────────────────────────────────────────

    #[test]
    fn any_field_finds_predicate_at_every_depth() {
        let needle = |f: &FieldDefinition| f.name == "needle";
        // inside a group inside a tab inside an array → still found.
        let fields = vec![
            FieldDefinition::builder("arr", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("tabs", FieldType::Tabs)
                        .tabs(vec![FieldTab::new(
                            "T",
                            vec![
                                FieldDefinition::builder("g", FieldType::Group)
                                    .fields(vec![text_field("needle")])
                                    .build(),
                            ],
                        )])
                        .build(),
                ])
                .build(),
        ];
        assert!(any_field(&fields, &needle));
        assert!(!any_field(&fields, &|f| f.name == "absent"));
    }

    #[test]
    fn any_field_descends_blocks() {
        let fields = vec![
            FieldDefinition::builder("content", FieldType::Blocks)
                .blocks(vec![BlockDefinition::new(
                    "hero",
                    vec![text_field("needle")],
                )])
                .build(),
        ];
        assert!(any_field(&fields, &|f| f.name == "needle"));
    }

    // ── walk_leaf_fields ─────────────────────────────────────────────

    #[test]
    fn walk_leaf_fields_prefixes_groups_and_skips_array_descent() {
        let fields = vec![
            text_field("title"),
            FieldDefinition::builder("seo", FieldType::Group)
                .localized(true)
                .fields(vec![text_field("desc")])
                .build(),
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![text_field("label")])
                .build(),
        ];
        let mut seen = Vec::new();
        walk_leaf_fields(&fields, "", false, &mut |field, prefix, loc| {
            seen.push((prefixed_name(prefix, &field.name), loc));
            Ok(())
        })
        .unwrap();
        // Group → prefixed + localized inherited; Array visited as a leaf column
        // (its `label` sub-field is NOT descended — it lives in a join table).
        assert_eq!(
            seen,
            vec![
                ("title".to_string(), false),
                ("seo__desc".to_string(), true),
                ("items".to_string(), false),
            ]
        );
    }

    #[test]
    fn walk_all_fields_visits_every_field_with_ancestor_path() {
        let fields = vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![text_field("title")])
                .build(),
            FieldDefinition::builder("content", FieldType::Blocks)
                .blocks(vec![BlockDefinition::new(
                    "hero",
                    vec![text_field("heading")],
                )])
                .build(),
            FieldDefinition::builder("layout", FieldType::Tabs)
                .tabs(vec![FieldTab::new("Main", vec![text_field("body")])])
                .build(),
        ];

        let mut seen = Vec::new();
        let mut path = Vec::new();
        walk_all_fields(&fields, &mut path, &mut |field, path| {
            let rendered: Vec<String> = path
                .iter()
                .map(|s| match s {
                    SchemaStep::Field(f) => f.name.clone(),
                    SchemaStep::Block(b) => format!("block:{}", b.block_type),
                    SchemaStep::Tab { index, .. } => format!("tab:{index}"),
                })
                .collect();
            let depth = 1 + path
                .iter()
                .filter(|s| matches!(s, SchemaStep::Field(_)))
                .count();
            seen.push((field.name.clone(), rendered.join("/"), depth));
        });

        // Every field visited — containers included — with full ancestor chain;
        // block/tab steps share their owning field's depth level.
        assert_eq!(
            seen,
            vec![
                ("seo".to_string(), String::new(), 1),
                ("title".to_string(), "seo".to_string(), 2),
                ("content".to_string(), String::new(), 1),
                ("heading".to_string(), "content/block:hero".to_string(), 2),
                ("layout".to_string(), String::new(), 1),
                ("body".to_string(), "layout/tab:0".to_string(), 2),
            ]
        );
    }

    #[test]
    fn find_field_descends_wrappers_but_not_groups() {
        let fields = vec![
            text_field("title"),
            FieldDefinition::builder("row", FieldType::Row)
                .fields(vec![text_field("in_row")])
                .build(),
            FieldDefinition::builder("tabs", FieldType::Tabs)
                .tabs(vec![FieldTab::new("SEO", vec![text_field("in_tab")])])
                .build(),
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![text_field("in_group")])
                .build(),
        ];

        // Top-level and wrapper-nested fields resolve by bare name.
        assert!(find_field("title", &fields).is_some());
        assert!(find_field("in_row", &fields).is_some());
        assert!(find_field("in_tab", &fields).is_some());

        // Containers match by their own name; their sub-fields do not leak out.
        assert_eq!(
            find_field("seo", &fields).map(|f| &f.field_type),
            Some(&FieldType::Group)
        );
        assert!(find_field("in_group", &fields).is_none());
        assert!(find_field("missing", &fields).is_none());
    }

    /// The shared classifier maps each `FieldType` to the structural kind every
    /// walker dispatches on. If this drifts, every walker changes at once —
    /// which is the point (one source, exhaustive matches).
    #[test]
    fn field_children_classifies_each_field_type() {
        let group = FieldDefinition::builder("g", FieldType::Group)
            .fields(vec![text_field("x")])
            .build();
        let row = FieldDefinition::builder("r", FieldType::Row)
            .fields(vec![text_field("x")])
            .build();
        let collapsible = FieldDefinition::builder("c", FieldType::Collapsible)
            .fields(vec![text_field("x")])
            .build();
        let tabs = FieldDefinition::builder("t", FieldType::Tabs)
            .tabs(vec![FieldTab::new("T", vec![text_field("x")])])
            .build();
        let array = FieldDefinition::builder("a", FieldType::Array)
            .fields(vec![text_field("x")])
            .build();
        let blocks = FieldDefinition::builder("b", FieldType::Blocks)
            .blocks(vec![BlockDefinition::new("hero", vec![text_field("x")])])
            .build();
        let leaf = text_field("title");

        assert!(matches!(field_children(&group), FieldChildren::Group(_)));
        assert!(matches!(field_children(&row), FieldChildren::Wrapper(_)));
        assert!(matches!(
            field_children(&collapsible),
            FieldChildren::Wrapper(_)
        ));
        assert!(matches!(field_children(&tabs), FieldChildren::Tabs(_)));
        assert!(matches!(field_children(&array), FieldChildren::Array(_)));
        assert!(matches!(field_children(&blocks), FieldChildren::Blocks(_)));
        assert!(matches!(field_children(&leaf), FieldChildren::Leaf));
    }
}
