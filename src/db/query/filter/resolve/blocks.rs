//! Walk block-type definitions to build the JSON-extract expression and
//! `json_each` joins for a Blocks-field filter path.

use anyhow::{Result, anyhow, bail};

use crate::core::{
    BLOCK_TYPE_KEY, BlockDefinition, FieldChildren, FieldDefinition, FieldType, field_children,
    flatten_array_sub_fields,
};
use crate::db::{DbConnection, query::filter::elements::ListLeaf};

use super::types::BlockWalkResult;

/// Walk block type definitions to build `json_each` joins and a final
/// `json_extract` expression for a nested path.
///
/// At each segment:
/// - **Blocks/Array** sub-field → add a `json_each()` join, recurse
/// - **Group** sub-field → extend the JSON path (no join)
/// - **Scalar** → leaf node, produce `json_extract` expression (a leaf holding
///   a list — a scalar has-many list, a has-many reference's ids — is flagged
///   with it)
/// - **`_block_type`** → special: extract from current nesting level
///
/// Layout wrappers (row, collapsible, tabs) are transparent: a row stores
/// their sub-fields beside its own, so a path names those directly.
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
    let mut current_fields = block_fields(block_defs);

    let mut remaining = segments;

    while !remaining.is_empty() {
        let seg = remaining[0];
        remaining = &remaining[1..];

        // Handle _block_type at nested level — always Text.
        if seg == BLOCK_TYPE_KEY {
            if !remaining.is_empty() {
                bail!("{BLOCK_TYPE_KEY} must be the last segment in a filter path");
            }
            let expr = build_block_type_expr(conn, &each_joins, &mut json_path_parts, join_table);

            return Ok((each_joins, expr, Some(FieldType::Text), None));
        }

        let field_def = current_fields
            .iter()
            .find(|f| f.name == seg)
            .ok_or_else(|| anyhow!("Unknown field '{seg}' in block filter path"))?;

        match field_children(field_def) {
            // Nested array → json_each join, descend into its sub-fields.
            FieldChildren::Array(sub) => {
                let source =
                    build_json_each_source(conn, &each_joins, &json_path_parts, seg, join_table);
                let alias = format!("j{}", each_joins.len());
                each_joins.push((source, alias));
                json_path_parts.clear();

                current_fields = flatten_array_sub_fields(sub);
            }
            // Nested blocks → json_each join, descend into every block's fields.
            FieldChildren::Blocks(blocks) => {
                let source =
                    build_json_each_source(conn, &each_joins, &json_path_parts, seg, join_table);
                let alias = format!("j{}", each_joins.len());
                each_joins.push((source, alias));
                json_path_parts.clear();

                current_fields = block_fields(blocks);
            }
            // A group extends the JSON path (no join).
            FieldChildren::Group(sub) => {
                json_path_parts.push(seg.to_string());
                current_fields = flatten_array_sub_fields(sub);
            }
            // Never among the flattened fields: a wrapper holds no value of
            // its own.
            FieldChildren::Wrapper(_) | FieldChildren::Tabs(_) => {
                bail!("Layout field '{seg}' has no value of its own to filter on");
            }
            FieldChildren::Leaf => {
                if !remaining.is_empty() {
                    bail!("Scalar field '{seg}' cannot have sub-paths");
                }
                json_path_parts.push(seg.to_string());
                let path = json_path_parts.join(".");
                let expr = if each_joins.is_empty() {
                    conn.json_extract_expr("data", &path)
                } else {
                    let last_alias = &each_joins.last().expect("each_joins is non-empty").1;
                    conn.json_extract_expr(&format!("{last_alias}.value"), &path)
                };

                return Ok((
                    each_joins,
                    expr,
                    Some(field_def.field_type.clone()),
                    ListLeaf::of(field_def),
                ));
            }
        }
    }

    bail!("Filter path must end on a scalar field or _block_type, not a container")
}

/// Every block type's fields, layout wrappers flattened: a block row holds
/// them all under its own keys.
fn block_fields(block_defs: &[BlockDefinition]) -> Vec<&FieldDefinition> {
    block_defs
        .iter()
        .flat_map(|bd| flatten_array_sub_fields(&bd.fields))
        .collect()
}

fn build_block_type_expr(
    conn: &dyn DbConnection,
    each_joins: &[(String, String)],
    json_path_parts: &mut Vec<String>,
    _join_table: &str,
) -> String {
    if each_joins.is_empty() {
        json_path_parts.push(BLOCK_TYPE_KEY.to_string());
        conn.json_extract_expr("data", &json_path_parts.join("."))
    } else {
        let last_alias = &each_joins.last().expect("each_joins is non-empty").1;
        let source = format!("{last_alias}.value");

        if json_path_parts.is_empty() {
            conn.json_extract_expr(&source, BLOCK_TYPE_KEY)
        } else {
            json_path_parts.push(BLOCK_TYPE_KEY.to_string());
            conn.json_extract_expr(&source, &json_path_parts.join("."))
        }
    }
}

