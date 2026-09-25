//! What one collection's search index is built from, and how each indexed
//! column's stored value becomes indexable text.

use std::collections::HashMap;

use crate::{
    config::LocaleConfig,
    core::{Builder, CollectionDefinition, Registry, richtext::SearchableAttrs},
    db::query::fts::{
        extract::extract_richtext_text,
        fields::{
            RichtextFormat, build_node_searchable_map, richtext_column_format, richtext_columns,
        },
    },
};

/// One collection's full-text search index: the table it mirrors, its
/// definition (which columns are indexed, which are rich text) and the locale
/// columns those expand to.
#[derive(Builder)]
pub struct FtsIndex<'a> {
    #[builder(required)]
    pub slug: &'a str,
    #[builder(required)]
    pub def: &'a CollectionDefinition,
    #[builder(required)]
    pub locale_config: &'a LocaleConfig,
    /// Resolves each rich text field's `admin.nodes` to the registered custom
    /// nodes' `searchable_attrs`. Without it, custom nodes contribute no text,
    /// so every write and rebuild path passes the registry it validated with.
    pub registry: Option<&'a Registry>,
}

/// How each indexed column's stored value becomes indexable text: rich text
/// columns (either format) are read as their plain text, including the
/// opted-in attrs of custom nodes; every other column as-is. The one reading
/// shared by the per-write upsert and the startup rebuild, so both index the
/// same words.
pub(super) struct ColumnText<'a> {
    richtext: HashMap<String, RichtextFormat>,
    searchable: SearchableAttrs<'a>,
}

impl<'a> ColumnText<'a> {
    pub(super) fn new(index: &FtsIndex<'a>) -> Self {
        Self {
            richtext: richtext_columns(index.def),
            searchable: build_node_searchable_map(index.def, index.registry),
        }
    }

    /// Whether any indexed value needs reading beyond its stored text.
    pub(super) fn has_richtext(&self) -> bool {
        !self.richtext.is_empty()
    }

    /// The indexable text of `column`'s stored value `raw`.
    pub(super) fn text(&self, column: &str, raw: &str) -> String {
        match richtext_column_format(column, &self.richtext) {
            Some(format) if !raw.is_empty() => {
                extract_richtext_text(raw, format, &self.searchable, column)
            }
            _ => raw.to_string(),
        }
    }
}
