//! Display labels for validation-error paths.
//!
//! A [`FieldError`](crate::core::validate::FieldError) is keyed by its data
//! path (`title`, `seo__title`, `items[0][label]`, `content[1][heading]`) and
//! names the field by its schema name. The admin shows translated messages, so
//! the field it names must read the way the form labels it — the (localized)
//! label, prefixed by the labels of the groups/arrays/blocks it sits in.

use std::collections::HashMap;

use crate::{
    admin::handlers::shared::field_label,
    core::{
        BLOCK_TYPE_KEY, BlockDefinition, FieldChildren, FieldDefinition, field_children,
        find_field, in_label_locale,
    },
};

/// Separator between the labels of a nested field's containers and its own.
const CHAIN_SEPARATOR: &str = " › ";

/// Which fields a lookup may resolve against at the current path position.
#[derive(Clone, Copy)]
enum Scope<'a> {
    /// A plain field list (top level, a group's or an array row's fields).
    Fields(&'a [FieldDefinition]),
    /// A blocks row whose block type is unknown: the first block type
    /// declaring the name wins.
    AnyBlock(&'a [BlockDefinition]),
    /// Directly after an array/blocks field name — only a row index may follow.
    Rows,
}

/// Split an error path into its head and bracketed segments
/// (`items[0][label]` → `items`, `["0", "label"]`). `None` for a path outside
/// that grammar (a rich-text node attribute `body[hero#2].url`, say).
fn split_path(path: &str) -> Option<(&str, Vec<&str>)> {
    let head_end = path.find('[').unwrap_or(path.len());
    let (head, mut rest) = path.split_at(head_end);

    let mut segments = Vec::new();
    while !rest.is_empty() {
        let inner = rest.strip_prefix('[')?;
        let close = inner.find(']')?;
        let segment = &inner[..close];

        if segment.is_empty() || segment.contains('#') {
            return None;
        }

        segments.push(segment);
        rest = &inner[close + 1..];
    }

    if head.is_empty() || head.contains('#') {
        return None;
    }

    Some((head, segments))
}

/// Find `name` in `scope`, descending transparent layout wrappers.
fn lookup<'a>(name: &str, scope: Scope<'a>) -> Option<&'a FieldDefinition> {
    match scope {
        Scope::Fields(fields) => find_field(name, fields),
        Scope::AnyBlock(blocks) => blocks.iter().find_map(|b| find_field(name, &b.fields)),
        Scope::Rows => None,
    }
}

/// The scope inside `field` right after its name: a group's own fields, or
/// [`Scope::Rows`] for an array/blocks field awaiting its row index. A layout
/// wrapper is never named in a path (lookups see through it) and a leaf has no
/// sub-fields, so neither resolves a further name.
fn scope_after_name(field: &FieldDefinition) -> Scope<'_> {
    match field_children(field) {
        FieldChildren::Array(_) | FieldChildren::Blocks(_) => Scope::Rows,
        FieldChildren::Group(fields) => Scope::Fields(fields),
        FieldChildren::Wrapper(_) | FieldChildren::Tabs(_) | FieldChildren::Leaf => {
            Scope::Fields(&[])
        }
    }
}

/// Where a path walk stands: the scope names resolve in, the field reached
/// last, and the path consumed so far (the form key prefix of the current row).
struct Walk<'a> {
    scope: Scope<'a>,
    current: Option<&'a FieldDefinition>,
    consumed: String,
    chain: Vec<&'a FieldDefinition>,
}

/// Resolves error paths to label chains against a definition's fields.
///
/// `form` is the submitted form map (flat bracket keys), used to learn a
/// blocks row's `_block_type` so its sub-fields resolve in the right block;
/// without it the first block type declaring the name is used.
pub struct ErrorLabels<'a> {
    fields: &'a [FieldDefinition],
    form: Option<&'a HashMap<String, String>>,
}

impl<'a> ErrorLabels<'a> {
    /// Resolve against `fields`, reading block types from `form` when given.
    pub fn new(fields: &'a [FieldDefinition], form: Option<&'a HashMap<String, String>>) -> Self {
        Self { fields, form }
    }

    /// The display label chain for the field an error `path` names, resolved
    /// in the admin UI `locale` (`SEO › Title`). `None` when the path does not
    /// name a schema field — the caller keeps the raw name then.
    pub fn label_chain(&self, path: &str, locale: &str) -> Option<String> {
        let chain = self.resolve(path)?;

        let labels: Vec<String> = in_label_locale(Some(locale.to_string()), || {
            chain.iter().copied().map(field_label).collect()
        });

        Some(labels.join(CHAIN_SEPARATOR))
    }

    /// Walk `path` through the schema, collecting every named field it passes.
    fn resolve(&self, path: &str) -> Option<Vec<&'a FieldDefinition>> {
        let (head, segments) = split_path(path)?;

        let mut walk = Walk {
            scope: Scope::Fields(self.fields),
            current: None,
            consumed: head.to_string(),
            chain: Vec::new(),
        };

        step_names(&mut walk, head)?;

        for segment in segments {
            walk.consumed.push('[');
            walk.consumed.push_str(segment);
            walk.consumed.push(']');

            if segment.bytes().all(|b| b.is_ascii_digit()) {
                self.step_row(&mut walk)?;
            } else {
                step_names(&mut walk, segment)?;
            }
        }

