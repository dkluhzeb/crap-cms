//! Cross-language helpers shared by every generator backend in
//! `typegen/`. Includes naming conventions (`to_pascal_case`),
//! field-classification predicates (`is_optional`, `rel_has_many`),
//! registry traversal (`sorted_*_slugs`), recursive sub-type
//! collection (`SubTypeKind`, `SubTypeField`, `collect_sub_type_fields`),
//! and the [`w!`] macro every generator uses to emit lines without
//! repeating `.expect("write to String")`.

use crate::core::{FieldDefinition, FieldType, Registry, Slug};

/// `writeln!` to a `String` that infallibly succeeds — wraps the
/// boilerplate `.expect("write to String")` every per-language
/// generator otherwise duplicated 30+ times. Re-export with
/// `use super::helpers::w;` in each generator file.
///
/// The macro brings `std::fmt::Write` into a local block scope so
/// callers don't need `use std::fmt::Write;` of their own.
///
/// Two forms: `w!(out)` emits a blank newline; `w!(out, fmt, args...)`
/// emits a formatted line.
macro_rules! w {
    ($out:expr) => {{
        use ::std::fmt::Write as _;
        ::std::writeln!($out).expect("write to String")
    }};
    ($out:expr, $($arg:tt)*) => {{
        use ::std::fmt::Write as _;
        ::std::writeln!($out, $($arg)*).expect("write to String")
    }};
}
pub(super) use w;

/// `write!`-without-trailing-newline variant of [`w!`]. Use for
/// multi-line format-string emissions where the string literal
/// already carries its own newlines (e.g. doc-prose blocks that span
/// several `--- ...` lines). Suppresses the `clippy::format_push_string`
/// warning that fires on the equivalent `out.push_str(&format!(...))`
/// pattern.
macro_rules! wraw {
    ($out:expr, $($arg:tt)*) => {{
        use ::std::fmt::Write as _;
        ::std::write!($out, $($arg)*).expect("write to String")
    }};
}
pub(super) use wraw;

/// Convert a slug like "`site_settings`" to `PascalCase` "`SiteSettings`".
pub(crate) fn to_pascal_case(slug: &str) -> String {
    slug.split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(c) => {
                    let mut s = c.to_uppercase().to_string();
                    s.push_str(&chars.collect::<String>());
                    s
                }
                None => String::new(),
            }
        })
        .collect()
}

/// Whether a field should be treated as optional in generated types.
pub(super) fn is_optional(field: &FieldDefinition) -> bool {
    !field.required || field.field_type == FieldType::Checkbox
}

/// Whether a field's relationship config marks it as has-many.
/// Returns `false` when there is no `RelationshipConfig` (i.e. defaults to has-one).
pub(super) fn rel_has_many(field: &FieldDefinition) -> bool {
    field.relationship.as_ref().is_some_and(|rc| rc.has_many)
}

/// Whether a field is a single-valued (has-one) relationship or upload. Such a
/// field is optional on read even when `required`: population nulls it when the
/// target is soft-deleted or access-denied (a has-many drops the entry instead).
/// Shared by the client type generator and the proto-conversion generator so
/// their optionality can't drift.
pub(super) fn is_single_ref(field: &FieldDefinition) -> bool {
    field.field_type.is_reference() && !rel_has_many(field)
}

/// Get sorted collection slugs from the registry.
pub(super) fn sorted_collection_slugs(registry: &Registry) -> Vec<&Slug> {
    let mut slugs: Vec<&Slug> = registry.collections.keys().collect();
    slugs.sort();
    slugs
}

/// Get sorted global slugs from the registry.
pub(super) fn sorted_global_slugs(registry: &Registry) -> Vec<&Slug> {
    let mut slugs: Vec<&Slug> = registry.globals.keys().collect();
    slugs.sort();
    slugs
}

/// Distinguishes Array from Group for sub-type rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SubTypeKind {
    Array,
    Group,
}

/// A field that needs a named sub-type definition.
///
/// `parent_pascal` is the `PascalCase` path of the field's enclosing
/// owner — for top-level fields it's the collection or global slug
/// `PascalCase`d (e.g. `"Navigation"`); for fields nested inside another
/// Array or Group it's the parent's compound name (e.g.
/// `"NavigationMainNav"`). Concatenating `parent_pascal` with the
/// field's own `PascalCase`d name gives the sub-type class name. Layout
/// wrappers (Row/Collapsible/Tabs) are transparent and don't
/// contribute to the path.
pub(super) struct SubTypeField<'a> {
    pub field: &'a FieldDefinition,
    pub kind: SubTypeKind,
    pub parent_pascal: String,
}

