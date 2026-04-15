//! Dot notation normalization and filter path resolution.
//!
//! Converts dot-notation filter fields to their SQL representations:
//! - Group fields (`seo.meta_title`) → flat columns (`seo__meta_title`)
//! - Array/Blocks/Relationship sub-fields → EXISTS subquery descriptors

use anyhow::{Result, anyhow, bail};

use crate::core::{BlockDefinition, FieldDefinition, FieldType};
use crate::db::query::helpers::join_table;
use crate::db::{
    DbConnection, FilterClause, LocaleContext, LocaleMode, query::is_valid_identifier,
};

// ── Dot notation normalization ───────────────────────────────────────────

/// Rewrite dot notation for group fields: `seo.meta_title` → `seo__meta_title`.
///
/// Array, Blocks, and Relationship fields keep their dots (resolved at SQL
/// generation time via subqueries). Only Group fields are converted here
/// because they map to flat `{group}__{sub}` columns on the parent table.
pub fn normalize_filter_fields(filters: &mut [FilterClause], fields: &[FieldDefinition]) {
    for clause in filters.iter_mut() {
        match clause {
            FilterClause::Single(f) => normalize_field_name(&mut f.field, fields),
            FilterClause::Or(groups) => {
                for group in groups.iter_mut() {
                    for f in group.iter_mut() {
                        normalize_field_name(&mut f.field, fields);
                    }
                }
            }
        }
    }
}

fn normalize_field_name(field: &mut String, fields: &[FieldDefinition]) {
    if !field.contains('.') {
        return;
    }
    let first_segment = match field.split('.').next() {
        Some(s) => s,
        None => return,
    };

    if is_group_field(first_segment, fields) {
        *field = field.replace('.', "__");
    }
}

