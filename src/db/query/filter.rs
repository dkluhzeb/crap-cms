//! SQL filter/WHERE clause building, locale column resolution, and subquery
//! generation for array/block/relationship sub-field filtering.

use anyhow::{Result, bail};

use crate::core::CollectionDefinition;
use crate::core::field::{BlockDefinition, FieldDefinition, FieldType};
use super::{LocaleMode, LocaleContext, Filter, FilterClause, FilterOp, is_valid_identifier};

// ── Operator SQL generation ──────────────────────────────────────────────

/// Generate a SQL condition applying a [`FilterOp`] to an arbitrary SQL
/// expression, appending bind parameters to `params`.
fn build_op_condition(expr: &str, op: &FilterOp, params: &mut Vec<Box<dyn rusqlite::types::ToSql>>) -> String {
    match op {
        FilterOp::Equals(v) => {
            params.push(Box::new(v.clone()));
            format!("{} = ?", expr)
        }
        FilterOp::NotEquals(v) => {
            params.push(Box::new(v.clone()));
            format!("{} != ?", expr)
        }
        FilterOp::Like(v) => {
            params.push(Box::new(v.clone()));
            format!("{} LIKE ?", expr)
        }
        FilterOp::Contains(v) => {
            let escaped = v.replace('%', "\\%").replace('_', "\\_");
            params.push(Box::new(format!("%{}%", escaped)));
            format!("{} LIKE ? ESCAPE '\\'", expr)
        }
        FilterOp::GreaterThan(v) => {
            params.push(Box::new(v.clone()));
            format!("{} > ?", expr)
        }
        FilterOp::LessThan(v) => {
            params.push(Box::new(v.clone()));
            format!("{} < ?", expr)
        }
        FilterOp::GreaterThanOrEqual(v) => {
            params.push(Box::new(v.clone()));
            format!("{} >= ?", expr)
        }
        FilterOp::LessThanOrEqual(v) => {
            params.push(Box::new(v.clone()));
            format!("{} <= ?", expr)
        }
        FilterOp::In(vals) => {
            let placeholders: Vec<_> = vals.iter().map(|v| {
                params.push(Box::new(v.clone()));
                "?".to_string()
            }).collect();
            format!("{} IN ({})", expr, placeholders.join(", "))
        }
        FilterOp::NotIn(vals) => {
            let placeholders: Vec<_> = vals.iter().map(|v| {
                params.push(Box::new(v.clone()));
                "?".to_string()
            }).collect();
            format!("{} NOT IN ({})", expr, placeholders.join(", "))
        }
        FilterOp::Exists => {
            format!("{} IS NOT NULL", expr)
        }
        FilterOp::NotExists => {
            format!("{} IS NULL", expr)
        }
    }
}

/// Build a single [`Filter`] into a SQL condition string and append its bind
/// parameters to `params`.
///
/// Defense-in-depth: rejects field names that are not valid SQL identifiers
/// (alphanumeric + underscore), even though higher-level validation should
/// have caught them already.
pub fn build_filter_condition(f: &Filter, params: &mut Vec<Box<dyn rusqlite::types::ToSql>>) -> Result<String> {
    if !is_valid_identifier(&f.field) {
        bail!("Invalid field name '{}': must be alphanumeric/underscore", f.field);
    }
    Ok(build_op_condition(&f.field, &f.op, params))
}

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
    if let Some(fd) = fields.iter().find(|f| f.name == first_segment) {
        if fd.field_type == FieldType::Group {
            *field = field.replace('.', "__");
        }
    }
}

// ── Resolved filter types ────────────────────────────────────────────────

