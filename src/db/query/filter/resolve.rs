//! Dot notation normalization and filter path resolution.
//!
//! Converts dot-notation filter fields to their SQL representations:
//! - Group fields (`seo.meta_title`) → flat columns (`seo__meta_title`)
//! - Array/Blocks/Relationship sub-fields → EXISTS subquery descriptors

use anyhow::{Result, anyhow, bail};

use super::super::{FilterClause, is_valid_identifier};
use crate::core::field::{BlockDefinition, FieldDefinition, FieldType};

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

    if let Some(fd) = fields.iter().find(|f| f.name == first_segment)
        && fd.field_type == FieldType::Group
    {
        *field = field.replace('.', "__");
    }
}

// ── Resolved filter types ────────────────────────────────────────────────

/// A filter resolved to its SQL representation.
#[derive(Debug)]
pub(super) enum ResolvedFilter {
    /// Direct column on parent table (existing behavior).
    Column(String),
    /// EXISTS subquery against a join table.
    Subquery {
        join_table: String,
        parent_table: String,
        condition: SubqueryCondition,
    },
}

/// How to access the filtered value within a subquery.
#[derive(Debug)]
pub(super) enum SubqueryCondition {
    /// Direct column on join table (array sub-fields, has-many related_id).
    Column(String),
    /// `_block_type` column on the join table.
    BlockType,
    /// `json_extract` on the `data` column, possibly with `json_each` joins
    /// for nested blocks/arrays.
    Json {
        /// `json_each` joins: `(source_expr, alias)`.
        each_joins: Vec<(String, String)>,
        /// Final expression, e.g. `json_extract(data, '$.body')`.
        extract_expr: String,
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
pub(super) fn resolve_filter(
    field: &str,
    slug: &str,
    fields: &[FieldDefinition],
) -> Result<ResolvedFilter> {
    if !field.contains('.') {
        return Ok(ResolvedFilter::Column(field.to_string()));
    }

    // Guarded by early return above: field.contains('.') is true here
    let dot_pos = field.find('.').expect("dot checked above");
    let root = &field[..dot_pos];
    let rest = &field[dot_pos + 1..];

    let field_def = fields
        .iter()
        .find(|f| f.name == root)
        .ok_or_else(|| anyhow!("Unknown field '{}' in filter path '{}'", root, field))?;

    let join_table = format!("{}_{}", slug, root);

    match field_def.field_type {
        FieldType::Array => resolve_array_filter(root, rest, field, slug, field_def, join_table),
        FieldType::Blocks => resolve_blocks_filter(root, rest, field, slug, field_def, join_table),
        FieldType::Relationship => {
            resolve_relationship_filter(root, rest, slug, field_def, join_table)
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

fn resolve_array_filter(
    root: &str,
    rest: &str,
    field: &str,
    slug: &str,
    field_def: &FieldDefinition,
    join_table: String,
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
                let extract_expr = format!("json_extract({}, '$.{}')", first_seg, remaining);
                Ok(ResolvedFilter::Subquery {
                    join_table,
                    parent_table: slug.to_string(),
                    condition: SubqueryCondition::Json {
                        each_joins: vec![],
                        extract_expr,
                    },
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
        Ok(ResolvedFilter::Subquery {
            join_table,
            parent_table: slug.to_string(),
            condition: SubqueryCondition::Column(rest.to_string()),
        })
    }
}

fn resolve_blocks_filter(
    _root: &str,
    rest: &str,
    field: &str,
    slug: &str,
    field_def: &FieldDefinition,
    join_table: String,
) -> Result<ResolvedFilter> {
    if rest == "_block_type" {
        Ok(ResolvedFilter::Subquery {
            join_table,
            parent_table: slug.to_string(),
            condition: SubqueryCondition::BlockType,
        })
    } else {
        let rest_parts: Vec<&str> = rest.split('.').collect();
        for seg in &rest_parts {
            if !is_valid_identifier(seg) && *seg != "_block_type" {
                bail!("Invalid segment '{}' in filter path '{}'", seg, field);
            }
        }
        let (each_joins, extract_expr) =
            walk_block_fields(&rest_parts, &field_def.blocks, &join_table)?;
        Ok(ResolvedFilter::Subquery {
            join_table,
            parent_table: slug.to_string(),
            condition: SubqueryCondition::Json {
                each_joins,
                extract_expr,
            },
        })
    }
}

fn resolve_relationship_filter(
    root: &str,
    rest: &str,
    slug: &str,
    field_def: &FieldDefinition,
    join_table: String,
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
                condition: SubqueryCondition::Column("related_id".to_string()),
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

/// Walk block type definitions to build `json_each` joins and a final
/// `json_extract` expression for a nested path.
///
/// At each segment:
/// - **Blocks/Array** sub-field → add a `json_each()` join, recurse
/// - **Group** sub-field → extend the JSON path (no join)
/// - **Scalar** → leaf node, produce `json_extract` expression
/// - **`_block_type`** → special: extract from current nesting level
pub(super) fn walk_block_fields(
    segments: &[&str],
    block_defs: &[BlockDefinition],
    join_table: &str,
) -> Result<(Vec<(String, String)>, String)> {
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

        // Handle _block_type at nested level
        if seg == "_block_type" {
            if !remaining.is_empty() {
                bail!("_block_type must be the last segment in a filter path");
            }
            let expr = build_block_type_expr(&each_joins, &mut json_path_parts, join_table);

            return Ok((each_joins, expr));
        }

        let field_def = current_fields
            .iter()
            .find(|f| f.name == seg)
            .ok_or_else(|| anyhow!("Unknown field '{}' in block filter path", seg))?;

        match field_def.field_type {
            FieldType::Blocks => {
                // Nested blocks → json_each join
                let source = build_json_each_source(&each_joins, &json_path_parts, seg, join_table);
                let alias = format!("j{}", each_joins.len());
                each_joins.push((source, alias));
                json_path_parts.clear();
                current_fields = field_def
                    .blocks
                    .iter()
                    .flat_map(|bd| bd.fields.iter())
                    .collect();
            }
            FieldType::Array => {
                // Nested array in block JSON → json_each join
                let source = build_json_each_source(&each_joins, &json_path_parts, seg, join_table);
                let alias = format!("j{}", each_joins.len());
                each_joins.push((source, alias));
                json_path_parts.clear();
                current_fields = field_def.fields.iter().collect();
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
                let expr = if !each_joins.is_empty() {
                    let last_alias = &each_joins.last().expect("each_joins is non-empty").1;
                    format!(
                        "json_extract({}.value, '$.{}')",
                        last_alias,
                        json_path_parts.join(".")
                    )
                } else {
                    format!("json_extract(data, '$.{}')", json_path_parts.join("."))
                };

                return Ok((each_joins, expr));
            }
        }
    }

    bail!("Filter path must end on a scalar field or _block_type, not a container")
}

fn build_block_type_expr(
    each_joins: &[(String, String)],
    json_path_parts: &mut Vec<String>,
    _join_table: &str,
) -> String {
    if !each_joins.is_empty() {
        let last_alias = &each_joins.last().expect("each_joins is non-empty").1;

        if json_path_parts.is_empty() {
            format!("json_extract({}.value, '$._block_type')", last_alias)
        } else {
            json_path_parts.push("_block_type".to_string());
            format!(
                "json_extract({}.value, '$.{}')",
                last_alias,
                json_path_parts.join(".")
            )
        }
    } else {
        json_path_parts.push("_block_type".to_string());
        format!("json_extract(data, '$.{}')", json_path_parts.join("."))
    }
}

/// Build the source expression for a `json_each()` join.
///
/// If there are prior `json_each` joins, references the last alias's `.value`.
/// Otherwise, references `{join_table}.data`. Accumulated group path parts
/// are included in the JSON path.
pub(super) fn build_json_each_source(
    each_joins: &[(String, String)],
    json_path_parts: &[String],
    segment: &str,
    join_table: &str,
) -> String {
    let mut path_parts: Vec<&str> = json_path_parts.iter().map(|s| s.as_str()).collect();
    path_parts.push(segment);
    let json_path = path_parts.join(".");

    if let Some((_src, alias)) = each_joins.last() {
        format!("json_extract({}.value, '$.{}')", alias, json_path)
    } else {
        format!("json_extract({}.data, '$.{}')", join_table, json_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::field::{BlockDefinition, FieldDefinition, FieldType, RelationshipConfig};
    use crate::db::query::{Filter, FilterClause, FilterOp};

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
        let resolved = resolve_filter("status", "posts", &[]).unwrap();
        match resolved {
            ResolvedFilter::Column(col) => assert_eq!(col, "status"),
            other => panic!("Expected Column, got {:?}", other),
        }
    }

    #[test]
    fn resolve_filter_array_subfield() {
        let fields = vec![make_array_field(
            "items",
            vec![make_field("name", FieldType::Text, false)],
        )];
        let resolved = resolve_filter("items.name", "posts", &fields).unwrap();
        match resolved {
            ResolvedFilter::Subquery {
                join_table,
                parent_table,
                condition,
            } => {
                assert_eq!(join_table, "posts_items");
                assert_eq!(parent_table, "posts");
                match condition {
                    SubqueryCondition::Column(col) => assert_eq!(col, "name"),
                    other => panic!("Expected Column, got {:?}", other),
                }
            }
            other => panic!("Expected Subquery, got {:?}", other),
        }
    }

    #[test]
    fn resolve_filter_array_group_in_array() {
        let mut addr = make_field("address", FieldType::Group, false);
        addr.fields = vec![make_field("city", FieldType::Text, false)];
        let fields = vec![make_array_field("items", vec![addr])];

        let resolved = resolve_filter("items.address.city", "posts", &fields).unwrap();
        match resolved {
            ResolvedFilter::Subquery { condition, .. } => match condition {
                SubqueryCondition::Json {
                    each_joins,
                    extract_expr,
                } => {
                    assert!(each_joins.is_empty());
                    assert_eq!(extract_expr, "json_extract(address, '$.city')");
                }
                other => panic!("Expected Json, got {:?}", other),
            },
            other => panic!("Expected Subquery, got {:?}", other),
        }
    }

    #[test]
    fn resolve_filter_block_type() {
        let fields = vec![make_blocks_field("content", vec![])];
        let resolved = resolve_filter("content._block_type", "posts", &fields).unwrap();
        match resolved {
            ResolvedFilter::Subquery { condition, .. } => {
                assert!(matches!(condition, SubqueryCondition::BlockType));
            }
            other => panic!("Expected Subquery, got {:?}", other),
        }
    }

    #[test]
    fn resolve_filter_block_scalar() {
        let fields = vec![make_blocks_field(
            "content",
            vec![make_block_def(
                "paragraph",
                vec![make_field("body", FieldType::Textarea, false)],
            )],
        )];
        let resolved = resolve_filter("content.body", "posts", &fields).unwrap();
        match resolved {
            ResolvedFilter::Subquery { condition, .. } => match condition {
                SubqueryCondition::Json {
                    each_joins,
                    extract_expr,
                } => {
                    assert!(each_joins.is_empty());
                    assert_eq!(extract_expr, "json_extract(data, '$.body')");
                }
                other => panic!("Expected Json, got {:?}", other),
            },
            other => panic!("Expected Subquery, got {:?}", other),
        }
    }

    #[test]
    fn resolve_filter_has_many_relationship() {
        let fields = vec![make_has_many_field("tags", "tags")];
        let resolved = resolve_filter("tags.id", "posts", &fields).unwrap();
        match resolved {
            ResolvedFilter::Subquery {
                join_table,
                condition,
                ..
            } => {
                assert_eq!(join_table, "posts_tags");
                match condition {
                    SubqueryCondition::Column(col) => assert_eq!(col, "related_id"),
                    other => panic!("Expected Column, got {:?}", other),
                }
            }
            other => panic!("Expected Subquery, got {:?}", other),
        }
    }

    #[test]
    fn resolve_filter_has_many_rejects_non_id() {
        let fields = vec![make_has_many_field("tags", "tags")];
        let result = resolve_filter("tags.name", "posts", &fields);
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
        let fields = vec![
            FieldDefinition::builder("author", FieldType::Relationship)
                .relationship(RelationshipConfig::new("users", false))
                .build(),
        ];
        let result = resolve_filter("author.name", "posts", &fields);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Has-one"));
    }

    #[test]
    fn resolve_filter_unsupported_field_type() {
        let fields = vec![make_field("title", FieldType::Text, false)];
        let result = resolve_filter("title.sub", "posts", &fields);
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
        let fields = vec![FieldDefinition::builder("tags", FieldType::Relationship).build()];
        let result = resolve_filter("tags.id", "posts", &fields);
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
        let fields = vec![make_field("title", FieldType::Text, false)];
        let result = resolve_filter("nonexistent.sub", "posts", &fields);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Unknown field"));
    }

    #[test]
    fn resolve_filter_array_nested_non_group_error() {
        let fields = vec![make_array_field(
            "items",
            vec![make_field("name", FieldType::Text, false)],
        )];
        let result = resolve_filter("items.name.deep", "posts", &fields);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Nested dot path"));
    }

    #[test]
    fn resolve_filter_array_invalid_segment() {
        let fields = vec![make_array_field(
            "items",
            vec![make_field("name", FieldType::Text, false)],
        )];
        let result = resolve_filter("items.bad field", "posts", &fields);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid segment"));
    }

    #[test]
    fn resolve_filter_blocks_invalid_segment() {
        let fields = vec![make_blocks_field(
            "content",
            vec![make_block_def(
                "text",
                vec![make_field("body", FieldType::Textarea, false)],
            )],
        )];
        let result = resolve_filter("content.bad field", "posts", &fields);
        assert!(result.is_err());
    }

    // ── walk_block_fields ────────────────────────────────────────────────

    #[test]
    fn walk_block_simple_scalar() {
        let block_defs = vec![make_block_def(
            "text",
            vec![make_field("body", FieldType::Textarea, false)],
        )];
        let (joins, expr) = walk_block_fields(&["body"], &block_defs, "posts_content").unwrap();
        assert!(joins.is_empty());
        assert_eq!(expr, "json_extract(data, '$.body')");
    }

    #[test]
    fn walk_block_group_then_scalar() {
        let mut grp = make_field("meta", FieldType::Group, false);
        grp.fields = vec![make_field("title", FieldType::Text, false)];
        let block_defs = vec![make_block_def("rich", vec![grp])];

        let (joins, expr) =
            walk_block_fields(&["meta", "title"], &block_defs, "posts_content").unwrap();
        assert!(joins.is_empty());
        assert_eq!(expr, "json_extract(data, '$.meta.title')");
    }

    #[test]
    fn walk_block_nested_blocks_scalar() {
        let inner_blocks = vec![make_block_def(
            "quote",
            vec![make_field("text", FieldType::Text, false)],
        )];
        let mut nested = make_field("nested", FieldType::Blocks, false);
        nested.blocks = inner_blocks;
        let block_defs = vec![make_block_def("rich", vec![nested])];

        let (joins, expr) =
            walk_block_fields(&["nested", "text"], &block_defs, "posts_content").unwrap();
        assert_eq!(joins.len(), 1);
        assert_eq!(joins[0].0, "json_extract(posts_content.data, '$.nested')");
        assert_eq!(joins[0].1, "j0");
        assert_eq!(expr, "json_extract(j0.value, '$.text')");
    }

    #[test]
    fn walk_block_deeply_nested() {
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

        let (joins, expr) =
            walk_block_fields(&["nested", "deeper", "field"], &block_defs, "posts_content")
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
        let inner_blocks = vec![make_block_def(
            "quote",
            vec![make_field("text", FieldType::Text, false)],
        )];
        let mut nested = make_field("nested", FieldType::Blocks, false);
        nested.blocks = inner_blocks;
        let block_defs = vec![make_block_def("rich", vec![nested])];

        let (joins, expr) =
            walk_block_fields(&["nested", "_block_type"], &block_defs, "posts_content").unwrap();
        assert_eq!(joins.len(), 1);
        assert_eq!(expr, "json_extract(j0.value, '$._block_type')");
    }

    #[test]
    fn walk_block_group_then_nested_blocks() {
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

        let (joins, expr) =
            walk_block_fields(&["sidebar", "nested", "body"], &block_defs, "posts_content")
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
        let block_defs = vec![make_block_def("text", vec![])];
        let result = walk_block_fields(&[], &block_defs, "table");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Empty path"));
    }

    #[test]
    fn walk_block_scalar_with_subpath_error() {
        let block_defs = vec![make_block_def(
            "text",
            vec![make_field("body", FieldType::Textarea, false)],
        )];
        let result = walk_block_fields(&["body", "extra"], &block_defs, "table");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Scalar field"));
    }

    #[test]
    fn walk_block_container_as_leaf_error() {
        let mut nested = make_field("nested", FieldType::Blocks, false);
        nested.blocks = vec![make_block_def("inner", vec![])];
        let block_defs = vec![make_block_def("outer", vec![nested])];
        let result = walk_block_fields(&["nested"], &block_defs, "table");
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
        let block_defs = vec![make_block_def(
            "text",
            vec![make_field("body", FieldType::Textarea, false)],
        )];
        let result = walk_block_fields(&["_block_type", "extra"], &block_defs, "table");
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
        let block_defs = vec![make_block_def(
            "text",
            vec![make_field("body", FieldType::Textarea, false)],
        )];
        let (joins, expr) =
            walk_block_fields(&["_block_type"], &block_defs, "posts_content").unwrap();
        assert!(joins.is_empty());
        assert_eq!(expr, "json_extract(data, '$._block_type')");
    }

    #[test]
    fn walk_block_array_in_block() {
        let mut arr = make_field("items", FieldType::Array, false);
        arr.fields = vec![make_field("name", FieldType::Text, false)];
        let block_defs = vec![make_block_def("list", vec![arr])];

        let (joins, expr) =
            walk_block_fields(&["items", "name"], &block_defs, "posts_content").unwrap();
        assert_eq!(joins.len(), 1);
        assert_eq!(joins[0].0, "json_extract(posts_content.data, '$.items')");
        assert_eq!(expr, "json_extract(j0.value, '$.name')");
    }

    #[test]
    fn walk_block_nested_block_type_with_group_path() {
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

        let (joins, expr) = walk_block_fields(
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