/// Build the source expression for a `json_each()` join.
///
/// If there are prior `json_each` joins, references the last alias's `.value`.
/// Otherwise, references `{join_table}.data`. Accumulated group path parts
/// are included in the JSON path.
fn build_json_each_source(
    conn: &dyn DbConnection,
    each_joins: &[(String, String)],
    json_path_parts: &[String],
    segment: &str,
    join_table: &str,
) -> String {
    let mut path_parts: Vec<&str> = json_path_parts
        .iter()
        .map(std::string::String::as_str)
        .collect();
    path_parts.push(segment);
    let json_path = path_parts.join(".");

    if let Some((_src, alias)) = each_joins.last() {
        conn.json_extract_expr(&format!("{alias}.value"), &json_path)
    } else {
        conn.json_extract_expr(&format!("{join_table}.data"), &json_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::query::filter::resolve::test_helpers::*;

    #[test]
    fn walk_block_simple_scalar() {
        let (_dir, conn) = test_conn();
        let block_defs = vec![make_block_def(
            "text",
            vec![make_field("body", FieldType::Textarea, false)],
        )];
        let (joins, expr, _leaf, _list) =
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

        let (joins, expr, _leaf, _list) =
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

        let (joins, expr, _leaf, _list) =
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

        let (joins, expr, _leaf, _list) = walk_block_fields(
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

        let (joins, expr, _leaf, _list) = walk_block_fields(
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

        let (joins, expr, _leaf, _list) = walk_block_fields(
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
        let (joins, expr, _leaf, _list) =
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

        let (joins, expr, _leaf, _list) =
            walk_block_fields(&conn, &["items", "name"], &block_defs, "posts_content").unwrap();
        assert_eq!(joins.len(), 1);
        assert_eq!(joins[0].0, "json_extract(posts_content.data, '$.items')");
        assert_eq!(expr, "json_extract(j0.value, '$.name')");
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

        let (joins, expr, _leaf, _list) = walk_block_fields(
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

    /// A scalar has-many leaf inside a block is flagged as a list, so its
    /// filter quantifies over the elements of the stored array.
    #[test]
    fn walk_block_flags_a_scalar_has_many_leaf_as_a_list() {
        let (_dir, conn) = test_conn();
        let tags = FieldDefinition::builder("tags", FieldType::Select)
            .has_many(true)
            .build();
        let block_defs = vec![make_block_def(
            "card",
            vec![tags, make_field("body", FieldType::Text, false)],
        )];

        let (_, expr, leaf, list) =
            walk_block_fields(&conn, &["tags"], &block_defs, "posts_content").unwrap();
        assert_eq!(expr, "json_extract(data, '$.tags')");
        assert_eq!(leaf, Some(FieldType::Select));
        assert_eq!(list, Some(ListLeaf::Scalar(FieldType::Select)));

        let (_, _, _, list) =
            walk_block_fields(&conn, &["body"], &block_defs, "posts_content").unwrap();
        assert!(list.is_none());
    }

    /// A has-many relationship inside a block is flagged with its id list.
    #[test]
    fn walk_block_flags_a_has_many_reference_as_a_list() {
        let (_dir, conn) = test_conn();
        let block_defs = vec![make_block_def(
            "card",
            vec![make_has_many_field("related", "tags")],
        )];

        let (_, _, _, list) =
            walk_block_fields(&conn, &["related"], &block_defs, "posts_content").unwrap();
        assert_eq!(list, Some(ListLeaf::References { polymorphic: false }));
    }

    /// Regression: a field inside a layout row of a block was unreachable —
    /// the walk looked among the block's direct fields and put the row's name
    /// into the JSON path, though a block row stores the field under its own
    /// key. The field is named directly, and extracted from its own key.
    #[test]
    fn walk_block_reaches_a_field_inside_a_layout_row() {
        let (_dir, conn) = test_conn();
        let row = FieldDefinition::builder("layout", FieldType::Row)
            .fields(vec![make_field("caption", FieldType::Text, false)])
            .build();
        let block_defs = vec![make_block_def("image", vec![row])];

        let (joins, expr, leaf, _) =
            walk_block_fields(&conn, &["caption"], &block_defs, "posts_content").unwrap();
        assert!(joins.is_empty());
        assert_eq!(expr, "json_extract(data, '$.caption')");
        assert_eq!(leaf, Some(FieldType::Text));

        assert!(
            walk_block_fields(&conn, &["layout", "caption"], &block_defs, "posts_content").is_err()
        );
    }
}
