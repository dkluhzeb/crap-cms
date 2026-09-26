//! Where a collection's or global's has-many lists are stored: every column
//! holding one, and how it holds it.

use std::slice;

use anyhow::Result;

use crate::{
    config::LocaleConfig,
    core::{
        BlockDefinition, Builder, FieldChildren, FieldDefinition, Registry, field_children,
        flatten_array_sub_fields,
    },
    db::{
        migrate::helpers::{block_paths, field_paths, holds_leaf},
        query::{
            helpers::{ListPlace, global_table, join_table, prefixed_name, walk_leaf_fields},
            stored_columns,
        },
    },
};

use super::values::is_list_leaf;

/// A collection or global whose has-many lists are kept in their list form.
pub(super) struct Target<'a> {
    pub(super) slug: &'a str,
    pub(super) table: String,
    pub(super) fields: &'a [FieldDefinition],
    /// A collection, which keeps a search index; a global keeps none.
    pub(super) collection: bool,
}

/// Every collection and global of the registry.
pub(super) fn targets(registry: &Registry) -> Vec<Target<'_>> {
    let collections = registry.collections.iter().map(|(slug, def)| Target {
        slug,
        table: slug.to_string(),
        fields: &def.fields,
        collection: true,
    });
    let globals = registry.globals.iter().map(|(slug, def)| Target {
        slug,
        table: global_table(slug),
        fields: &def.fields,
        collection: false,
    });

    collections.chain(globals).collect()
}

/// How a stored column holds has-many lists.
pub(super) enum Stored {
    /// The column of a has-many field.
    List(Box<FieldDefinition>),
    /// JSON holding a field's value, which holds has-many lists at some depth —
    /// a group, array or blocks field inside an array row.
    Json(Box<FieldDefinition>),
    /// A blocks table's `data`, per block type.
    Blocks(Vec<BlockDefinition>),
}

impl Stored {
    /// What a pass covers in the column, for the gate's fingerprint.
    pub(super) fn signature(&self) -> String {
        match self {
            Self::List(field) | Self::Json(field) => {
                field_paths(slice::from_ref(field.as_ref()), &is_list_leaf)
            }
            Self::Blocks(defs) => block_paths(defs, &is_list_leaf),
        }
    }
}

/// One stored column holding has-many lists.
#[derive(Builder)]
pub(super) struct Column {
    #[builder(required)]
    pub(super) table: String,
    #[builder(required)]
    pub(super) name: String,
    #[builder(required)]
    pub(super) stored: Stored,
    /// A join table's rows belong to a document named by `parent_id`.
    #[builder(default = false)]
    pub(super) join: bool,
}

impl Column {
    /// Where the column's lists live: a document's own column, or a join
    /// table's row (an array row's column, a blocks row's `data`).
    pub(super) fn place(&self) -> ListPlace {
        if self.join {
            ListPlace::Row
        } else {
            ListPlace::Column
        }
    }
}

/// Every column of a target holding has-many lists: main-table columns (one
/// per locale when localized, `group__field` inside a group), array join-table
/// columns — a list of its own or JSON holding one — and blocks `data`.
pub(super) fn list_columns(
    target: &Target<'_>,
    locale_config: &LocaleConfig,
) -> Result<Vec<Column>> {
    let mut columns = Vec::new();

    walk_leaf_fields(target.fields, "", false, &mut |field, prefix, inherited| {
        let leaf = LeafField {
            field,
            prefix,
            inherited,
        };

        push_field_columns(&mut columns, &target.table, &leaf, locale_config)
    })?;

    Ok(columns)
}

/// A field as the leaf walk reaches it: its group `prefix`, and whether an
/// enclosing group is localized (`inherited`).
struct LeafField<'a> {
    field: &'a FieldDefinition,
    prefix: &'a str,
    inherited: bool,
}

/// The columns of one field that hold has-many lists.
fn push_field_columns(
    columns: &mut Vec<Column>,
    table: &str,
    leaf: &LeafField<'_>,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let LeafField {
        field,
        prefix,
        inherited,
    } = *leaf;
    let base = prefixed_name(prefix, &field.name);

    match field_children(field) {
        FieldChildren::Array(sub) => push_array_columns(columns, &join_table(table, &base), sub),
        FieldChildren::Blocks(defs)
            if defs.iter().any(|d| holds_leaf(&d.fields, &is_list_leaf)) =>
        {
            let stored = Stored::Blocks(defs.to_vec());
            let column = Column::builder(join_table(table, &base), "data".to_string(), stored);
            columns.push(column.join(true).build());
        }
        _ if field.is_has_many_scalar() => {
            let localized = inherited || field.localized;

            for name in stored_columns(&base, localized, locale_config)? {
                let stored = Stored::List(Box::new(field.clone()));
                columns.push(Column::builder(table.to_string(), name, stored).build());
            }
        }
        _ => {}
    }

    Ok(())
}

/// The columns of an array join table holding has-many lists: a sub-field's
/// own column, or the JSON of a group, array or blocks inside it.
fn push_array_columns(columns: &mut Vec<Column>, table: &str, sub: &[FieldDefinition]) {
    for sf in flatten_array_sub_fields(sub) {
        let stored = match field_children(sf) {
            FieldChildren::Leaf if is_list_leaf(sf) => Stored::List(Box::new(sf.clone())),
            FieldChildren::Group(_) | FieldChildren::Array(_) | FieldChildren::Blocks(_)
                if holds_leaf(slice::from_ref(sf), &is_list_leaf) =>
            {
                Stored::Json(Box::new(sf.clone()))
            }
            _ => continue,
        };

        let column = Column::builder(table.to_string(), sf.name.clone(), stored);
        columns.push(column.join(true).build());
    }
}
