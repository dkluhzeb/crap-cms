//! Walk a filter path through the JSON a row stores — a block row's `data`, or
//! a group / nested array / nested blocks column of an array row — to the
//! `json_extract` expression of its leaf and the `json_each` joins that expand
//! every nested array or blocks value on the way.
//!
//! One walker for both row kinds, so a path reaches the same depth whichever
//! row it starts in: groups extend the JSON path, a nested array or blocks
//! field expands its rows, at any depth.
//!
//! A block row holds its own block type's fields. Where block types define a
//! name differently the walk forks, one reading per block type, each only for
//! rows of that type (see `filter::row_fields`); a path that holds for at
//! least one block type is accepted. Rows of a block type declaring no field
//! of that name get one more reading: the value is absent there and reads as
//! NULL — as it does when every declaring type defines the name alike, where
//! the one unconditional extract finds no key in such a row.

use anyhow::{Result, anyhow, bail};

use crate::{
    core::{
        BLOCK_TYPE_KEY, BlockDefinition, FieldChildren, FieldDefinition, FieldType, field_children,
    },
    db::{
        DbConnection,
        query::filter::{
            elements::ListLeaf,
            row_fields::{
                RowField, RowLookup, block_row_fields, lookup_row_field, plain_row_fields,
            },
        },
    },
};

use super::types::{JsonLeaf, JsonStep};

/// The value a row whose block type declares no field of the filtered name
/// holds there: NULL, typed so every backend compares it.
const ABSENT_VALUE: &str = "CAST(NULL AS TEXT)";

/// A walk's position: the JSON value the next segment is looked up in —
/// `path` below `base` — the steps taken so far, the fields that value holds,
/// and, in a block row, the expression of the row's block type.
#[derive(Clone)]
pub(super) struct JsonWalk<'a> {
    base: String,
    path: Vec<String>,
    steps: Vec<JsonStep>,
    fields: Vec<RowField<'a>>,
    row_type: Option<String>,
}

impl<'a> JsonWalk<'a> {
    fn at(base: String, fields: Vec<RowField<'a>>, row_type: Option<String>) -> Self {
        Self {
            base,
            path: Vec::new(),
            steps: Vec::new(),
            fields,
            row_type,
        }
    }

    /// A walk starting in a block row's `data` object of `join_table`, which
    /// holds its block type's fields; the type itself is the row's
    /// `_block_type` column.
    pub(super) fn block_row(join_table: &str, blocks: &'a [BlockDefinition]) -> Self {
        Self::at(
            format!("{join_table}.data"),
            block_row_fields(blocks),
            Some(format!("{join_table}.{BLOCK_TYPE_KEY}")),
        )
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
        let mut walk = Self::at(column, Vec::new(), None);

        walk.enter(conn, container, None)?;

        Ok(walk)
    }

    /// Walk `segments` to their leaf — one reading per block type where block
    /// types define a segment differently.
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
    /// value, a field storing no value (a join), `_block_type` outside a block
    /// row or before the last segment, and a path that ends on a container —
    /// for a segment block types define differently, only when the path fails
    /// for every one of them (the first block type's error).
    pub(super) fn walk(self, conn: &dyn DbConnection, segments: &[&str]) -> Result<Vec<JsonLeaf>> {
        let Some((segment, rest)) = segments.split_first() else {
            bail!("Empty path for a row filter");
        };

        if *segment == BLOCK_TYPE_KEY {
            return Ok(vec![self.block_type(rest)?]);
        }

        match lookup_row_field(&self.fields, segment) {
            RowLookup::Unknown => bail!("Unknown field '{segment}' in row filter path"),
            RowLookup::Uniform(field) => self.step(conn, field, segment, rest),
            RowLookup::PerBlockType(candidates) => {
                self.per_block_type(conn, &candidates, segment, rest)
            }
        }
    }

    /// Continue the walk through `field`, reached by `segment`, with `rest`
    /// below it.
    fn step(
        mut self,
        conn: &dyn DbConnection,
        field: &'a FieldDefinition,
        segment: &str,
        rest: &[&str],
    ) -> Result<Vec<JsonLeaf>> {
        if !matches!(field_children(field), FieldChildren::Leaf) {
            self.enter(conn, field, Some(segment))?;

            if rest.is_empty() {
                bail!("Filter path must end on a value field, not the container '{segment}'");
            }

            return self.walk(conn, rest);
        }

        if !rest.is_empty() {
            bail!("Scalar field '{segment}' cannot have sub-paths");
        }

        if !field.field_type.is_writable() {
            bail!(
                "Field '{segment}' (type {:?}) stores no value to filter on",
                field.field_type
            );
        }

        let field_type = Some(field.field_type.clone());

        Ok(vec![self.leaf(
            conn,
            segment,
            field_type,
            ListLeaf::of(field),
        )])
    }