/// A filter resolved to its SQL representation.
#[derive(Debug)]
enum ResolvedFilter {
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
enum SubqueryCondition {
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
fn resolve_filter(field: &str, slug: &str, fields: &[FieldDefinition]) -> Result<ResolvedFilter> {
    if !field.contains('.') {
        return Ok(ResolvedFilter::Column(field.to_string()));
    }

    let dot_pos = field.find('.').unwrap();
    let root = &field[..dot_pos];
    let rest = &field[dot_pos + 1..];

    let field_def = fields.iter().find(|f| f.name == root)
        .ok_or_else(|| anyhow::anyhow!("Unknown field '{}' in filter path '{}'", root, field))?;

    let join_table = format!("{}_{}", slug, root);

    match field_def.field_type {
        FieldType::Array => {
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
                            rest, root
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
        FieldType::Blocks => {
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
                let (each_joins, extract_expr) = walk_block_fields(
                    &rest_parts, &field_def.blocks, &join_table,
                )?;
                Ok(ResolvedFilter::Subquery {
                    join_table,
                    parent_table: slug.to_string(),
                    condition: SubqueryCondition::Json { each_joins, extract_expr },
                })
            }
        }
        FieldType::Relationship => {
            if let Some(ref rc) = field_def.relationship {
                if rc.has_many {
                    if rest != "id" {
                        bail!(
                            "Has-many relationship '{}' can only be filtered by '.id', got '.{}'",
                            root, rest
                        );
                    }
                    Ok(ResolvedFilter::Subquery {
                        join_table,
                        parent_table: slug.to_string(),
                        condition: SubqueryCondition::Column("related_id".to_string()),
                    })
                } else {
                    bail!("Has-one relationship '{}' does not use dot notation for filtering", root);
                }
            } else {
                bail!("Relationship field '{}' missing relationship config", root);
            }
        }
        _ => {
            bail!(
                "Field '{}' (type {:?}) does not support sub-field filtering",
                root, field_def.field_type
            );
        }
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
fn walk_block_fields(
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
    let all_fields: Vec<&FieldDefinition> = block_defs.iter()
        .flat_map(|bd| bd.fields.iter())
        .collect();
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
            let expr = if !each_joins.is_empty() {
                let last_alias = &each_joins.last().unwrap().1;
                if json_path_parts.is_empty() {
                    format!("json_extract({}.value, '$._block_type')", last_alias)
                } else {
                    json_path_parts.push("_block_type".to_string());
                    format!("json_extract({}.value, '$.{}')", last_alias, json_path_parts.join("."))
                }
            } else {
                json_path_parts.push("_block_type".to_string());
                format!("json_extract(data, '$.{}')", json_path_parts.join("."))
            };
            return Ok((each_joins, expr));
        }

        let field_def = current_fields.iter()
            .find(|f| f.name == seg)
            .ok_or_else(|| anyhow::anyhow!("Unknown field '{}' in block filter path", seg))?;

        match field_def.field_type {
            FieldType::Blocks => {
                // Nested blocks → json_each join
                let source = build_json_each_source(
                    &each_joins, &json_path_parts, seg, join_table,
                );
                let alias = format!("j{}", each_joins.len());
                each_joins.push((source, alias));
                json_path_parts.clear();
                current_fields = field_def.blocks.iter()
                    .flat_map(|bd| bd.fields.iter())
                    .collect();
            }
            FieldType::Array => {
                // Nested array in block JSON → json_each join
                let source = build_json_each_source(
                    &each_joins, &json_path_parts, seg, join_table,
                );
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
                current_fields = field_def.tabs.iter().flat_map(|t| t.fields.iter()).collect();
            }
            _ => {
                // Scalar leaf
                if !remaining.is_empty() {
                    bail!("Scalar field '{}' cannot have sub-paths", seg);
                }
                json_path_parts.push(seg.to_string());
                let expr = if !each_joins.is_empty() {
                    let last_alias = &each_joins.last().unwrap().1;
                    format!("json_extract({}.value, '$.{}')", last_alias, json_path_parts.join("."))
                } else {
                    format!("json_extract(data, '$.{}')", json_path_parts.join("."))
                };
                return Ok((each_joins, expr));
            }
        }
    }

    bail!("Filter path must end on a scalar field or _block_type, not a container")
}

/// Build the source expression for a `json_each()` join.
///
/// If there are prior `json_each` joins, references the last alias's `.value`.
/// Otherwise, references `{join_table}.data`. Accumulated group path parts
/// are included in the JSON path.
fn build_json_each_source(
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

// ── Subquery SQL generation ──────────────────────────────────────────────

/// Build a complete SQL condition for a single filter, dispatching between
/// direct column conditions and EXISTS subqueries.
fn build_filter_sql(
    f: &Filter,
    slug: &str,
    fields: &[FieldDefinition],
    params: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
) -> Result<String> {
    let resolved = resolve_filter(&f.field, slug, fields)?;
    match resolved {
        ResolvedFilter::Column(col) => {
            build_filter_condition(&Filter { field: col, op: f.op.clone() }, params)
        }
        ResolvedFilter::Subquery { ref join_table, ref parent_table, ref condition } => {
            build_subquery_sql(join_table, parent_table, condition, &f.op, params)
        }
    }
}

/// Generate an `EXISTS (SELECT 1 FROM … WHERE …)` clause for a subquery filter.
fn build_subquery_sql(
    join_table: &str,
    parent_table: &str,
    condition: &SubqueryCondition,
    op: &FilterOp,
    params: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
) -> Result<String> {
    match condition {
        SubqueryCondition::Column(col) => {
            if !is_valid_identifier(col) {
                bail!("Invalid column name '{}' in subquery", col);
            }
            let cond = build_op_condition(col, op, params);
            Ok(format!(
                "EXISTS (SELECT 1 FROM {} WHERE parent_id = {}.id AND {})",
                join_table, parent_table, cond
            ))
        }
        SubqueryCondition::BlockType => {
            let cond = build_op_condition("_block_type", op, params);
            Ok(format!(
                "EXISTS (SELECT 1 FROM {} WHERE parent_id = {}.id AND {})",
                join_table, parent_table, cond
            ))
        }
        SubqueryCondition::Json { each_joins, extract_expr } => {
            let mut from_parts = vec![join_table.to_string()];
            for (source, alias) in each_joins {
                from_parts.push(format!("json_each({}) AS {}", source, alias));
            }
            let cond = build_op_condition(extract_expr, op, params);
            Ok(format!(
                "EXISTS (SELECT 1 FROM {} WHERE {}.parent_id = {}.id AND {})",
                from_parts.join(", "),
                join_table,
                parent_table,
                cond
            ))
        }
    }
}

// ── WHERE clause building ────────────────────────────────────────────────

/// Build a complete ` WHERE …` clause from a slice of [`FilterClause`]s.
///
/// Top-level clauses are joined with `AND`. An [`FilterClause::Or`] group
/// produces `(a OR b OR (c AND d))` sub-expressions, while
/// [`FilterClause::Single`] produces a plain condition.
///
/// Dot-notation fields (e.g., `items.name`, `content.body`) are resolved to
/// EXISTS subqueries against join tables. Non-dot fields use direct column
/// conditions.
///
/// Returns an **empty string** when `filters` is empty (no WHERE at all),
/// so callers can unconditionally append the result to their query.
pub fn build_where_clause(
    filters: &[FilterClause],
    slug: &str,
    fields: &[FieldDefinition],
    params: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
) -> Result<String> {
    if filters.is_empty() {
        return Ok(String::new());
    }

    let mut conditions = Vec::new();
    for clause in filters {
        match clause {
            FilterClause::Single(f) => {
                conditions.push(build_filter_sql(f, slug, fields, params)?);
            }
            FilterClause::Or(groups) => {
                if groups.len() == 1 && groups[0].len() == 1 {
                    conditions.push(build_filter_sql(&groups[0][0], slug, fields, params)?);
                } else {
                    let mut or_parts = Vec::new();
                    for group in groups {
                        if group.len() == 1 {
                            or_parts.push(build_filter_sql(&group[0], slug, fields, params)?);
                        } else {
                            let and_parts: Vec<String> = group.iter()
                                .map(|f| build_filter_sql(f, slug, fields, params))
                                .collect::<Result<_, _>>()?;
                            or_parts.push(format!("({})", and_parts.join(" AND ")));
                        }
                    }
                    conditions.push(format!("({})", or_parts.join(" OR ")));
                }
            }
        }
    }

    Ok(format!(" WHERE {}", conditions.join(" AND ")))
}

/// Resolve filter clauses to use locale-specific column names.
///
/// Walks every [`FilterClause`] (including nested OR groups) and replaces each
/// filter's field name with the locale-suffixed column name returned by
/// [`resolve_filter_column`]. Non-localized fields pass through unchanged.
///
/// This is a pure transformation — no database access required.
pub fn resolve_filters(filters: &[FilterClause], def: &CollectionDefinition, locale_ctx: Option<&LocaleContext>) -> Vec<FilterClause> {
    filters.iter().map(|clause| {
        match clause {
            FilterClause::Single(f) => {
                let resolved = resolve_filter_column(&f.field, def, locale_ctx);
                FilterClause::Single(Filter { field: resolved, op: f.op.clone() })
            }
            FilterClause::Or(groups) => {
                FilterClause::Or(groups.iter().map(|group| {
                    group.iter().map(|f| {
                        let resolved = resolve_filter_column(&f.field, def, locale_ctx);
                        Filter { field: resolved, op: f.op.clone() }
                    }).collect()
                }).collect())
            }
        }
    }).collect()
}

/// Map a filter field name to its actual SQL column name, accounting for locale.
///
/// When a [`LocaleContext`] is present and localization is enabled:
/// - If the field is a localized top-level field, returns `"field__locale"`.
/// - If the field is a group sub-field (`"group__sub"`) where either the group
///   or the sub-field is localized, returns `"group__sub__locale"`.
/// - For [`LocaleMode::Single`] the requested locale is used; otherwise the
///   default locale from config is used.
///
/// Non-localized fields (or disabled locale config) pass through unchanged.
pub fn resolve_filter_column(field_name: &str, def: &CollectionDefinition, locale_ctx: Option<&LocaleContext>) -> String {
    if let Some(ctx) = locale_ctx {
        if ctx.config.is_enabled() {
            // Check if this field is localized
            for field in &def.fields {
                if field.field_type == FieldType::Group {
                    let prefix = format!("{}__{}", field.name, "");
                    if field_name.starts_with(&prefix) {
                        let sub_name = &field_name[prefix.len()..];
                        for sub in &field.fields {
                            if sub.name == sub_name && (field.localized || sub.localized) {
                                let locale = match &ctx.mode {
                                    LocaleMode::Single(l) => l.as_str(),
                                    _ => ctx.config.default_locale.as_str(),
                                };
                                return format!("{}__{}", field_name, locale);
                            }
                        }
                    }
                } else if field.field_type == FieldType::Row || field.field_type == FieldType::Collapsible {
                    for sub in &field.fields {
                        if sub.name == field_name && sub.localized {
                            let locale = match &ctx.mode {
                                LocaleMode::Single(l) => l.as_str(),
                                _ => ctx.config.default_locale.as_str(),
                            };
                            return format!("{}__{}", field_name, locale);
                        }
                    }
                } else if field.field_type == FieldType::Tabs {
                    for tab in &field.tabs {
                        for sub in &tab.fields {
                            if sub.name == field_name && sub.localized {
                                let locale = match &ctx.mode {
                                    LocaleMode::Single(l) => l.as_str(),
                                    _ => ctx.config.default_locale.as_str(),
                                };
                                return format!("{}__{}", field_name, locale);
                            }
                        }
                    }
                } else if field.name == field_name && field.localized {
                    let locale = match &ctx.mode {
                        LocaleMode::Single(l) => l.as_str(),
                        _ => ctx.config.default_locale.as_str(),
                    };
                    return format!("{}__{}", field_name, locale);
                }
            }
        }
    }
    field_name.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LocaleConfig;
    use crate::core::collection::{
        CollectionAccess, CollectionAdmin, CollectionDefinition, CollectionHooks, CollectionLabels,
    };
    use crate::core::field::{BlockDefinition, FieldDefinition, FieldType, RelationshipConfig};
    use crate::db::query::{FilterClause, FilterOp, Filter, LocaleContext, LocaleMode};

    // ── Helpers ────────────────────────────────────────────────────────────

    fn make_field(name: &str, ft: FieldType, localized: bool) -> FieldDefinition {
        FieldDefinition {
            name: name.to_string(),
            field_type: ft,
            localized,
            ..Default::default()
        }
    }

    fn make_collection(fields: Vec<FieldDefinition>) -> CollectionDefinition {
        CollectionDefinition {
            slug: "test".to_string(),
            labels: CollectionLabels::default(),
            timestamps: true,
            fields,
            admin: CollectionAdmin::default(),
            hooks: CollectionHooks::default(),
            auth: None,
            upload: None,
            access: CollectionAccess::default(),
            live: None,
            versions: None,
        }
    }

    fn locale_config_en_de() -> LocaleConfig {
        LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        }
    }

    fn make_array_field(name: &str, sub_fields: Vec<FieldDefinition>) -> FieldDefinition {
        FieldDefinition {
            name: name.to_string(),
            field_type: FieldType::Array,
            fields: sub_fields,
            ..Default::default()
        }
    }

    fn make_blocks_field(name: &str, blocks: Vec<BlockDefinition>) -> FieldDefinition {
        FieldDefinition {
            name: name.to_string(),
            field_type: FieldType::Blocks,
            blocks,
            ..Default::default()
        }
    }

    fn make_has_many_field(name: &str, collection: &str) -> FieldDefinition {
        FieldDefinition {
            name: name.to_string(),
            field_type: FieldType::Relationship,
            relationship: Some(RelationshipConfig {
                collection: collection.to_string(),
                has_many: true,
                max_depth: None,
                polymorphic: vec![],
            }),
            ..Default::default()
        }
    }

    fn make_block_def(block_type: &str, fields: Vec<FieldDefinition>) -> BlockDefinition {
        BlockDefinition {
            block_type: block_type.to_string(),
            fields,
            ..Default::default()
        }
    }

    // ── build_filter_condition ─────────────────────────────────────────────

    #[test]
    fn filter_condition_equals() {
        let f = Filter { field: "status".into(), op: FilterOp::Equals("active".into()) };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_condition(&f, &mut params).unwrap();
        assert_eq!(sql, "status = ?");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn filter_condition_not_equals() {
        let f = Filter { field: "status".into(), op: FilterOp::NotEquals("draft".into()) };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_condition(&f, &mut params).unwrap();
        assert_eq!(sql, "status != ?");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn filter_condition_like() {
        let f = Filter { field: "title".into(), op: FilterOp::Like("%hello%".into()) };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_condition(&f, &mut params).unwrap();
        assert_eq!(sql, "title LIKE ?");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn filter_condition_contains() {
        let f = Filter { field: "body".into(), op: FilterOp::Contains("search term".into()) };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_condition(&f, &mut params).unwrap();
        assert_eq!(sql, "body LIKE ? ESCAPE '\\'");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn filter_condition_greater_than() {
        let f = Filter { field: "age".into(), op: FilterOp::GreaterThan("18".into()) };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_condition(&f, &mut params).unwrap();
        assert_eq!(sql, "age > ?");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn filter_condition_less_than() {
        let f = Filter { field: "price".into(), op: FilterOp::LessThan("100".into()) };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_condition(&f, &mut params).unwrap();
        assert_eq!(sql, "price < ?");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn filter_condition_greater_than_or_equal() {
        let f = Filter { field: "score".into(), op: FilterOp::GreaterThanOrEqual("50".into()) };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_condition(&f, &mut params).unwrap();
        assert_eq!(sql, "score >= ?");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn filter_condition_less_than_or_equal() {
        let f = Filter { field: "rating".into(), op: FilterOp::LessThanOrEqual("5".into()) };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_condition(&f, &mut params).unwrap();
        assert_eq!(sql, "rating <= ?");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn filter_condition_in() {
        let f = Filter {
            field: "status".into(),
            op: FilterOp::In(vec!["a".into(), "b".into(), "c".into()]),
        };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_condition(&f, &mut params).unwrap();
        assert_eq!(sql, "status IN (?, ?, ?)");
        assert_eq!(params.len(), 3);
    }

    #[test]
    fn filter_condition_not_in() {
        let f = Filter {
            field: "role".into(),
            op: FilterOp::NotIn(vec!["banned".into(), "suspended".into()]),
        };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_condition(&f, &mut params).unwrap();
        assert_eq!(sql, "role NOT IN (?, ?)");
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn filter_condition_exists() {
        let f = Filter { field: "avatar".into(), op: FilterOp::Exists };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_condition(&f, &mut params).unwrap();
        assert_eq!(sql, "avatar IS NOT NULL");
        assert_eq!(params.len(), 0);
    }

    #[test]
    fn filter_condition_not_exists() {
        let f = Filter { field: "deleted_at".into(), op: FilterOp::NotExists };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_condition(&f, &mut params).unwrap();
        assert_eq!(sql, "deleted_at IS NULL");
        assert_eq!(params.len(), 0);
    }

    #[test]
    fn filter_condition_rejects_invalid_identifier() {
        let f = Filter { field: "field name".into(), op: FilterOp::Equals("v".into()) };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let result = build_filter_condition(&f, &mut params);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid field name"));
    }

    // ── build_where_clause ────────────────────────────────────────────────

    #[test]
    fn where_clause_empty_filters() {
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_where_clause(&[], "test", &[], &mut params).unwrap();
        assert_eq!(sql, "");
        assert_eq!(params.len(), 0);
    }

    #[test]
    fn where_clause_single_filter() {
        let filters = vec![
            FilterClause::Single(Filter { field: "status".into(), op: FilterOp::Equals("active".into()) }),
        ];
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_where_clause(&filters, "test", &[], &mut params).unwrap();
        assert_eq!(sql, " WHERE status = ?");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn where_clause_multiple_and() {
        let filters = vec![
            FilterClause::Single(Filter { field: "status".into(), op: FilterOp::Equals("active".into()) }),
            FilterClause::Single(Filter { field: "role".into(), op: FilterOp::Equals("admin".into()) }),
        ];
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_where_clause(&filters, "test", &[], &mut params).unwrap();
        assert_eq!(sql, " WHERE status = ? AND role = ?");
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn where_clause_or_groups() {
        let filters = vec![
            FilterClause::Or(vec![
                vec![Filter { field: "a".into(), op: FilterOp::Equals("1".into()) }],
                vec![
                    Filter { field: "b".into(), op: FilterOp::Equals("2".into()) },
                    Filter { field: "c".into(), op: FilterOp::Equals("3".into()) },
                ],
            ]),
        ];
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_where_clause(&filters, "test", &[], &mut params).unwrap();
        assert_eq!(sql, " WHERE (a = ? OR (b = ? AND c = ?))");
        assert_eq!(params.len(), 3);
    }

    // ── normalize_filter_fields ──────────────────────────────────────────

    #[test]
    fn normalize_group_dot_to_double_underscore() {
        let fields = vec![make_field("seo", FieldType::Group, false)];
        let mut filters = vec![
            FilterClause::Single(Filter {
                field: "seo.meta_title".into(),
                op: FilterOp::Equals("test".into()),
            }),
        ];
        normalize_filter_fields(&mut filters, &fields);
        match &filters[0] {
            FilterClause::Single(f) => assert_eq!(f.field, "seo__meta_title"),
            other => panic!("Expected Single, got {:?}", other),
        }
    }

    #[test]
    fn normalize_preserves_array_dots() {
        let fields = vec![make_array_field("items", vec![
            make_field("name", FieldType::Text, false),
        ])];
        let mut filters = vec![
            FilterClause::Single(Filter {
                field: "items.name".into(),
                op: FilterOp::Equals("test".into()),
            }),
        ];
        normalize_filter_fields(&mut filters, &fields);
        match &filters[0] {
            FilterClause::Single(f) => assert_eq!(f.field, "items.name"),
            other => panic!("Expected Single, got {:?}", other),
        }
    }

    #[test]
    fn normalize_preserves_blocks_dots() {
        let fields = vec![make_blocks_field("content", vec![])];
        let mut filters = vec![
            FilterClause::Single(Filter {
                field: "content.body".into(),
                op: FilterOp::Equals("test".into()),
            }),
        ];
        normalize_filter_fields(&mut filters, &fields);
        match &filters[0] {
            FilterClause::Single(f) => assert_eq!(f.field, "content.body"),
            other => panic!("Expected Single, got {:?}", other),
        }
    }

    #[test]
    fn normalize_in_or_groups() {
        let fields = vec![make_field("seo", FieldType::Group, false)];
        let mut filters = vec![
            FilterClause::Or(vec![
                vec![Filter { field: "seo.title".into(), op: FilterOp::Equals("a".into()) }],
                vec![Filter { field: "seo.desc".into(), op: FilterOp::Equals("b".into()) }],
            ]),
        ];
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
        let mut filters = vec![
            FilterClause::Single(Filter {
                field: "title".into(),
                op: FilterOp::Equals("test".into()),
            }),
        ];
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
        let fields = vec![make_array_field("items", vec![
            make_field("name", FieldType::Text, false),
        ])];
        let resolved = resolve_filter("items.name", "posts", &fields).unwrap();
        match resolved {
            ResolvedFilter::Subquery { join_table, parent_table, condition } => {
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
            ResolvedFilter::Subquery { condition, .. } => {
                match condition {
                    SubqueryCondition::Json { each_joins, extract_expr } => {
                        assert!(each_joins.is_empty());
                        assert_eq!(extract_expr, "json_extract(address, '$.city')");
                    }
                    other => panic!("Expected Json, got {:?}", other),
                }
            }
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
        let fields = vec![make_blocks_field("content", vec![
            make_block_def("paragraph", vec![make_field("body", FieldType::Textarea, false)]),
        ])];
        let resolved = resolve_filter("content.body", "posts", &fields).unwrap();
        match resolved {
            ResolvedFilter::Subquery { condition, .. } => {
                match condition {
                    SubqueryCondition::Json { each_joins, extract_expr } => {
                        assert!(each_joins.is_empty());
                        assert_eq!(extract_expr, "json_extract(data, '$.body')");
                    }
                    other => panic!("Expected Json, got {:?}", other),
                }
            }
            other => panic!("Expected Subquery, got {:?}", other),
        }
    }

    #[test]
    fn resolve_filter_has_many_relationship() {
        let fields = vec![make_has_many_field("tags", "tags")];
        let resolved = resolve_filter("tags.id", "posts", &fields).unwrap();
        match resolved {
            ResolvedFilter::Subquery { join_table, condition, .. } => {
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
        assert!(result.unwrap_err().to_string().contains("only be filtered by '.id'"));
    }

    // ── walk_block_fields ────────────────────────────────────────────────

    #[test]
    fn walk_block_simple_scalar() {
        let block_defs = vec![
            make_block_def("text", vec![make_field("body", FieldType::Textarea, false)]),
        ];
        let (joins, expr) = walk_block_fields(&["body"], &block_defs, "posts_content").unwrap();
        assert!(joins.is_empty());
        assert_eq!(expr, "json_extract(data, '$.body')");
    }

    #[test]
    fn walk_block_group_then_scalar() {
        let mut grp = make_field("meta", FieldType::Group, false);
        grp.fields = vec![make_field("title", FieldType::Text, false)];
        let block_defs = vec![make_block_def("rich", vec![grp])];

        let (joins, expr) = walk_block_fields(&["meta", "title"], &block_defs, "posts_content").unwrap();
        assert!(joins.is_empty());
        assert_eq!(expr, "json_extract(data, '$.meta.title')");
    }

    #[test]
    fn walk_block_nested_blocks_scalar() {
        let inner_blocks = vec![
            make_block_def("quote", vec![make_field("text", FieldType::Text, false)]),
        ];
        let mut nested = make_field("nested", FieldType::Blocks, false);
        nested.blocks = inner_blocks;
        let block_defs = vec![make_block_def("rich", vec![nested])];

        let (joins, expr) = walk_block_fields(&["nested", "text"], &block_defs, "posts_content").unwrap();
        assert_eq!(joins.len(), 1);
        assert_eq!(joins[0].0, "json_extract(posts_content.data, '$.nested')");
        assert_eq!(joins[0].1, "j0");
        assert_eq!(expr, "json_extract(j0.value, '$.text')");
    }

    #[test]
    fn walk_block_deeply_nested() {
        // content -> nested -> deeper -> field
        let deep_blocks = vec![
            make_block_def("leaf", vec![make_field("field", FieldType::Text, false)]),
        ];
        let mut deeper = make_field("deeper", FieldType::Blocks, false);
        deeper.blocks = deep_blocks;
        let mid_blocks = vec![make_block_def("mid", vec![deeper])];
        let mut nested = make_field("nested", FieldType::Blocks, false);
        nested.blocks = mid_blocks;
        let block_defs = vec![make_block_def("top", vec![nested])];

        let (joins, expr) = walk_block_fields(
            &["nested", "deeper", "field"], &block_defs, "posts_content"
        ).unwrap();
        assert_eq!(joins.len(), 2);
        assert_eq!(joins[0].0, "json_extract(posts_content.data, '$.nested')");
        assert_eq!(joins[0].1, "j0");
        assert_eq!(joins[1].0, "json_extract(j0.value, '$.deeper')");
        assert_eq!(joins[1].1, "j1");
        assert_eq!(expr, "json_extract(j1.value, '$.field')");
    }

    #[test]
    fn walk_block_nested_block_type() {
        let inner_blocks = vec![
            make_block_def("quote", vec![make_field("text", FieldType::Text, false)]),
        ];
        let mut nested = make_field("nested", FieldType::Blocks, false);
        nested.blocks = inner_blocks;
        let block_defs = vec![make_block_def("rich", vec![nested])];

        let (joins, expr) = walk_block_fields(
            &["nested", "_block_type"], &block_defs, "posts_content"
        ).unwrap();
        assert_eq!(joins.len(), 1);
        assert_eq!(expr, "json_extract(j0.value, '$._block_type')");
    }

    #[test]
    fn walk_block_group_then_nested_blocks() {
        // group "sidebar" → blocks "nested" → scalar "body"
        let inner_blocks = vec![
            make_block_def("text", vec![make_field("body", FieldType::Textarea, false)]),
        ];
        let mut nested = make_field("nested", FieldType::Blocks, false);
        nested.blocks = inner_blocks;
        let mut sidebar = make_field("sidebar", FieldType::Group, false);
        sidebar.fields = vec![nested];
        let block_defs = vec![make_block_def("layout", vec![sidebar])];

        let (joins, expr) = walk_block_fields(
            &["sidebar", "nested", "body"], &block_defs, "posts_content"
        ).unwrap();
        assert_eq!(joins.len(), 1);
        assert_eq!(joins[0].0, "json_extract(posts_content.data, '$.sidebar.nested')");
        assert_eq!(expr, "json_extract(j0.value, '$.body')");
    }

    // ── build_subquery_sql ──────────────────────────────────────────────

    #[test]
    fn subquery_array_column() {
        let fields = vec![make_array_field("items", vec![
            make_field("name", FieldType::Text, false),
        ])];
        let f = Filter { field: "items.name".into(), op: FilterOp::Equals("X".into()) };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_sql(&f, "posts", &fields, &mut params).unwrap();
        assert_eq!(sql, "EXISTS (SELECT 1 FROM posts_items WHERE parent_id = posts.id AND name = ?)");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn subquery_block_type() {
        let fields = vec![make_blocks_field("content", vec![])];
        let f = Filter { field: "content._block_type".into(), op: FilterOp::Equals("image".into()) };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_sql(&f, "posts", &fields, &mut params).unwrap();
        assert_eq!(sql, "EXISTS (SELECT 1 FROM posts_content WHERE parent_id = posts.id AND _block_type = ?)");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn subquery_block_json_simple() {
        let fields = vec![make_blocks_field("content", vec![
            make_block_def("paragraph", vec![make_field("body", FieldType::Textarea, false)]),
        ])];
        let f = Filter { field: "content.body".into(), op: FilterOp::Contains("hello".into()) };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_sql(&f, "posts", &fields, &mut params).unwrap();
        assert_eq!(
            sql,
            "EXISTS (SELECT 1 FROM posts_content WHERE posts_content.parent_id = posts.id AND json_extract(data, '$.body') LIKE ? ESCAPE '\\')"
        );
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn subquery_block_nested_with_json_each() {
        let inner_blocks = vec![
            make_block_def("quote", vec![make_field("text", FieldType::Text, false)]),
        ];
        let mut nested = make_field("nested", FieldType::Blocks, false);
        nested.blocks = inner_blocks;
        let fields = vec![make_blocks_field("content", vec![
            make_block_def("rich", vec![nested]),
        ])];
        let f = Filter { field: "content.nested.text".into(), op: FilterOp::Equals("hi".into()) };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_sql(&f, "posts", &fields, &mut params).unwrap();
        assert_eq!(
            sql,
            "EXISTS (SELECT 1 FROM posts_content, json_each(json_extract(posts_content.data, '$.nested')) AS j0 WHERE posts_content.parent_id = posts.id AND json_extract(j0.value, '$.text') = ?)"
        );
    }

    #[test]
    fn subquery_has_many_relationship() {
        let fields = vec![make_has_many_field("tags", "tags")];
        let f = Filter { field: "tags.id".into(), op: FilterOp::Equals("tag1".into()) };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_sql(&f, "posts", &fields, &mut params).unwrap();
        assert_eq!(sql, "EXISTS (SELECT 1 FROM posts_tags WHERE parent_id = posts.id AND related_id = ?)");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn subquery_with_in_operator() {
        let fields = vec![make_has_many_field("tags", "tags")];
        let f = Filter { field: "tags.id".into(), op: FilterOp::In(vec!["a".into(), "b".into()]) };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_sql(&f, "posts", &fields, &mut params).unwrap();
        assert_eq!(sql, "EXISTS (SELECT 1 FROM posts_tags WHERE parent_id = posts.id AND related_id IN (?, ?))");
        assert_eq!(params.len(), 2);
    }

    // ── build_where_clause with subqueries ──────────────────────────────

    #[test]
    fn where_clause_mixed_column_and_subquery() {
        let fields = vec![
            make_field("status", FieldType::Text, false),
            make_array_field("items", vec![make_field("name", FieldType::Text, false)]),
        ];
        let filters = vec![
            FilterClause::Single(Filter { field: "status".into(), op: FilterOp::Equals("active".into()) }),
            FilterClause::Single(Filter { field: "items.name".into(), op: FilterOp::Equals("X".into()) }),
        ];
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_where_clause(&filters, "posts", &fields, &mut params).unwrap();
        assert_eq!(
            sql,
            " WHERE status = ? AND EXISTS (SELECT 1 FROM posts_items WHERE parent_id = posts.id AND name = ?)"
        );
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn where_clause_or_with_subquery() {
        let fields = vec![
            make_field("status", FieldType::Text, false),
            make_has_many_field("tags", "tags"),
        ];
        let filters = vec![
            FilterClause::Or(vec![
                vec![Filter { field: "status".into(), op: FilterOp::Equals("draft".into()) }],
                vec![Filter { field: "tags.id".into(), op: FilterOp::Equals("t1".into()) }],
            ]),
        ];
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_where_clause(&filters, "posts", &fields, &mut params).unwrap();
        assert_eq!(
            sql,
            " WHERE (status = ? OR EXISTS (SELECT 1 FROM posts_tags WHERE parent_id = posts.id AND related_id = ?))"
        );
        assert_eq!(params.len(), 2);
    }

    // ── resolve_filter_column ─────────────────────────────────────────────

    #[test]
    fn resolve_column_non_localized_passthrough() {
        let def = make_collection(vec![make_field("title", FieldType::Text, false)]);
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".into()),
            config: locale_config_en_de(),
        };
        let result = resolve_filter_column("title", &def, Some(&ctx));
        assert_eq!(result, "title");
    }

    #[test]
    fn resolve_column_localized_single_locale() {
        let def = make_collection(vec![make_field("title", FieldType::Text, true)]);
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".into()),
            config: locale_config_en_de(),
        };
        let result = resolve_filter_column("title", &def, Some(&ctx));
        assert_eq!(result, "title__de");
    }

    #[test]
    fn resolve_column_group_sub_field_localized() {
        let mut group = make_field("meta", FieldType::Group, false);
        let sub = make_field("description", FieldType::Text, true);
        group.fields = vec![sub];

        let def = make_collection(vec![group]);
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".into()),
            config: locale_config_en_de(),
        };
        let result = resolve_filter_column("meta__description", &def, Some(&ctx));
        assert_eq!(result, "meta__description__de");
    }

    // ── resolve_filters ───────────────────────────────────────────────────

    #[test]
    fn resolve_filters_non_localized_passthrough() {
        let def = make_collection(vec![make_field("status", FieldType::Text, false)]);
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".into()),
            config: locale_config_en_de(),
        };
        let filters = vec![
            FilterClause::Single(Filter { field: "status".into(), op: FilterOp::Equals("active".into()) }),
        ];
        let resolved = resolve_filters(&filters, &def, Some(&ctx));
        match &resolved[0] {
            FilterClause::Single(f) => assert_eq!(f.field, "status"),
            other => panic!("Expected Single, got {:?}", other),
        }
    }

    #[test]
    fn resolve_filter_has_one_relationship_rejects_dot() {
        let fields = vec![FieldDefinition {
            name: "author".to_string(),
            field_type: FieldType::Relationship,
            relationship: Some(RelationshipConfig {
                collection: "users".to_string(),
                has_many: false,
                max_depth: None,
                polymorphic: vec![],
            }),
            ..Default::default()
        }];
        let result = resolve_filter("author.name", "posts", &fields);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Has-one"));
    }

    #[test]
    fn resolve_filter_unsupported_field_type() {
        let fields = vec![make_field("title", FieldType::Text, false)];
        let result = resolve_filter("title.sub", "posts", &fields);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("does not support sub-field filtering"));
    }

    #[test]
    fn resolve_filter_relationship_missing_config() {
        let fields = vec![FieldDefinition {
            name: "tags".to_string(),
            field_type: FieldType::Relationship,
            relationship: None,
            ..Default::default()
        }];
        let result = resolve_filter("tags.id", "posts", &fields);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("missing relationship config"));
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
        let fields = vec![make_array_field("items", vec![
            make_field("name", FieldType::Text, false),
        ])];
        let result = resolve_filter("items.name.deep", "posts", &fields);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Nested dot path"));
    }

    #[test]
    fn resolve_filter_array_invalid_segment() {
        let fields = vec![make_array_field("items", vec![
            make_field("name", FieldType::Text, false),
        ])];
        let result = resolve_filter("items.bad field", "posts", &fields);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid segment"));
    }

    #[test]
    fn resolve_filter_blocks_invalid_segment() {
        let fields = vec![make_blocks_field("content", vec![
            make_block_def("text", vec![make_field("body", FieldType::Textarea, false)]),
        ])];
        let result = resolve_filter("content.bad field", "posts", &fields);
        assert!(result.is_err());
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
        let block_defs = vec![
            make_block_def("text", vec![make_field("body", FieldType::Textarea, false)]),
        ];
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
        assert!(result.unwrap_err().to_string().contains("must end on a scalar"));
    }

    #[test]
    fn walk_block_block_type_not_last_error() {
        let block_defs = vec![
            make_block_def("text", vec![make_field("body", FieldType::Textarea, false)]),
        ];
        let result = walk_block_fields(&["_block_type", "extra"], &block_defs, "table");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("_block_type must be the last segment"));
    }

    #[test]
    fn walk_block_top_level_block_type_without_joins() {
        let block_defs = vec![
            make_block_def("text", vec![make_field("body", FieldType::Textarea, false)]),
        ];
        let (joins, expr) = walk_block_fields(&["_block_type"], &block_defs, "posts_content").unwrap();
        assert!(joins.is_empty());
        assert_eq!(expr, "json_extract(data, '$._block_type')");
    }

    #[test]
    fn walk_block_array_in_block() {
        let mut arr = make_field("items", FieldType::Array, false);
        arr.fields = vec![make_field("name", FieldType::Text, false)];
        let block_defs = vec![make_block_def("list", vec![arr])];

        let (joins, expr) = walk_block_fields(&["items", "name"], &block_defs, "posts_content").unwrap();
        assert_eq!(joins.len(), 1);
        assert_eq!(joins[0].0, "json_extract(posts_content.data, '$.items')");
        assert_eq!(expr, "json_extract(j0.value, '$.name')");
    }

    #[test]
    fn walk_block_nested_block_type_with_group_path() {
        // group "meta" → nested blocks → _block_type
        let inner_blocks = vec![
            make_block_def("quote", vec![make_field("text", FieldType::Text, false)]),
        ];
        let mut nested = make_field("nested", FieldType::Blocks, false);
        nested.blocks = inner_blocks;
        let mut meta = make_field("meta", FieldType::Group, false);
        meta.fields = vec![nested];
        let block_defs = vec![make_block_def("rich", vec![meta])];

        let (joins, expr) = walk_block_fields(
            &["meta", "nested", "_block_type"], &block_defs, "posts_content"
        ).unwrap();
        assert_eq!(joins.len(), 1);
        assert_eq!(joins[0].0, "json_extract(posts_content.data, '$.meta.nested')");
        assert_eq!(expr, "json_extract(j0.value, '$._block_type')");
    }

    // ── resolve_filter_column with default mode ──────────────────────────

    #[test]
    fn resolve_column_localized_default_mode() {
        let def = make_collection(vec![make_field("title", FieldType::Text, true)]);
        let ctx = LocaleContext {
            mode: LocaleMode::Default,
            config: locale_config_en_de(),
        };
        let result = resolve_filter_column("title", &def, Some(&ctx));
        assert_eq!(result, "title__en");
    }

    #[test]
    fn resolve_column_group_localized_default_mode() {
        let mut group = make_field("meta", FieldType::Group, true);
        group.fields = vec![make_field("title", FieldType::Text, false)];
        let def = make_collection(vec![group]);
        let ctx = LocaleContext {
            mode: LocaleMode::Default,
            config: locale_config_en_de(),
        };
        let result = resolve_filter_column("meta__title", &def, Some(&ctx));
        assert_eq!(result, "meta__title__en");
    }

    #[test]
    fn resolve_column_no_locale_ctx() {
        let def = make_collection(vec![make_field("title", FieldType::Text, true)]);
        let result = resolve_filter_column("title", &def, None);
        assert_eq!(result, "title");
    }

    // ── where clause with single-item OR ────────────────────────────────

    #[test]
    fn where_clause_or_single_item_group() {
        let filters = vec![
            FilterClause::Or(vec![
                vec![Filter { field: "a".into(), op: FilterOp::Equals("1".into()) }],
            ]),
        ];
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_where_clause(&filters, "test", &[], &mut params).unwrap();
        // Single-item OR should simplify to just the condition
        assert_eq!(sql, " WHERE a = ?");
    }

    // ── resolve_filters with OR ──────────────────────────────────────────

    #[test]
    fn resolve_filters_or_groups() {
        let def = make_collection(vec![make_field("title", FieldType::Text, true)]);
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".into()),
            config: locale_config_en_de(),
        };
        let filters = vec![
            FilterClause::Or(vec![
                vec![Filter { field: "title".into(), op: FilterOp::Equals("A".into()) }],
                vec![Filter { field: "title".into(), op: FilterOp::Equals("B".into()) }],
            ]),
        ];
        let resolved = resolve_filters(&filters, &def, Some(&ctx));
        match &resolved[0] {
            FilterClause::Or(groups) => {
                assert_eq!(groups[0][0].field, "title__de");
                assert_eq!(groups[1][0].field, "title__de");
            }
            other => panic!("Expected Or, got {:?}", other),
        }
    }

    #[test]
    fn resolve_filters_applies_locale() {
        let def = make_collection(vec![make_field("title", FieldType::Text, true)]);
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".into()),
            config: locale_config_en_de(),
        };
        let filters = vec![
            FilterClause::Single(Filter { field: "title".into(), op: FilterOp::Equals("Hallo".into()) }),
        ];
        let resolved = resolve_filters(&filters, &def, Some(&ctx));
        match &resolved[0] {
            FilterClause::Single(f) => assert_eq!(f.field, "title__de"),
            other => panic!("Expected Single, got {:?}", other),
        }
    }
}
