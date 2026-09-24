//! Walk a filter path through the JSON a row stores — a block row's `data`, or
//! a group / nested array / nested blocks column of an array row — to the
//! `json_extract` expression of its leaf and the `json_each` joins that expand
//! every nested array or blocks value on the way.
//!
//! One walker for both row kinds, so a path reaches the same depth whichever
//! row it starts in: groups extend the JSON path, a nested array or blocks
//! field expands its rows, at any depth.

use anyhow::{Result, anyhow, bail};

use crate::{
    core::{
        BLOCK_TYPE_KEY, BlockDefinition, FieldChildren, FieldDefinition, FieldType, field_children,
        flatten_array_sub_fields,
    },
    db::{DbConnection, query::filter::elements::ListLeaf},
};

use super::types::JsonWalkResult;

/// A walk's position: the JSON value the next segment is looked up in —
/// `path` below `base` — the fields that value holds, and whether it is a
/// block row (the only level that carries a `_block_type`).
pub(super) struct JsonWalk<'a> {
    base: String,
    path: Vec<String>,
    each_joins: Vec<(String, String)>,
    fields: Vec<&'a FieldDefinition>,
    block_row: bool,
}

impl<'a> JsonWalk<'a> {
    fn at(base: String, fields: Vec<&'a FieldDefinition>, block_row: bool) -> Self {
        Self {
            base,
            path: Vec::new(),
            each_joins: Vec::new(),
            fields,
            block_row,
        }
    }

    /// A walk starting in a block row's `data` object of `join_table`, which
    /// holds every block type's fields.
    pub(super) fn block_row(join_table: &str, blocks: &'a [BlockDefinition]) -> Self {
        Self::at(format!("{join_table}.data"), block_fields(blocks), true)
    }

    /// A walk starting in the JSON an array row's `container` sub-field holds
    /// in `column`: a group's object, or a nested array's or blocks' rows.
    ///
    /// # Errors
    ///
    /// Returns an error when `container` holds a value rather than JSON with
    /// sub-fields.
    pub(super) fn array_column(
        conn: &dyn DbConnection,
        column: String,
        container: &'a FieldDefinition,
    ) -> Result<Self> {
        let mut walk = Self::at(column, Vec::new(), false);

        walk.enter(conn, container, None)?;

        Ok(walk)
    }

    /// Walk `segments` to their leaf.
    ///
    /// At each segment:
    /// - **Array/Blocks** field → a `json_each` join over its rows, descend
    /// - **Group** field → extend the JSON path (no join), descend
    /// - **Value** field → the leaf: its `json_extract` expression (a leaf
    ///   holding a list — a scalar has-many list, a has-many reference's ids —
    ///   is flagged with it)
    /// - **`_block_type`** → the type of the block row the walk is in
    ///
    /// Layout wrappers (row, collapsible, tabs) are transparent: a row stores
    /// their sub-fields beside its own, so a path names those directly.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty path, an unknown field, a sub-path into a
    /// value, `_block_type` outside a block row or before the last segment, and
    /// a path that ends on a container.
    pub(super) fn walk(
        mut self,
        conn: &dyn DbConnection,
        segments: &[&str],
    ) -> Result<JsonWalkResult> {
        let Some(last) = segments.last() else {
            bail!("Empty path for a row filter");
        };

        let mut remaining = segments;

        while let Some((seg, rest)) = remaining.split_first() {
            if *seg == BLOCK_TYPE_KEY {
                return self.block_type(conn, rest);
            }

            let field = self.field(seg)?;

            if matches!(field_children(field), FieldChildren::Leaf) {
                if !rest.is_empty() {
                    bail!("Scalar field '{seg}' cannot have sub-paths");
                }

                let field_type = Some(field.field_type.clone());

                return Ok(self.leaf(conn, seg, field_type, ListLeaf::of(field)));
            }

            self.enter(conn, field, Some(*seg))?;
            remaining = rest;
        }

        bail!("Filter path must end on a value field, not the container '{last}'")
    }