    /// The readings of `segment` in a block row whose block types define it
    /// differently: each candidate's, for rows of its block type only, and the
    /// absent reading for rows of every other type. Fails only when the path
    /// fails for every declaring block type.
    fn per_block_type(
        &self,
        conn: &dyn DbConnection,
        candidates: &[(&'a str, &'a FieldDefinition)],
        segment: &str,
        rest: &[&str],
    ) -> Result<Vec<JsonLeaf>> {
        let mut leaves = Vec::new();
        let mut first_error = None;

        for &(block_type, field) in candidates {
            match self
                .of_block_type(block_type)
                .step(conn, field, segment, rest)
            {
                Ok(found) => leaves.extend(found),
                Err(e) => {
                    first_error.get_or_insert(e);
                }
            }
        }

        if leaves.is_empty() {
            return Err(first_error.unwrap_or_else(|| anyhow!("Unknown field '{segment}'")));
        }

        leaves.extend(self.absent(candidates));

        Ok(leaves)
    }

    /// The reading of a block row whose type declares none of `candidates`:
    /// the value is absent, read as NULL. `None` outside a block row.
    fn absent(&self, candidates: &[(&'a str, &'a FieldDefinition)]) -> Option<JsonLeaf> {
        let expr = self.row_type.clone()?;

        let mut declared: Vec<String> = candidates
            .iter()
            .map(|(block_type, _)| (*block_type).to_string())
            .collect();
        declared.sort();
        declared.dedup();

        let mut walk = self.clone();
        walk.steps.push(JsonStep::OtherBlockType { expr, declared });

        Some(walk.finish(ABSENT_VALUE.to_string(), None, None))
    }

    /// This walk, restricted to block rows of `block_type`.
    fn of_block_type(&self, block_type: &str) -> Self {
        let mut walk = self.clone();

        if let Some(expr) = &self.row_type {
            walk.steps.push(JsonStep::BlockType {
                expr: expr.clone(),
                block_type: block_type.to_string(),
            });
        }

        walk
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
                self.fields = plain_row_fields(sub);
                self.row_type = None;
            }
            FieldChildren::Array(sub) => {
                self.expand_rows(conn);
                self.fields = plain_row_fields(sub);
                self.row_type = None;
            }
            FieldChildren::Blocks(blocks) => {
                self.expand_rows(conn);
                self.fields = block_row_fields(blocks);
                self.row_type = Some(conn.json_extract_expr(&self.base, BLOCK_TYPE_KEY));
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
        let joins = self
            .steps
            .iter()
            .filter(|step| matches!(step, JsonStep::Each { .. }))
            .count();
        let alias = format!("j{joins}");

        self.base = format!("{alias}.value");
        self.path.clear();
        self.steps.push(JsonStep::Each { source, alias });
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
    ) -> JsonLeaf {
        self.path.push(key.to_string());
        let extract_expr = self.extract(conn);

        self.finish(extract_expr, field_type, list)
    }

    /// The reading ending in `extract_expr`, after the steps taken so far.
    fn finish(
        self,
        extract_expr: String,
        field_type: Option<FieldType>,
        list: Option<ListLeaf>,
    ) -> JsonLeaf {
        JsonLeaf {
            steps: self.steps,
            extract_expr,
            field_type,
            list,
        }
    }