        (!walk.chain.is_empty()).then_some(walk.chain)
    }

    /// Enter a row of the field reached last. A group accepts the index a
    /// group-in-row path carries without changing scope.
    fn step_row(&self, walk: &mut Walk<'a>) -> Option<()> {
        let field = walk.current?;

        walk.scope = match field_children(field) {
            FieldChildren::Array(fields) | FieldChildren::Group(fields) => Scope::Fields(fields),
            FieldChildren::Blocks(_) => self.block_scope(field, &walk.consumed),
            FieldChildren::Wrapper(_) | FieldChildren::Tabs(_) | FieldChildren::Leaf => {
                return None;
            }
        };

        Some(())
    }

    /// The fields of the blocks row at `row_path`: its submitted block type's
    /// fields, or any block type's when the form does not say.
    fn block_scope(&self, field: &'a FieldDefinition, row_path: &str) -> Scope<'a> {
        let block_type = self
            .form
            .and_then(|form| form.get(&format!("{row_path}[{BLOCK_TYPE_KEY}]")));

        let Some(block) =
            block_type.and_then(|bt| field.blocks.iter().find(|b| b.block_type == *bt))
        else {
            return Scope::AnyBlock(&field.blocks);
        };

        Scope::Fields(&block.fields)
    }
}

/// Resolve a `__`-joined run of names (`seo__title`), one field per part.
fn step_names(walk: &mut Walk<'_>, names: &str) -> Option<()> {
    for name in names.split("__") {
        let field = lookup(name, walk.scope)?;

        walk.chain.push(field);
        walk.current = Some(field);
        walk.scope = scope_after_name(field);
    }

    Some(())
}

#[cfg(test)]
mod tests {
    use crate::core::{FieldAdmin, FieldType, LocalizedString};

    use super::*;

    fn labelled(name: &str, ft: FieldType, label: LocalizedString) -> FieldDefinition {
        FieldDefinition::builder(name, ft)
            .admin(FieldAdmin::builder().label(label).build())
            .build()
    }

    fn de_en(en: &str, de: &str) -> LocalizedString {
        LocalizedString::Localized(HashMap::from([
            ("en".to_string(), en.to_string()),
            ("de".to_string(), de.to_string()),
        ]))
    }

    fn schema() -> Vec<FieldDefinition> {
        let seo = FieldDefinition::builder("seo", FieldType::Group)
            .admin(
                FieldAdmin::builder()
                    .label(LocalizedString::Plain("SEO".into()))
                    .build(),
            )
            .fields(vec![labelled(
                "title",
                FieldType::Text,
                de_en("Title", "Titel"),
            )])
            .build();

        let items = FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("layout", FieldType::Row)
                    .fields(vec![
                        FieldDefinition::builder("label", FieldType::Text).build(),
                    ])
                    .build(),
            ])
            .build();

        let hero = BlockDefinition::new(
            "hero",
            vec![labelled(
                "heading",
                FieldType::Text,
                LocalizedString::Plain("Hero heading".into()),
            )],
        );
        let quote = BlockDefinition::new(
            "quote",
            vec![labelled(
                "heading",
                FieldType::Text,
                LocalizedString::Plain("Quote heading".into()),
            )],
        );
        let content = FieldDefinition::builder("content", FieldType::Blocks)
            .blocks(vec![hero, quote])
            .build();

        vec![
            labelled("title", FieldType::Text, de_en("Title", "Titel")),
            seo,
            items,
            content,
        ]
    }

    #[test]
    fn top_level_field_resolves_to_its_label_in_the_ui_locale() {
        let fields = schema();
        let labels = ErrorLabels::new(&fields, None);

        assert_eq!(labels.label_chain("title", "de").as_deref(), Some("Titel"));
        assert_eq!(labels.label_chain("title", "en").as_deref(), Some("Title"));
    }

    #[test]
    fn grouped_field_resolves_to_the_group_label_chain() {
        let fields = schema();
        let labels = ErrorLabels::new(&fields, None);

        assert_eq!(
            labels.label_chain("seo__title", "de").as_deref(),
            Some("SEO › Titel")
        );
    }

    #[test]
    fn array_row_sub_field_resolves_through_layout_wrappers() {
        let fields = schema();
        let labels = ErrorLabels::new(&fields, None);

        assert_eq!(
            labels.label_chain("items[2][label]", "en").as_deref(),
            Some("Items › Label")
        );
        assert_eq!(
            labels.label_chain("items[2]", "en").as_deref(),
            Some("Items")
        );
    }

    #[test]
    fn blocks_row_sub_field_uses_the_submitted_block_type() {
        let fields = schema();
        let form = HashMap::from([("content[0][_block_type]".to_string(), "quote".to_string())]);
        let labels = ErrorLabels::new(&fields, Some(&form));

        assert_eq!(
            labels.label_chain("content[0][heading]", "en").as_deref(),
            Some("Content › Quote heading")
        );
    }

    #[test]
    fn blocks_row_without_a_known_type_uses_the_first_declaring_block() {
        let fields = schema();
        let labels = ErrorLabels::new(&fields, None);

        assert_eq!(
            labels.label_chain("content[0][heading]", "en").as_deref(),
            Some("Content › Hero heading")
        );
    }

    #[test]
    fn paths_outside_the_schema_do_not_resolve() {
        let fields = schema();
        let labels = ErrorLabels::new(&fields, None);

        assert_eq!(labels.label_chain("missing", "en"), None);
        assert_eq!(labels.label_chain("title__de", "en"), None);
        assert_eq!(labels.label_chain("body[hero#2].url", "en"), None);
        assert_eq!(labels.label_chain("title[0]", "en"), None);
        assert_eq!(labels.label_chain("_form", "en"), None);
    }
}