    /// The field `name` at the current level.
    fn field(&self, name: &str) -> Result<&'a FieldDefinition> {
        self.fields
            .iter()
            .find(|f| f.name == name)
            .copied()
            .ok_or_else(|| anyhow!("Unknown field '{name}' in row filter path"))
    }

    /// Descend into the container `field` — reached by its `key` below the
    /// current position, or, without one, as the whole current value.
    fn enter(
        &mut self,
        conn: &dyn DbConnection,
        field: &'a FieldDefinition,
        key: Option<&str>,
    ) -> Result<()> {
        if let Some(key) = key {
            self.path.push(key.to_string());
        }

        match field_children(field) {
            FieldChildren::Group(sub) => {
                self.fields = flatten_array_sub_fields(sub);
                self.block_row = false;
            }
            FieldChildren::Array(sub) => {
                self.expand_rows(conn);
                self.fields = flatten_array_sub_fields(sub);
                self.block_row = false;
            }
            FieldChildren::Blocks(blocks) => {
                self.expand_rows(conn);
                self.fields = block_fields(blocks);
                self.block_row = true;
            }
            // Never among the flattened fields: a wrapper holds no value of
            // its own.
            FieldChildren::Wrapper(_) | FieldChildren::Tabs(_) => {
                bail!(
                    "Layout field '{}' has no value of its own to filter on",
                    field.name
                );
            }
            FieldChildren::Leaf => bail!("Field '{}' has no sub-fields to filter on", field.name),
        }

        Ok(())
    }

    /// Expand the rows at the current position with a `json_each` join, and
    /// move into one of them.
    fn expand_rows(&mut self, conn: &dyn DbConnection) {
        let source = self.extract(conn);
        let alias = format!("j{}", self.each_joins.len());

        self.base = format!("{alias}.value");
        self.path.clear();
        self.each_joins.push((source, alias));
    }

    /// The expression of the value at the current position.
    fn extract(&self, conn: &dyn DbConnection) -> String {
        if self.path.is_empty() {
            return self.base.clone();
        }

        conn.json_extract_expr(&self.base, &self.path.join("."))
    }

    /// The leaf `key` at the current position.
    fn leaf(
        mut self,
        conn: &dyn DbConnection,
        key: &str,
        field_type: Option<FieldType>,
        list: Option<ListLeaf>,
    ) -> JsonWalkResult {
        self.path.push(key.to_string());
        let expr = self.extract(conn);

        (self.each_joins, expr, field_type, list)
    }

    /// The `_block_type` of the block row the walk is in — the last segment,
    /// and only inside a blocks field's rows.
    fn block_type(self, conn: &dyn DbConnection, rest: &[&str]) -> Result<JsonWalkResult> {
        if !rest.is_empty() {
            bail!("{BLOCK_TYPE_KEY} must be the last segment in a filter path");
        }

        if !self.block_row {
            bail!("{BLOCK_TYPE_KEY} names a block row's type; this path is not in a block row");
        }

        Ok(self.leaf(conn, BLOCK_TYPE_KEY, Some(FieldType::Text), None))
    }
}