    /// The `_block_type` of the block row the walk is in — the last segment,
    /// and only inside a blocks field's rows.
    fn block_type(self, rest: &[&str]) -> Result<JsonLeaf> {
        if !rest.is_empty() {
            bail!("{BLOCK_TYPE_KEY} must be the last segment in a filter path");
        }

        let Some(extract_expr) = self.row_type.clone() else {
            bail!("{BLOCK_TYPE_KEY} names a block row's type; this path is not in a block row");
        };

        Ok(self.finish(extract_expr, Some(FieldType::Text), None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::query::filter::resolve::test_helpers::*;

    /// A reading's joins, extract expression, leaf type and list.
    type Parts = (
        Vec<(String, String)>,
        String,
        Option<FieldType>,
        Option<ListLeaf>,
    );

    /// The one reading a path has in every row — no block-type condition.
    fn single(leaves: Vec<JsonLeaf>) -> Parts {
        let [leaf] = <[JsonLeaf; 1]>::try_from(leaves).expect("exactly one reading");
        assert!(leaf.is_unconditional(), "unexpected block-type step");

        let joins = leaf
            .each_joins()
            .into_iter()
            .map(|(source, alias)| (source.to_string(), alias.to_string()))
            .collect();

        (joins, leaf.extract_expr, leaf.field_type, leaf.list)
    }

    fn walk_block_fields(
        conn: &dyn DbConnection,
        segments: &[&str],
        blocks: &[BlockDefinition],
        join_table: &str,
    ) -> Result<Parts> {
        JsonWalk::block_row(join_table, blocks)
            .walk(conn, segments)
            .map(single)
    }

    fn walk_array_column(
        conn: &dyn DbConnection,
        container: &FieldDefinition,
        segments: &[&str],
    ) -> Result<Parts> {
        let column = format!("\"posts_items\".\"{}\"", container.name);

        JsonWalk::array_column(conn, column, container)?
            .walk(conn, segments)
            .map(single)
    }

    /// One reading of a blocks path: block-type steps, extract, type, list.
    type Reading = (Vec<JsonStep>, String, Option<FieldType>, Option<ListLeaf>);

    /// Every reading of a blocks path.
    fn readings(
        conn: &dyn DbConnection,
        blocks: &[BlockDefinition],
        segments: &[&str],
    ) -> Vec<Reading> {
        JsonWalk::block_row("posts_content", blocks)
            .walk(conn, segments)
            .unwrap()
            .into_iter()
            .map(|leaf| (leaf.steps, leaf.extract_expr, leaf.field_type, leaf.list))
            .collect()
    }

    fn guard(expr: &str, block_type: &str) -> JsonStep {
        JsonStep::BlockType {
            expr: expr.to_string(),
            block_type: block_type.to_string(),
        }
    }

    fn other(expr: &str, declared: &[&str]) -> JsonStep {
        JsonStep::OtherBlockType {
            expr: expr.to_string(),
            declared: declared.iter().map(ToString::to_string).collect(),
        }
    }

    /// Two block types, `stat` and `note`, naming fields alike but defining
    /// them differently: a number vs text, a has-many list vs a single value,
    /// a scalar vs a group.
    fn mixed_blocks() -> Vec<BlockDefinition> {
        let tags_list = FieldDefinition::builder("tags", FieldType::Text)
            .has_many(true)
            .build();
        let mut info_group = make_field("info", FieldType::Group, false);
        info_group.fields = vec![make_field("x", FieldType::Text, false)];

        vec![
            make_block_def(
                "stat",
                vec![
                    make_field("score", FieldType::Number, false),
                    tags_list,
                    make_field("info", FieldType::Text, false),
                    make_field("title", FieldType::Text, false),
                ],
            ),
            make_block_def(
                "note",
                vec![
                    make_field("score", FieldType::Text, false),
                    make_field("tags", FieldType::Text, false),
                    info_group,
                    make_field("title", FieldType::Text, false),
                ],
            ),
        ]
    }

    /// Regression: the first declared block type's definition was taken for
    /// every row, so a number in one block type and text in another cast the
    /// text on Postgres (an error), and compared the other type's rows as the
    /// wrong type. Each block type now has its own reading, for its own rows.
    #[test]
    fn a_name_typed_differently_per_block_type_is_read_per_type() {
        let (_dir, conn) = test_conn();

        let found = readings(&conn, &mixed_blocks(), &["score"]);

        let column = "posts_content._block_type";
        assert_eq!(
            found,
            vec![
                (
                    vec![guard(column, "stat")],
                    "json_extract(posts_content.data, '$.score')".to_string(),
                    Some(FieldType::Number),
                    None
                ),
                (
                    vec![guard(column, "note")],
                    "json_extract(posts_content.data, '$.score')".to_string(),
                    Some(FieldType::Text),
                    None
                ),
                (
                    vec![other(column, &["note", "stat"])],
                    ABSENT_VALUE.to_string(),
                    None,
                    None
                ),
            ]
        );
    }

    /// Regression: a row of a block type declaring no field of the name
    /// matched nothing once the declaring types defined it differently — while
    /// with one shared definition it reads NULL. It reads NULL in both.
    #[test]
    fn rows_of_an_undeclaring_block_type_read_the_value_as_absent() {
        let (_dir, conn) = test_conn();
        let mut blocks = mixed_blocks();
        blocks.push(make_block_def(
            "blank",
            vec![make_field("title", FieldType::Text, false)],
        ));

        let found = readings(&conn, &blocks, &["score"]);
        let absent = found.last().expect("readings");

        assert_eq!(found.len(), 3);
        assert_eq!(
            absent.0,
            vec![other("posts_content._block_type", &["note", "stat"])]
        );
        assert_eq!(absent.1, ABSENT_VALUE);
        assert_eq!(absent.2, None);
    }

    /// Regression: a has-many list in one block type and a single value in
    /// another expanded the single value as a list ("malformed JSON").
    #[test]
    fn a_list_in_one_block_type_and_a_value_in_another_are_read_per_type() {
        let (_dir, conn) = test_conn();

        let lists: Vec<Option<ListLeaf>> = readings(&conn, &mixed_blocks(), &["tags"])
            .into_iter()
            .map(|(_, _, _, list)| list)
            .collect();

        assert_eq!(
            lists,
            vec![Some(ListLeaf::Scalar(FieldType::Text)), None, None]
        );
    }

    /// Regression: a group in one block type was unreachable when another
    /// block type named a scalar the same. A path valid for one block type is
    /// accepted, read only in that type's rows.
    #[test]
    fn a_path_valid_for_one_block_type_is_read_in_its_rows_only() {
        let (_dir, conn) = test_conn();
        let column = "posts_content._block_type";

        // The declaring readings that hold, then the absent one.
        let deep = readings(&conn, &mixed_blocks(), &["info", "x"]);
        assert_eq!(deep.len(), 2);
        assert_eq!(deep[0].0, vec![guard(column, "note")]);
        assert_eq!(deep[0].1, "json_extract(posts_content.data, '$.info.x')");
        assert_eq!(deep[1].0, vec![other(column, &["note", "stat"])]);

        let scalar = readings(&conn, &mixed_blocks(), &["info"]);
        assert_eq!(scalar.len(), 2);
        assert_eq!(scalar[0].0, vec![guard(column, "stat")]);

        let err = JsonWalk::block_row("posts_content", &mixed_blocks())
            .walk(&conn, &["info", "x", "y"])
            .unwrap_err();
        assert!(err.to_string().contains("cannot have sub-paths"), "{err}");
    }

    /// A name every block type defines alike keeps one reading for all rows.
    #[test]
    fn a_name_defined_alike_keeps_one_reading() {
        let (_dir, conn) = test_conn();

        let (joins, expr, leaf, _) = JsonWalk::block_row("posts_content", &mixed_blocks())
            .walk(&conn, &["title"])
            .map(single)
            .unwrap();

        assert!(joins.is_empty());
        assert_eq!(expr, "json_extract(posts_content.data, '$.title')");
        assert_eq!(leaf, Some(FieldType::Text));
    }

    /// A nested block row's type is read from its own JSON, so a fork there
    /// conditions on the nested row.
    #[test]
    fn a_nested_block_row_forks_on_its_own_type() {
        let (_dir, conn) = test_conn();
        let mut nested = make_field("nested", FieldType::Blocks, false);
        nested.blocks = mixed_blocks();
        let blocks = vec![make_block_def("wrap", vec![nested])];

        let found = readings(&conn, &blocks, &["nested", "score"]);

        assert_eq!(found.len(), 3);
        assert_eq!(
            found[0].0,
            vec![
                JsonStep::Each {
                    source: "json_extract(posts_content.data, '$.nested')".to_string(),
                    alias: "j0".to_string(),
                },
                guard("json_extract(j0.value, '$._block_type')", "stat"),
            ]
        );
    }

    /// Regression: a join field inside a row was accepted as a filter leaf,
    /// though it stores nothing (top level refuses it). Refused at any depth.
    #[test]
    fn a_join_field_inside_a_row_is_refused() {
        let (_dir, conn) = test_conn();
        let posts = make_field("posts", FieldType::Join, false);
        let mut meta = make_field("meta", FieldType::Group, false);
        meta.fields = vec![make_field("posts", FieldType::Join, false)];
        let blocks = vec![make_block_def("card", vec![posts, meta.clone()])];

        for segments in [&["posts"][..], &["meta", "posts"][..]] {
            let err = walk_block_fields(&conn, segments, &blocks, "posts_content").unwrap_err();
            assert!(err.to_string().contains("stores no value"), "{err}");
        }

        let err = walk_array_column(&conn, &meta, &["posts"]).unwrap_err();
        assert!(err.to_string().contains("stores no value"), "{err}");
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
        assert_eq!(expr, "posts_content._block_type");
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