/// Recursively collect Array and Group fields that need sub-type definitions.
/// Walks through Row, Collapsible, Tabs, and Group containers to find nested
/// fields at any depth. `parent_pascal` is the `PascalCase` path of the
/// caller's enclosing owner (collection/global slug or an outer
/// Array/Group's compound name). Each returned [`SubTypeField`] carries
/// the path of its own immediate parent, so declarations match the
/// references emitted by [`super::lua::field`].
pub(super) fn collect_sub_type_fields<'a>(
    fields: &'a [FieldDefinition],
    parent_pascal: &str,
) -> Vec<SubTypeField<'a>> {
    let mut result = Vec::new();
    for f in fields {
        if f.field_type == FieldType::Array && !f.fields.is_empty() {
            let inner_pascal = format!("{}{}", parent_pascal, to_pascal_case(&f.name));
            result.push(SubTypeField {
                field: f,
                kind: SubTypeKind::Array,
                parent_pascal: parent_pascal.to_string(),
            });
            result.extend(collect_sub_type_fields(&f.fields, &inner_pascal));
        } else if f.field_type == FieldType::Group && !f.fields.is_empty() {
            let inner_pascal = format!("{}{}", parent_pascal, to_pascal_case(&f.name));
            result.push(SubTypeField {
                field: f,
                kind: SubTypeKind::Group,
                parent_pascal: parent_pascal.to_string(),
            });
            result.extend(collect_sub_type_fields(&f.fields, &inner_pascal));
        } else if matches!(f.field_type, FieldType::Row | FieldType::Collapsible) {
            result.extend(collect_sub_type_fields(&f.fields, parent_pascal));
        } else if f.field_type == FieldType::Tabs {
            for tab in &f.tabs {
                result.extend(collect_sub_type_fields(&tab.fields, parent_pascal));
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{FieldDefinition, FieldTab, GlobalDefinition, RelationshipConfig};

    // ── to_pascal_case ─────────────────────────────────────────────────

    #[test]
    fn to_pascal_case_single_word() {
        assert_eq!(to_pascal_case("posts"), "Posts");
    }

    #[test]
    fn to_pascal_case_multi_word() {
        assert_eq!(to_pascal_case("site_settings"), "SiteSettings");
    }

    #[test]
    fn to_pascal_case_already_pascal() {
        assert_eq!(to_pascal_case("Posts"), "Posts");
    }

    #[test]
    fn to_pascal_case_empty() {
        assert_eq!(to_pascal_case(""), "");
    }

    #[test]
    fn to_pascal_case_three_words() {
        assert_eq!(to_pascal_case("my_cool_thing"), "MyCoolThing");
    }

    // ── rel_has_many ────────────────────────────────────────────────────

    #[test]
    fn rel_has_many_with_has_many_config() {
        let f = FieldDefinition::builder("", FieldType::Upload)
            .relationship(RelationshipConfig::new("media", true))
            .build();
        assert!(rel_has_many(&f));
    }

    #[test]
    fn rel_has_many_with_has_one_config() {
        let f = FieldDefinition::builder("", FieldType::Upload)
            .relationship(RelationshipConfig::new("media", false))
            .build();
        assert!(!rel_has_many(&f));
    }

    #[test]
    fn rel_has_many_with_no_config() {
        let f = FieldDefinition::builder("", FieldType::Upload).build();
        assert!(!rel_has_many(&f));
    }

    // ── is_optional ─────────────────────────────────────────────────────

    #[test]
    fn is_optional_required_field() {
        let f = FieldDefinition::builder("", FieldType::Text)
            .required(true)
            .build();
        assert!(!is_optional(&f));
    }

    #[test]
    fn is_optional_non_required_field() {
        let f = FieldDefinition::builder("", FieldType::Text)
            .required(false)
            .build();
        assert!(is_optional(&f));
    }

    #[test]
    fn is_optional_checkbox_always_optional() {
        let f = FieldDefinition::builder("", FieldType::Checkbox)
            .required(true)
            .build();
        assert!(is_optional(&f), "checkbox should always be optional");
    }

    // ── sorted_*_slugs ─────────────────────────────────────────────────

    fn make_collection(slug: &str) -> crate::core::CollectionDefinition {
        crate::core::CollectionDefinition::new(slug)
    }

    fn make_global(slug: &str) -> GlobalDefinition {
        GlobalDefinition::new(slug)
    }

    #[test]
    fn sorted_collection_slugs_sorted() {
        let mut registry = Registry::default();
        registry
            .collections
            .insert("zebra".into(), make_collection("zebra"));
        registry
            .collections
            .insert("alpha".into(), make_collection("alpha"));
        registry
            .collections
            .insert("middle".into(), make_collection("middle"));
        let slugs = sorted_collection_slugs(&registry);
        let expected: Vec<Slug> = vec!["alpha".into(), "middle".into(), "zebra".into()];
        assert_eq!(slugs, expected.iter().collect::<Vec<&Slug>>());
    }

    #[test]
    fn sorted_global_slugs_sorted() {
        let mut registry = Registry::default();
        registry
            .globals
            .insert("settings".into(), make_global("settings"));
        registry
            .globals
            .insert("about".into(), make_global("about"));
        let slugs = sorted_global_slugs(&registry);
        let expected: Vec<Slug> = vec!["about".into(), "settings".into()];
        assert_eq!(slugs, expected.iter().collect::<Vec<&Slug>>());
    }

    // ── collect_sub_type_fields ────────────────────────────────────────

    fn text_field(name: &str, required: bool) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text)
            .required(required)
            .build()
    }

    #[test]
    fn collect_sub_type_fields_top_level() {
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![text_field("label", true)])
                .build(),
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![text_field("title", true)])
                .build(),
            text_field("name", true),
        ];
        let result = collect_sub_type_fields(&fields, "Test");
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].field.name, "items");
        assert_eq!(result[0].kind, SubTypeKind::Array);
        assert_eq!(result[1].field.name, "seo");
        assert_eq!(result[1].kind, SubTypeKind::Group);
    }

    #[test]
    fn collect_sub_type_fields_nested_in_layout() {
        let fields = vec![
            FieldDefinition::builder("row_container", FieldType::Row)
                .fields(vec![
                    FieldDefinition::builder("row_items", FieldType::Array)
                        .fields(vec![text_field("val", true)])
                        .build(),
                ])
                .build(),
            FieldDefinition::builder("collapse", FieldType::Collapsible)
                .fields(vec![
                    FieldDefinition::builder("meta", FieldType::Group)
                        .fields(vec![text_field("key", true)])
                        .build(),
                ])
                .build(),
            FieldDefinition::builder("tabs", FieldType::Tabs)
                .tabs(vec![FieldTab::new(
                    "T",
                    vec![
                        FieldDefinition::builder("tab_items", FieldType::Array)
                            .fields(vec![text_field("x", true)])
                            .build(),
                    ],
                )])
                .build(),
        ];
        let result = collect_sub_type_fields(&fields, "Test");
        let names: Vec<&str> = result.iter().map(|s| s.field.name.as_str()).collect();
        assert!(names.contains(&"row_items"), "array inside Row: {names:?}");
        assert!(
            names.contains(&"meta"),
            "group inside Collapsible: {names:?}"
        );
        assert!(names.contains(&"tab_items"), "array inside Tabs: {names:?}");
    }

    #[test]
    fn collect_sub_type_fields_recursive() {
        let fields = vec![
            FieldDefinition::builder("outer", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("inner", FieldType::Array)
                        .fields(vec![text_field("x", true)])
                        .build(),
                ])
                .build(),
            FieldDefinition::builder("group", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("nested_arr", FieldType::Array)
                        .fields(vec![text_field("y", true)])
                        .build(),
                ])
                .build(),
        ];
        let result = collect_sub_type_fields(&fields, "Test");
        let names: Vec<&str> = result.iter().map(|s| s.field.name.as_str()).collect();
        assert_eq!(names, vec!["outer", "inner", "group", "nested_arr"]);

        // Regression: nested sub-types must carry the compound parent
        // path so declarations match the references emitted by the
        // per-language `field_to_*` mappers. Outer-level fields get the
        // initial `parent_pascal`; inner fields get `parent_pascal +
        // outer.PascalName`.
        let parents: Vec<&str> = result.iter().map(|s| s.parent_pascal.as_str()).collect();
        assert_eq!(parents, vec!["Test", "TestOuter", "Test", "TestGroup"]);
    }

    #[test]
    fn collect_sub_type_fields_skips_empty() {
        let fields = vec![
            FieldDefinition::builder("empty_arr", FieldType::Array).build(),
            FieldDefinition::builder("empty_group", FieldType::Group).build(),
        ];
        let result = collect_sub_type_fields(&fields, "Test");
        assert!(
            result.is_empty(),
            "empty Array/Group should not produce sub-types"
        );
    }
}