/// Every block type's fields, layout wrappers flattened: a block row holds
/// them all under its own keys.
fn block_fields(block_defs: &[BlockDefinition]) -> Vec<&FieldDefinition> {
    block_defs
        .iter()
        .flat_map(|bd| flatten_array_sub_fields(&bd.fields))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::query::filter::resolve::test_helpers::*;

    fn walk_block_fields(
        conn: &dyn DbConnection,
        segments: &[&str],
        blocks: &[BlockDefinition],
        join_table: &str,
    ) -> Result<JsonWalkResult> {
        JsonWalk::block_row(join_table, blocks).walk(conn, segments)
    }

    fn walk_array_column(
        conn: &dyn DbConnection,
        container: &FieldDefinition,
        segments: &[&str],
    ) -> Result<JsonWalkResult> {
        let column = format!("\"posts_items\".\"{}\"", container.name);

        JsonWalk::array_column(conn, column, container)?.walk(conn, segments)
    }

    /// A group column of an array row is read by its JSON path, groups nesting.
    #[test]
    fn array_group_column_extends_the_path() {
        let (_dir, conn) = test_conn();
        let mut geo = make_field("geo", FieldType::Group, false);
        geo.fields = vec![make_field("lat", FieldType::Number, false)];
        let mut address = make_field("address", FieldType::Group, false);
        address.fields = vec![geo];

        let (joins, expr, leaf, _) = walk_array_column(&conn, &address, &["geo", "lat"]).unwrap();
        assert!(joins.is_empty());
        assert_eq!(
            expr,
            "json_extract(\"posts_items\".\"address\", '$.geo.lat')"
        );
        assert_eq!(leaf, Some(FieldType::Number));
    }

    /// Regression: an array row's nested array was not filterable — only a
    /// group column was. Its rows are expanded straight from the column.
    #[test]
    fn array_nested_array_column_expands_its_rows() {
        let (_dir, conn) = test_conn();
        let sizes = make_array_field("sizes", vec![make_field("label", FieldType::Text, false)]);

        let (joins, expr, _, _) = walk_array_column(&conn, &sizes, &["label"]).unwrap();
        assert_eq!(
            joins,
            vec![("\"posts_items\".\"sizes\"".to_string(), "j0".to_string())]
        );
        assert_eq!(expr, "json_extract(j0.value, '$.label')");
    }

    /// Regression: an array row's nested blocks were not filterable; now at
    /// any depth, `_block_type` included.
    #[test]
    fn array_nested_blocks_column_reaches_any_depth() {
        let (_dir, conn) = test_conn();
        let inner = make_array_field("rows", vec![make_field("cell", FieldType::Text, false)]);
        let sections = make_blocks_field("sections", vec![make_block_def("grid", vec![inner])]);

        let (joins, expr, _, _) = walk_array_column(&conn, &sections, &["rows", "cell"]).unwrap();
        assert_eq!(joins.len(), 2);
        assert_eq!(joins[0].0, "\"posts_items\".\"sections\"");
        assert_eq!(joins[1].0, "json_extract(j0.value, '$.rows')");
        assert_eq!(expr, "json_extract(j1.value, '$.cell')");

        let (_, expr, _, _) = walk_array_column(&conn, &sections, &["_block_type"]).unwrap();
        assert_eq!(expr, "json_extract(j0.value, '$._block_type')");
    }

    /// `_block_type` names a block row's type; a group or an array row has
    /// none, so a path asking for it there is refused rather than read as NULL.
    #[test]
    fn block_type_outside_a_block_row_is_rejected() {
        let (_dir, conn) = test_conn();
        let mut meta = make_field("meta", FieldType::Group, false);
        meta.fields = vec![make_field("title", FieldType::Text, false)];
        let block_defs = vec![make_block_def("rich", vec![meta.clone()])];

        let err = walk_block_fields(
            &conn,
            &["meta", "_block_type"],
            &block_defs,
            "posts_content",
        )
        .unwrap_err();
        assert!(err.to_string().contains("not in a block row"), "{err}");

        let err = walk_array_column(&conn, &meta, &["_block_type"]).unwrap_err();
        assert!(err.to_string().contains("not in a block row"), "{err}");
    }

    #[test]
    fn a_value_sub_field_is_not_a_container() {
        let (_dir, conn) = test_conn();
        let name = make_field("name", FieldType::Text, false);

        let err = walk_array_column(&conn, &name, &["x"]).unwrap_err();
        assert!(err.to_string().contains("has no sub-fields"), "{err}");
    }

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
        assert_eq!(expr, "json_extract(posts_content.data, '$.body')");
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
        assert_eq!(expr, "json_extract(posts_content.data, '$.meta.title')");
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
                .contains("must end on a value field")
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
        assert_eq!(expr, "json_extract(posts_content.data, '$._block_type')");
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
        assert_eq!(expr, "json_extract(posts_content.data, '$.tags')");
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
        assert_eq!(expr, "json_extract(posts_content.data, '$.caption')");
        assert_eq!(leaf, Some(FieldType::Text));

        assert!(
            walk_block_fields(&conn, &["layout", "caption"], &block_defs, "posts_content").is_err()
        );
    }
}