/// Check if a field name refers to a Group, recursing into transparent layout wrappers.
fn is_group_field(name: &str, fields: &[FieldDefinition]) -> bool {
    for f in fields {
        if f.name == name && f.field_type == FieldType::Group {
            return true;
        }

        // Recurse into transparent layout wrappers
        match f.field_type {
            FieldType::Row | FieldType::Collapsible => {
                if is_group_field(name, &f.fields) {
                    return true;
                }
            }
            FieldType::Tabs => {
                for tab in &f.tabs {
                    if is_group_field(name, &tab.fields) {
                        return true;
                    }
                }
            }
            _ => {}
        }
    }

    false
}

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
fn lookup_column_field_type(col: &str, fields: &[FieldDefinition]) -> Option<FieldType> {
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
fn find_field_recursive<'a>(
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

// ── Resolved filter types ────────────────────────────────────────────────

/// A filter resolved to its SQL representation.
#[derive(Debug)]
pub(super) enum ResolvedFilter {
    /// Direct column on parent table (existing behavior).
    ///
    /// `field_type` is the leaf field's type, used to cast filter operand
    /// values when binding. `None` when the type cannot be determined —
    /// binding falls back to `DbValue::Text`.
    Column {
        col: String,
        field_type: Option<FieldType>,
    },
    /// EXISTS subquery against a join table.
    Subquery {
        join_table: String,
        parent_table: String,
        condition: SubqueryCondition,
        /// When the join table has a `_locale` column and the query is
        /// scoped to a single locale, this holds the locale string to
        /// constrain the subquery with `_locale = ?`. `None` means no
        /// locale filtering (junction table has no `_locale` column, or
        /// `LocaleMode::All` is active).
        locale_constraint: Option<String>,
    },
}

/// How to access the filtered value within a subquery.
#[derive(Debug)]
pub(super) enum SubqueryCondition {
    /// Direct column on join table (array sub-fields, has-many related_id).
    ///
    /// `field_type` drives operand casting; `None` means fall back to Text.
    Column {
        col: String,
        field_type: Option<FieldType>,
    },
    /// `_block_type` column on the join table. Always text.
    BlockType,
    /// `json_extract` on the `data` column, possibly with `json_each` joins
    /// for nested blocks/arrays.
    Json {
        /// `json_each` joins: `(source_expr, alias)`.
        each_joins: Vec<(String, String)>,
        /// Final expression, e.g. `json_extract(data, '$.body')`.
        extract_expr: String,
        /// Leaf field type for operand coercion. `None` falls back to Text.
        field_type: Option<FieldType>,
    },
}

// ── Filter path resolution ───────────────────────────────────────────────

/// Resolve a dot-notation filter field to its SQL representation.
///
/// Non-dot fields return [`ResolvedFilter::Column`]. Dot fields are routed
/// based on the root field type:
/// - **Array** → subquery with typed column on join table
/// - **Blocks** → subquery with `json_extract` (and `json_each` for nesting)
/// - **Relationship** (has-many) → subquery on `related_id`
///
/// `locale_ctx` drives per-locale filtering on junction tables: when the
/// target array/blocks/relationship field is `localized` and the query is
/// scoped to a single locale (`Single` or `Default`), the returned
/// [`ResolvedFilter::Subquery`] carries a `locale_constraint` so the EXISTS
/// subquery adds a `_locale = ?` clause. `LocaleMode::All` leaves the
/// constraint empty (match rows in any locale).
pub(super) fn resolve_filter(
    conn: &dyn DbConnection,
    field: &str,
    slug: &str,
    fields: &[FieldDefinition],
    locale_ctx: Option<&LocaleContext>,
) -> Result<ResolvedFilter> {
    if !field.contains('.') {
        let field_type = lookup_column_field_type(field, fields);
        return Ok(ResolvedFilter::Column {
            col: field.to_string(),
            field_type,
        });
    }

    // Guarded by early return above: field.contains('.') is true here
    let dot_pos = field.find('.').expect("dot checked above");
    let root = &field[..dot_pos];
    let rest = &field[dot_pos + 1..];

    let field_def = find_field_recursive(root, fields)
        .ok_or_else(|| anyhow!("Unknown field '{}' in filter path '{}'", root, field))?;

    let jt = join_table(slug, root);

    // The junction table has a `_locale` column iff the container field is
    // itself localized. Transparent layout wrappers (Row/Collapsible/Tabs)
    // do not carry localization, so we only need to check `field_def.localized`.
    // Nested containers inside a localized Group use `{group}__{array}` dot
    // notation that does not route here (resolve_filter expects the root to
    // be a top-level Array/Blocks/Relationship), so inherited Group locale
    // does not apply at this call site.
    let has_locale_col = field_def.localized && locale_ctx.is_some_and(|c| c.config.is_enabled());
    let locale_constraint = has_locale_col
        .then(|| locale_ctx.and_then(subquery_locale))
        .flatten();

    match field_def.field_type {
        FieldType::Array => resolve_array_filter(
            conn,
            root,
            rest,
            field,
            slug,
            field_def,
            jt,
            locale_constraint,
        ),
        FieldType::Blocks => resolve_blocks_filter(
            conn,
            root,
            rest,
            field,
            slug,
            field_def,
            jt,
            locale_constraint,
        ),
        FieldType::Relationship => {
            resolve_relationship_filter(root, rest, slug, field_def, jt, locale_constraint)
        }
        _ => {
            bail!(
                "Field '{}' (type {:?}) does not support sub-field filtering",
                root,
                field_def.field_type
            );
        }
    }
}

/// Pick the locale string to use as a `_locale = ?` constraint for a subquery.
///
/// `Single(loc)` → use that locale. `Default` → use the configured default.
/// `All` → `None` (match across all locales).
fn subquery_locale(ctx: &LocaleContext) -> Option<String> {
    match &ctx.mode {
        LocaleMode::Single(l) => Some(l.clone()),
        LocaleMode::Default => Some(ctx.config.default_locale.clone()),
        LocaleMode::All => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn resolve_array_filter(
    conn: &dyn DbConnection,
    root: &str,
    rest: &str,
    field: &str,
    slug: &str,
    field_def: &FieldDefinition,
    join_table: String,
    locale_constraint: Option<String>,
) -> Result<ResolvedFilter> {
    for seg in rest.split('.') {
        if !is_valid_identifier(seg) {
            bail!("Invalid segment '{}' in filter path '{}'", seg, field);
        }
    }
    if let Some(dot) = rest.find('.') {
        // Dotted path inside array — check if first segment is a Group sub-field.
        let first_seg = &rest[..dot];
        let remaining = &rest[dot + 1..];
        let sub_def = field_def.fields.iter().find(|f| f.name == first_seg);
        match sub_def.map(|f| &f.field_type) {
            Some(FieldType::Group) => {
                // Group sub-fields in arrays are stored as JSON TEXT columns.
                // Access nested values via json_extract.
                let extract_expr = conn.json_extract_expr(first_seg, remaining);
                let field_type = sub_def
                    .and_then(|g| find_field_recursive(remaining, &g.fields))
                    .map(|f| f.field_type.clone());
                Ok(ResolvedFilter::Subquery {
                    join_table,
                    parent_table: slug.to_string(),
                    condition: SubqueryCondition::Json {
                        each_joins: vec![],
                        extract_expr,
                        field_type,
                    },
                    locale_constraint,
                })
            }
            _ => {
                bail!(
                    "Nested dot path '{}' in array '{}': only Group sub-fields support nested filtering",
                    rest,
                    root
                );
            }
        }
    } else {
        // Simple sub-field — direct typed column on join table.
        let field_type = field_def
            .fields
            .iter()
            .find(|f| f.name == rest)
            .map(|f| f.field_type.clone());
        Ok(ResolvedFilter::Subquery {
            join_table,
            parent_table: slug.to_string(),
            condition: SubqueryCondition::Column {
                col: rest.to_string(),
                field_type,
            },
            locale_constraint,
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn resolve_blocks_filter(
    conn: &dyn DbConnection,
    _root: &str,
    rest: &str,
    field: &str,
    slug: &str,
    field_def: &FieldDefinition,
    join_table: String,
    locale_constraint: Option<String>,
) -> Result<ResolvedFilter> {
    if rest == "_block_type" {
        Ok(ResolvedFilter::Subquery {
            join_table,
            parent_table: slug.to_string(),
            condition: SubqueryCondition::BlockType,
            locale_constraint,
        })
    } else {
        let rest_parts: Vec<&str> = rest.split('.').collect();
        for seg in &rest_parts {
            if !is_valid_identifier(seg) && *seg != "_block_type" {
                bail!("Invalid segment '{}' in filter path '{}'", seg, field);
            }
        }
        let (each_joins, extract_expr, field_type) =
            walk_block_fields(conn, &rest_parts, &field_def.blocks, &join_table)?;
        Ok(ResolvedFilter::Subquery {
            join_table,
            parent_table: slug.to_string(),
            condition: SubqueryCondition::Json {
                each_joins,
                extract_expr,
                field_type,
            },
            locale_constraint,
        })
    }
}

fn resolve_relationship_filter(
    root: &str,
    rest: &str,
    slug: &str,
    field_def: &FieldDefinition,
    join_table: String,
    locale_constraint: Option<String>,
) -> Result<ResolvedFilter> {
    if let Some(ref rc) = field_def.relationship {
        if rc.has_many {
            if rest != "id" {
                bail!(
                    "Has-many relationship '{}' can only be filtered by '.id', got '.{}'",
                    root,
                    rest
                );
            }
            Ok(ResolvedFilter::Subquery {
                join_table,
                parent_table: slug.to_string(),
                condition: SubqueryCondition::Column {
                    col: "related_id".to_string(),
                    field_type: Some(FieldType::Text),
                },
                locale_constraint,
            })
        } else {
            bail!(
                "Has-one relationship '{}' does not use dot notation for filtering",
                root
            );
        }
    } else {
        bail!("Relationship field '{}' missing relationship config", root);
    }
}

/// Result of walking a block filter path: the `json_each` joins needed,
/// the final extract expression, and the leaf field type for binding.
pub(super) type BlockWalkResult = (Vec<(String, String)>, String, Option<FieldType>);

/// Walk block type definitions to build `json_each` joins and a final
/// `json_extract` expression for a nested path.
///
/// At each segment:
/// - **Blocks/Array** sub-field → add a `json_each()` join, recurse
/// - **Group** sub-field → extend the JSON path (no join)
/// - **Scalar** → leaf node, produce `json_extract` expression
/// - **`_block_type`** → special: extract from current nesting level
pub(super) fn walk_block_fields(
    conn: &dyn DbConnection,
    segments: &[&str],
    block_defs: &[BlockDefinition],
    join_table: &str,
) -> Result<BlockWalkResult> {
    if segments.is_empty() {
        bail!("Empty path for block filter");
    }

    let mut each_joins: Vec<(String, String)> = Vec::new();
    let mut json_path_parts: Vec<String> = Vec::new();

    // Collect all fields across all block types at the current level.
    let all_fields: Vec<&FieldDefinition> =
        block_defs.iter().flat_map(|bd| bd.fields.iter()).collect();
    let mut current_fields = all_fields;

    let mut remaining = segments;

    while !remaining.is_empty() {
        let seg = remaining[0];
        remaining = &remaining[1..];

        // Handle _block_type at nested level — always Text.
        if seg == "_block_type" {
            if !remaining.is_empty() {
                bail!("_block_type must be the last segment in a filter path");
            }
            let expr = build_block_type_expr(conn, &each_joins, &mut json_path_parts, join_table);

            return Ok((each_joins, expr, Some(FieldType::Text)));
        }

        let field_def = current_fields
            .iter()
            .find(|f| f.name == seg)
            .ok_or_else(|| anyhow!("Unknown field '{}' in block filter path", seg))?;

        match field_def.field_type {
            FieldType::Blocks | FieldType::Array => {
                // Nested blocks/array → json_each join
                let source =
                    build_json_each_source(conn, &each_joins, &json_path_parts, seg, join_table);
                let alias = format!("j{}", each_joins.len());
                each_joins.push((source, alias));
                json_path_parts.clear();

                current_fields = if field_def.field_type == FieldType::Blocks {
                    field_def
                        .blocks
                        .iter()
                        .flat_map(|bd| bd.fields.iter())
                        .collect()
                } else {
                    field_def.fields.iter().collect()
                };
            }
            FieldType::Group | FieldType::Row | FieldType::Collapsible => {
                json_path_parts.push(seg.to_string());
                current_fields = field_def.fields.iter().collect();
            }
            FieldType::Tabs => {
                json_path_parts.push(seg.to_string());
                current_fields = field_def
                    .tabs
                    .iter()
                    .flat_map(|t| t.fields.iter())
                    .collect();
            }
            _ => {
                // Scalar leaf
                if !remaining.is_empty() {
                    bail!("Scalar field '{}' cannot have sub-paths", seg);
                }
                json_path_parts.push(seg.to_string());
                let path = json_path_parts.join(".");
                let expr = if !each_joins.is_empty() {
                    let last_alias = &each_joins.last().expect("each_joins is non-empty").1;
                    conn.json_extract_expr(&format!("{}.value", last_alias), &path)
                } else {
                    conn.json_extract_expr("data", &path)
                };

                return Ok((each_joins, expr, Some(field_def.field_type.clone())));
            }
        }
    }

    bail!("Filter path must end on a scalar field or _block_type, not a container")
}

fn build_block_type_expr(
    conn: &dyn DbConnection,
    each_joins: &[(String, String)],
    json_path_parts: &mut Vec<String>,
    _join_table: &str,
) -> String {
    if !each_joins.is_empty() {
        let last_alias = &each_joins.last().expect("each_joins is non-empty").1;
        let source = format!("{}.value", last_alias);

        if json_path_parts.is_empty() {
            conn.json_extract_expr(&source, "_block_type")
        } else {
            json_path_parts.push("_block_type".to_string());
            conn.json_extract_expr(&source, &json_path_parts.join("."))
        }
    } else {
        json_path_parts.push("_block_type".to_string());
        conn.json_extract_expr("data", &json_path_parts.join("."))
    }
}

/// Build the source expression for a `json_each()` join.
///
/// If there are prior `json_each` joins, references the last alias's `.value`.
/// Otherwise, references `{join_table}.data`. Accumulated group path parts
/// are included in the JSON path.
pub(super) fn build_json_each_source(
    conn: &dyn DbConnection,
    each_joins: &[(String, String)],
    json_path_parts: &[String],
    segment: &str,
    join_table: &str,
) -> String {
    let mut path_parts: Vec<&str> = json_path_parts.iter().map(|s| s.as_str()).collect();
    path_parts.push(segment);
    let json_path = path_parts.join(".");

    if let Some((_src, alias)) = each_joins.last() {
        conn.json_extract_expr(&format!("{}.value", alias), &json_path)
    } else {
        conn.json_extract_expr(&format!("{}.data", join_table), &json_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::field::{
        BlockDefinition, FieldDefinition, FieldTab, FieldType, RelationshipConfig,
    };
    use crate::db::query::{Filter, FilterClause, FilterOp};

    fn test_conn() -> (tempfile::TempDir, crate::db::BoxedConnection) {
        let dir = tempfile::TempDir::new().unwrap();
        let config = crate::config::CrapConfig::default();
        let p = crate::db::pool::create_pool(dir.path(), &config).unwrap();
        (dir, p.get().unwrap())
    }

    fn make_field(name: &str, ft: FieldType, localized: bool) -> FieldDefinition {
        FieldDefinition::builder(name, ft)
            .localized(localized)
            .build()
    }

    fn make_array_field(name: &str, sub_fields: Vec<FieldDefinition>) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Array)
            .fields(sub_fields)
            .build()
    }

    fn make_blocks_field(name: &str, blocks: Vec<BlockDefinition>) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Blocks)
            .blocks(blocks)
            .build()
    }

    fn make_has_many_field(name: &str, collection: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Relationship)
            .relationship(RelationshipConfig::new(collection, true))
            .build()
    }

    fn make_block_def(block_type: &str, fields: Vec<FieldDefinition>) -> BlockDefinition {
        BlockDefinition::new(block_type, fields)
    }

    // ── normalize_filter_fields ──────────────────────────────────────────

    #[test]
    fn normalize_group_dot_to_double_underscore() {
        let fields = vec![make_field("seo", FieldType::Group, false)];
        let mut filters = vec![FilterClause::Single(Filter {
            field: "seo.meta_title".into(),
            op: FilterOp::Equals("test".into()),
        })];
        normalize_filter_fields(&mut filters, &fields);
        match &filters[0] {
            FilterClause::Single(f) => assert_eq!(f.field, "seo__meta_title"),
            other => panic!("Expected Single, got {:?}", other),
        }
    }

    #[test]
    fn normalize_preserves_array_dots() {
        let fields = vec![make_array_field(
            "items",
            vec![make_field("name", FieldType::Text, false)],
        )];
        let mut filters = vec![FilterClause::Single(Filter {
            field: "items.name".into(),
            op: FilterOp::Equals("test".into()),
        })];
        normalize_filter_fields(&mut filters, &fields);
        match &filters[0] {
            FilterClause::Single(f) => assert_eq!(f.field, "items.name"),
            other => panic!("Expected Single, got {:?}", other),
        }
    }

    #[test]
    fn normalize_preserves_blocks_dots() {
        let fields = vec![make_blocks_field("content", vec![])];
        let mut filters = vec![FilterClause::Single(Filter {
            field: "content.body".into(),
            op: FilterOp::Equals("test".into()),
        })];
        normalize_filter_fields(&mut filters, &fields);
        match &filters[0] {
            FilterClause::Single(f) => assert_eq!(f.field, "content.body"),
            other => panic!("Expected Single, got {:?}", other),
        }
    }

    #[test]
    fn normalize_in_or_groups() {
        let fields = vec![make_field("seo", FieldType::Group, false)];
        let mut filters = vec![FilterClause::Or(vec![
            vec![Filter {
                field: "seo.title".into(),
                op: FilterOp::Equals("a".into()),
            }],
            vec![Filter {
                field: "seo.desc".into(),
                op: FilterOp::Equals("b".into()),
            }],
        ])];
        normalize_filter_fields(&mut filters, &fields);
        match &filters[0] {
            FilterClause::Or(groups) => {
                assert_eq!(groups[0][0].field, "seo__title");
                assert_eq!(groups[1][0].field, "seo__desc");
            }
            other => panic!("Expected Or, got {:?}", other),
        }
    }

    #[test]
    fn normalize_no_dots_passthrough() {
        let fields = vec![make_field("title", FieldType::Text, false)];
        let mut filters = vec![FilterClause::Single(Filter {
            field: "title".into(),
            op: FilterOp::Equals("test".into()),
        })];
        normalize_filter_fields(&mut filters, &fields);
        match &filters[0] {
            FilterClause::Single(f) => assert_eq!(f.field, "title"),
            other => panic!("Expected Single, got {:?}", other),
        }
    }

    // ── resolve_filter ───────────────────────────────────────────────────

    #[test]
    fn resolve_filter_no_dots_returns_column() {
        let (_dir, conn) = test_conn();
        let resolved = resolve_filter(&conn, "status", "posts", &[], None).unwrap();
        match resolved {
            ResolvedFilter::Column { col, field_type } => {
                assert_eq!(col, "status");
                assert_eq!(field_type, None);
            }
            other => panic!("Expected Column, got {:?}", other),
        }
    }

    #[test]
    fn resolve_filter_array_subfield() {
        let (_dir, conn) = test_conn();
        let fields = vec![make_array_field(
            "items",
            vec![make_field("name", FieldType::Text, false)],
        )];
        let resolved = resolve_filter(&conn, "items.name", "posts", &fields, None).unwrap();
        match resolved {
            ResolvedFilter::Subquery {
                join_table,
                parent_table,
                condition,
                locale_constraint,
            } => {
                assert_eq!(join_table, "posts_items");
                assert_eq!(parent_table, "posts");
                assert_eq!(locale_constraint, None);
                match condition {
                    SubqueryCondition::Column { col, field_type } => {
                        assert_eq!(col, "name");
                        assert_eq!(field_type, Some(FieldType::Text));
                    }
                    other => panic!("Expected Column, got {:?}", other),
                }
            }
            other => panic!("Expected Subquery, got {:?}", other),
        }
    }

    #[test]
    fn resolve_filter_array_group_in_array() {
        let (_dir, conn) = test_conn();
        let mut addr = make_field("address", FieldType::Group, false);
        addr.fields = vec![make_field("city", FieldType::Text, false)];
        let fields = vec![make_array_field("items", vec![addr])];

        let resolved = resolve_filter(&conn, "items.address.city", "posts", &fields, None).unwrap();
        match resolved {
            ResolvedFilter::Subquery { condition, .. } => match condition {
                SubqueryCondition::Json {
                    each_joins,
                    extract_expr,
                    field_type,
                } => {
                    assert!(each_joins.is_empty());
                    assert_eq!(extract_expr, "json_extract(address, '$.city')");
                    assert_eq!(field_type, Some(FieldType::Text));
                }
                other => panic!("Expected Json, got {:?}", other),
            },
            other => panic!("Expected Subquery, got {:?}", other),
        }
    }

    #[test]
    fn resolve_filter_block_type() {
        let (_dir, conn) = test_conn();
        let fields = vec![make_blocks_field("content", vec![])];
        let resolved =
            resolve_filter(&conn, "content._block_type", "posts", &fields, None).unwrap();
        match resolved {
            ResolvedFilter::Subquery { condition, .. } => {
                assert!(matches!(condition, SubqueryCondition::BlockType));
            }
            other => panic!("Expected Subquery, got {:?}", other),
        }
    }

    #[test]
    fn resolve_filter_block_scalar() {
        let (_dir, conn) = test_conn();
        let fields = vec![make_blocks_field(
            "content",
            vec![make_block_def(
                "paragraph",
                vec![make_field("body", FieldType::Textarea, false)],
            )],
        )];
        let resolved = resolve_filter(&conn, "content.body", "posts", &fields, None).unwrap();
        match resolved {
            ResolvedFilter::Subquery { condition, .. } => match condition {
                SubqueryCondition::Json {
                    each_joins,
                    extract_expr,
                    field_type,
                } => {
                    assert!(each_joins.is_empty());
                    assert_eq!(extract_expr, "json_extract(data, '$.body')");
                    assert_eq!(field_type, Some(FieldType::Textarea));
                }
                other => panic!("Expected Json, got {:?}", other),
            },
            other => panic!("Expected Subquery, got {:?}", other),
        }
    }

    #[test]
    fn resolve_filter_has_many_relationship() {
        let (_dir, conn) = test_conn();
        let fields = vec![make_has_many_field("tags", "tags")];
        let resolved = resolve_filter(&conn, "tags.id", "posts", &fields, None).unwrap();
        match resolved {
            ResolvedFilter::Subquery {
                join_table,
                condition,
                ..
            } => {
                assert_eq!(join_table, "posts_tags");
                match condition {
                    SubqueryCondition::Column { col, field_type } => {
                        assert_eq!(col, "related_id");
                        assert_eq!(field_type, Some(FieldType::Text));
                    }
                    other => panic!("Expected Column, got {:?}", other),
                }
            }
            other => panic!("Expected Subquery, got {:?}", other),
        }
    }

    #[test]
    fn resolve_filter_has_many_rejects_non_id() {
        let (_dir, conn) = test_conn();
        let fields = vec![make_has_many_field("tags", "tags")];
        let result = resolve_filter(&conn, "tags.name", "posts", &fields, None);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("only be filtered by '.id'")
        );
    }

    #[test]
    fn resolve_filter_has_one_relationship_rejects_dot() {
        let (_dir, conn) = test_conn();
        let fields = vec![
            FieldDefinition::builder("author", FieldType::Relationship)
                .relationship(RelationshipConfig::new("users", false))
                .build(),
        ];
        let result = resolve_filter(&conn, "author.name", "posts", &fields, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Has-one"));
    }

    #[test]
    fn resolve_filter_unsupported_field_type() {
        let (_dir, conn) = test_conn();
        let fields = vec![make_field("title", FieldType::Text, false)];
        let result = resolve_filter(&conn, "title.sub", "posts", &fields, None);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("does not support sub-field filtering")
        );
    }

    #[test]
    fn resolve_filter_relationship_missing_config() {
        let (_dir, conn) = test_conn();
        let fields = vec![FieldDefinition::builder("tags", FieldType::Relationship).build()];
        let result = resolve_filter(&conn, "tags.id", "posts", &fields, None);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("missing relationship config")
        );
    }

    #[test]
    fn resolve_filter_unknown_root_field() {
        let (_dir, conn) = test_conn();
        let fields = vec![make_field("title", FieldType::Text, false)];
        let result = resolve_filter(&conn, "nonexistent.sub", "posts", &fields, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Unknown field"));
    }

    #[test]
    fn resolve_filter_array_nested_non_group_error() {
        let (_dir, conn) = test_conn();
        let fields = vec![make_array_field(
            "items",
            vec![make_field("name", FieldType::Text, false)],
        )];
        let result = resolve_filter(&conn, "items.name.deep", "posts", &fields, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Nested dot path"));
    }

    #[test]
    fn resolve_filter_array_invalid_segment() {
        let (_dir, conn) = test_conn();
        let fields = vec![make_array_field(
            "items",
            vec![make_field("name", FieldType::Text, false)],
        )];
        let result = resolve_filter(&conn, "items.bad field", "posts", &fields, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid segment"));
    }

    #[test]
    fn resolve_filter_blocks_invalid_segment() {
        let (_dir, conn) = test_conn();
        let fields = vec![make_blocks_field(
            "content",
            vec![make_block_def(
                "text",
                vec![make_field("body", FieldType::Textarea, false)],
            )],
        )];
        let result = resolve_filter(&conn, "content.bad field", "posts", &fields, None);
        assert!(result.is_err());
    }

    // ── walk_block_fields ────────────────────────────────────────────────

    #[test]
    fn walk_block_simple_scalar() {
        let (_dir, conn) = test_conn();
        let block_defs = vec![make_block_def(
            "text",
            vec![make_field("body", FieldType::Textarea, false)],
        )];
        let (joins, expr, _leaf) =
            walk_block_fields(&conn, &["body"], &block_defs, "posts_content").unwrap();
        assert!(joins.is_empty());
        assert_eq!(expr, "json_extract(data, '$.body')");
    }

    #[test]
    fn walk_block_group_then_scalar() {
        let (_dir, conn) = test_conn();
        let mut grp = make_field("meta", FieldType::Group, false);
        grp.fields = vec![make_field("title", FieldType::Text, false)];
        let block_defs = vec![make_block_def("rich", vec![grp])];

        let (joins, expr, _leaf) =
            walk_block_fields(&conn, &["meta", "title"], &block_defs, "posts_content").unwrap();
        assert!(joins.is_empty());
        assert_eq!(expr, "json_extract(data, '$.meta.title')");
    }

    #[test]
    fn walk_block_nested_blocks_scalar() {
        let (_dir, conn) = test_conn();
        let inner_blocks = vec![make_block_def(
            "quote",
            vec![make_field("text", FieldType::Text, false)],
        )];
        let mut nested = make_field("nested", FieldType::Blocks, false);
        nested.blocks = inner_blocks;
        let block_defs = vec![make_block_def("rich", vec![nested])];

        let (joins, expr, _leaf) =
            walk_block_fields(&conn, &["nested", "text"], &block_defs, "posts_content").unwrap();
        assert_eq!(joins.len(), 1);
        assert_eq!(joins[0].0, "json_extract(posts_content.data, '$.nested')");
        assert_eq!(joins[0].1, "j0");
        assert_eq!(expr, "json_extract(j0.value, '$.text')");
    }

    #[test]
    fn walk_block_deeply_nested() {
        let (_dir, conn) = test_conn();
        // content -> nested -> deeper -> field
        let deep_blocks = vec![make_block_def(
            "leaf",
            vec![make_field("field", FieldType::Text, false)],
        )];
        let mut deeper = make_field("deeper", FieldType::Blocks, false);
        deeper.blocks = deep_blocks;
        let mid_blocks = vec![make_block_def("mid", vec![deeper])];
        let mut nested = make_field("nested", FieldType::Blocks, false);
        nested.blocks = mid_blocks;
        let block_defs = vec![make_block_def("top", vec![nested])];

        let (joins, expr, _leaf) = walk_block_fields(
            &conn,
            &["nested", "deeper", "field"],
            &block_defs,
            "posts_content",
        )
        .unwrap();
        assert_eq!(joins.len(), 2);
        assert_eq!(joins[0].0, "json_extract(posts_content.data, '$.nested')");
        assert_eq!(joins[0].1, "j0");
        assert_eq!(joins[1].0, "json_extract(j0.value, '$.deeper')");
        assert_eq!(joins[1].1, "j1");
        assert_eq!(expr, "json_extract(j1.value, '$.field')");
    }

    #[test]
    fn walk_block_nested_block_type() {
        let (_dir, conn) = test_conn();
        let inner_blocks = vec![make_block_def(
            "quote",
            vec![make_field("text", FieldType::Text, false)],
        )];
        let mut nested = make_field("nested", FieldType::Blocks, false);
        nested.blocks = inner_blocks;
        let block_defs = vec![make_block_def("rich", vec![nested])];

        let (joins, expr, _leaf) = walk_block_fields(
            &conn,
            &["nested", "_block_type"],
            &block_defs,
            "posts_content",
        )
        .unwrap();
        assert_eq!(joins.len(), 1);
        assert_eq!(expr, "json_extract(j0.value, '$._block_type')");
    }

    #[test]
    fn walk_block_group_then_nested_blocks() {
        let (_dir, conn) = test_conn();
        // group "sidebar" → blocks "nested" → scalar "body"
        let inner_blocks = vec![make_block_def(
            "text",
            vec![make_field("body", FieldType::Textarea, false)],
        )];
        let mut nested = make_field("nested", FieldType::Blocks, false);
        nested.blocks = inner_blocks;
        let mut sidebar = make_field("sidebar", FieldType::Group, false);
        sidebar.fields = vec![nested];
        let block_defs = vec![make_block_def("layout", vec![sidebar])];

        let (joins, expr, _leaf) = walk_block_fields(
            &conn,
            &["sidebar", "nested", "body"],
            &block_defs,
            "posts_content",
        )
        .unwrap();
        assert_eq!(joins.len(), 1);
        assert_eq!(
            joins[0].0,
            "json_extract(posts_content.data, '$.sidebar.nested')"
        );
        assert_eq!(expr, "json_extract(j0.value, '$.body')");
    }

    #[test]
    fn walk_block_empty_path_error() {
        let (_dir, conn) = test_conn();
        let block_defs = vec![make_block_def("text", vec![])];
        let result = walk_block_fields(&conn, &[], &block_defs, "table");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Empty path"));
    }

    #[test]
    fn walk_block_scalar_with_subpath_error() {
        let (_dir, conn) = test_conn();
        let block_defs = vec![make_block_def(
            "text",
            vec![make_field("body", FieldType::Textarea, false)],
        )];
        let result = walk_block_fields(&conn, &["body", "extra"], &block_defs, "table");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Scalar field"));
    }

    #[test]
    fn walk_block_container_as_leaf_error() {
        let (_dir, conn) = test_conn();
        let mut nested = make_field("nested", FieldType::Blocks, false);
        nested.blocks = vec![make_block_def("inner", vec![])];
        let block_defs = vec![make_block_def("outer", vec![nested])];
        let result = walk_block_fields(&conn, &["nested"], &block_defs, "table");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("must end on a scalar")
        );
    }

    #[test]
    fn walk_block_block_type_not_last_error() {
        let (_dir, conn) = test_conn();
        let block_defs = vec![make_block_def(
            "text",
            vec![make_field("body", FieldType::Textarea, false)],
        )];
        let result = walk_block_fields(&conn, &["_block_type", "extra"], &block_defs, "table");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("_block_type must be the last segment")
        );
    }

    #[test]
    fn walk_block_top_level_block_type_without_joins() {
        let (_dir, conn) = test_conn();
        let block_defs = vec![make_block_def(
            "text",
            vec![make_field("body", FieldType::Textarea, false)],
        )];
        let (joins, expr, _leaf) =
            walk_block_fields(&conn, &["_block_type"], &block_defs, "posts_content").unwrap();
        assert!(joins.is_empty());
        assert_eq!(expr, "json_extract(data, '$._block_type')");
    }

    #[test]
    fn walk_block_array_in_block() {
        let (_dir, conn) = test_conn();
        let mut arr = make_field("items", FieldType::Array, false);
        arr.fields = vec![make_field("name", FieldType::Text, false)];
        let block_defs = vec![make_block_def("list", vec![arr])];

        let (joins, expr, _leaf) =
            walk_block_fields(&conn, &["items", "name"], &block_defs, "posts_content").unwrap();
        assert_eq!(joins.len(), 1);
        assert_eq!(joins[0].0, "json_extract(posts_content.data, '$.items')");
        assert_eq!(expr, "json_extract(j0.value, '$.name')");
    }

    #[test]
    fn normalize_group_inside_row() {
        let group = FieldDefinition::builder("seo", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("title", FieldType::Text).build(),
            ])
            .build();
        let row = FieldDefinition::builder("layout", FieldType::Row)
            .fields(vec![group])
            .build();
        let fields = vec![row];

        let mut filters = vec![FilterClause::Single(Filter {
            field: "seo.title".to_string(),
            op: FilterOp::Equals("test".to_string()),
        })];
        normalize_filter_fields(&mut filters, &fields);

        match &filters[0] {
            FilterClause::Single(f) => assert_eq!(f.field, "seo__title"),
            _ => panic!("expected single"),
        }
    }

    #[test]
    fn normalize_group_inside_tabs() {
        let group = FieldDefinition::builder("seo", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("title", FieldType::Text).build(),
            ])
            .build();
        let tabs = FieldDefinition::builder("layout", FieldType::Tabs)
            .tabs(vec![FieldTab {
                label: "Main".to_string(),
                description: None,
                fields: vec![group],
            }])
            .build();
        let fields = vec![tabs];

        let mut filters = vec![FilterClause::Single(Filter {
            field: "seo.title".to_string(),
            op: FilterOp::Equals("test".to_string()),
        })];
        normalize_filter_fields(&mut filters, &fields);

        match &filters[0] {
            FilterClause::Single(f) => assert_eq!(f.field, "seo__title"),
            _ => panic!("expected single"),
        }
    }

    #[test]
    fn normalize_group_inside_collapsible() {
        let group = FieldDefinition::builder("seo", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("title", FieldType::Text).build(),
            ])
            .build();
        let collapsible = FieldDefinition::builder("advanced", FieldType::Collapsible)
            .fields(vec![group])
            .build();
        let fields = vec![collapsible];

        let mut filters = vec![FilterClause::Single(Filter {
            field: "seo.title".to_string(),
            op: FilterOp::Equals("test".to_string()),
        })];
        normalize_filter_fields(&mut filters, &fields);

        match &filters[0] {
            FilterClause::Single(f) => assert_eq!(f.field, "seo__title"),
            _ => panic!("expected single"),
        }
    }

    #[test]
    fn walk_block_nested_block_type_with_group_path() {
        let (_dir, conn) = test_conn();
        // group "meta" → nested blocks → _block_type
        let inner_blocks = vec![make_block_def(
            "quote",
            vec![make_field("text", FieldType::Text, false)],
        )];
        let mut nested = make_field("nested", FieldType::Blocks, false);
        nested.blocks = inner_blocks;
        let mut meta = make_field("meta", FieldType::Group, false);
        meta.fields = vec![nested];
        let block_defs = vec![make_block_def("rich", vec![meta])];

        let (joins, expr, _leaf) = walk_block_fields(
            &conn,
            &["meta", "nested", "_block_type"],
            &block_defs,
            "posts_content",
        )
        .unwrap();
        assert_eq!(joins.len(), 1);
        assert_eq!(
            joins[0].0,
            "json_extract(posts_content.data, '$.meta.nested')"
        );
        assert_eq!(expr, "json_extract(j0.value, '$._block_type')");
    }
}
