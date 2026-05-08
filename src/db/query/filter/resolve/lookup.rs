//! Field-tree lookups shared by normalize and resolve_filter paths.

use crate::core::{FieldDefinition, FieldType};

/// Look up the [`FieldType`] for a DB column name on the parent table.
///
/// Handles:
/// - Plain top-level fields (`"status"` → `FieldType::Text`)
/// - Transparent layout wrappers (Row/Collapsible/Tabs)
/// - Group sub-fields using the `{group}__{sub}` double-underscore naming
///   (including nested groups: `a__b__c`)
/// - Optional locale suffix (`field__{locale}`, `group__sub__{locale}`) —
///   the locale segment is the last path component and does not affect the
///   leaf field type.
///
/// Returns `None` when the column cannot be mapped to a known field —
/// callers fall back to `DbValue::Text` binding.
pub(super) fn lookup_column_field_type(col: &str, fields: &[FieldDefinition]) -> Option<FieldType> {
    // Fast path: a top-level scalar/layout leaf named exactly `col`.
    if let Some(f) = find_field_recursive(col, fields)
        && !matches!(
            f.field_type,
            FieldType::Group | FieldType::Array | FieldType::Blocks | FieldType::Relationship
        )
    {
        return Some(f.field_type.clone());
    }

    // Group column: split on `__` and walk the tree. If the final segment
    // fails to resolve, drop it and retry — the trailing segment may be a
    // locale suffix (e.g. `title__en`, `meta__description__de`).
    let parts: Vec<&str> = col.split("__").collect();
    if parts.len() < 2 {
        return None;
    }

    if let Some(ft) = walk_group_path(&parts, fields) {
        return Some(ft);
    }

    // Retry without trailing segment to handle locale-suffixed columns.
    if parts.len() >= 2 {
        let without_tail = &parts[..parts.len() - 1];
        return walk_group_path(without_tail, fields);
    }

    None
}

/// Walk a `__`-separated path through Group fields (and transparent layout
/// wrappers) to find the leaf field type.
fn walk_group_path(parts: &[&str], fields: &[FieldDefinition]) -> Option<FieldType> {
    if parts.is_empty() {
        return None;
    }

    let mut current = fields;
    let mut leaf_type: Option<FieldType> = None;

    for (i, seg) in parts.iter().enumerate() {
        let is_last = i == parts.len() - 1;
        let found = find_field_recursive(seg, current)?;

        if is_last {
            leaf_type = Some(found.field_type.clone());
            break;
        }

        match found.field_type {
            FieldType::Group => {
                current = &found.fields;
            }
            _ => return None,
        }
    }

    leaf_type
}

/// Find a field by name, recursing into transparent layout wrappers (Row, Collapsible, Tabs).
pub(super) fn find_field_recursive<'a>(
    name: &str,
    fields: &'a [FieldDefinition],
) -> Option<&'a FieldDefinition> {
    for f in fields {
        if f.name == name {
            return Some(f);
        }

        match f.field_type {
            FieldType::Row | FieldType::Collapsible => {
                if let Some(found) = find_field_recursive(name, &f.fields) {
                    return Some(found);
                }
            }
            FieldType::Tabs => {
                for tab in &f.tabs {
                    if let Some(found) = find_field_recursive(name, &tab.fields) {
                        return Some(found);
                    }
                }
            }
            _ => {}
        }
    }

    None
}
