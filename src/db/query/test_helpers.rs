//! Shared test helpers for `db::query` module tests.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::Result;

use crate::core::{FieldDefinition, FieldTab, FieldType, collection::CollectionDefinition};
use crate::db::{DbConnection, DbRow, DbValue};

pub(crate) fn make_field(name: &str, field_type: FieldType) -> FieldDefinition {
    FieldDefinition::builder(name, field_type).build()
}

pub(crate) fn make_localized_field(name: &str, field_type: FieldType) -> FieldDefinition {
    FieldDefinition::builder(name, field_type)
        .localized(true)
        .build()
}

pub(crate) fn make_group_field(name: &str, sub_fields: Vec<FieldDefinition>) -> FieldDefinition {
    FieldDefinition::builder(name, FieldType::Group)
        .fields(sub_fields)
        .build()
}

pub(crate) fn make_collection_def(
    slug: &str,
    fields: Vec<FieldDefinition>,
    timestamps: bool,
) -> CollectionDefinition {
    let mut def = CollectionDefinition::new(slug);
    def.fields = fields;
    def.timestamps = timestamps;
    def
}

pub(crate) fn make_locale_config() -> crate::config::LocaleConfig {
    crate::config::LocaleConfig {
        default_locale: "en".to_string(),
        locales: vec!["en".to_string(), "de".to_string()],
        fallback: true,
    }
}

pub(crate) fn make_row_field(name: &str, sub_fields: Vec<FieldDefinition>) -> FieldDefinition {
    FieldDefinition::builder(name, FieldType::Row)
        .fields(sub_fields)
        .build()
}

pub(crate) fn make_collapsible_field(
    name: &str,
    sub_fields: Vec<FieldDefinition>,
) -> FieldDefinition {
    FieldDefinition::builder(name, FieldType::Collapsible)
        .fields(sub_fields)
        .build()
}

pub(crate) fn make_tabs_field(name: &str, tabs: Vec<FieldTab>) -> FieldDefinition {
    FieldDefinition::builder(name, FieldType::Tabs)
        .tabs(tabs)
        .build()
}

/// Delegating [`DbConnection`] wrapper that counts read queries — the proof
/// harness for batching regressions: assert the query count stays constant
/// as the document count grows.
pub(crate) struct CountingConn<'a> {
    inner: &'a dyn DbConnection,
    pub(crate) reads: Cell<usize>,
}

impl<'a> CountingConn<'a> {
    pub(crate) fn new(inner: &'a dyn DbConnection) -> Self {
        Self {
            inner,
            reads: Cell::new(0),
        }
    }

    pub(crate) fn reads(&self) -> usize {
        self.reads.get()
    }
}

impl DbConnection for CountingConn<'_> {
    fn execute(&self, sql: &str, params: &[DbValue]) -> Result<usize> {
        self.inner.execute(sql, params)
    }

    fn execute_batch(&self, sql: &str) -> Result<()> {
        self.inner.execute_batch(sql)
    }

    fn query_all(&self, sql: &str, params: &[DbValue]) -> Result<Vec<DbRow>> {
        self.reads.set(self.reads.get() + 1);
        self.inner.query_all(sql, params)
    }

    fn query_one(&self, sql: &str, params: &[DbValue]) -> Result<Option<DbRow>> {
        self.reads.set(self.reads.get() + 1);
        self.inner.query_one(sql, params)
    }

    fn placeholder(&self, n: usize) -> String {
        self.inner.placeholder(n)
    }

    fn now_expr(&self) -> &'static str {
        self.inner.now_expr()
    }

    fn greatest_expr(&self, a: &str, b: &str) -> String {
        self.inner.greatest_expr(a, b)
    }

    fn kind(&self) -> &'static str {
        self.inner.kind()
    }

    fn table_exists(&self, name: &str) -> Result<bool> {
        self.inner.table_exists(name)
    }

    fn get_table_columns(&self, table: &str) -> Result<HashSet<String>> {
        self.inner.get_table_columns(table)
    }

    fn get_table_column_types(&self, table: &str) -> Result<HashMap<String, String>> {
        self.inner.get_table_column_types(table)
    }

    fn index_names(&self, table: &str, prefix: &str) -> Result<Vec<String>> {
        self.inner.index_names(table, prefix)
    }

    fn timestamp_column_default(&self) -> &'static str {
        self.inner.timestamp_column_default()
    }

    fn timestamp_column_type(&self) -> &'static str {
        self.inner.timestamp_column_type()
    }

    fn column_type_for(&self, ft: &FieldType) -> &'static str {
        self.inner.column_type_for(ft)
    }

    fn date_offset_expr(&self, seconds: i64, param_pos: usize) -> (String, DbValue) {
        self.inner.date_offset_expr(seconds, param_pos)
    }

    fn json_extract_expr(&self, column: &str, field: &str) -> String {
        self.inner.json_extract_expr(column, field)
    }

    fn json_each_source(&self, source: &str, alias: &str) -> String {
        self.inner.json_each_source(source, alias)
    }

    fn build_insert_ignore(&self, table: &str, columns: &str, values: &str) -> String {
        self.inner.build_insert_ignore(table, columns, values)
    }

    fn build_upsert(&self, table: &str, columns: &[&str], values: &str, key_col: &str) -> String {
        self.inner.build_upsert(table, columns, values, key_col)
    }

    fn supports_fts(&self) -> bool {
        self.inner.supports_fts()
    }

    fn like_operator(&self) -> &'static str {
        self.inner.like_operator()
    }

    fn list_user_tables(&self) -> Result<Vec<String>> {
        self.inner.list_user_tables()
    }

    fn supports_drop_column(&self) -> bool {
        self.inner.supports_drop_column()
    }

    fn vacuum_into(&self, dest: &Path) -> Result<()> {
        self.inner.vacuum_into(dest)
    }

    fn sidecar_extensions(&self) -> &[&str] {
        self.inner.sidecar_extensions()
    }

    fn normalize_timestamp(&self, ts: &str) -> String {
        self.inner.normalize_timestamp(ts)
    }
}
