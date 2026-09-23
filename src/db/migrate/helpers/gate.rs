//! The `_crap_meta` value of a conversion gated on the shape it covers.
//!
//! A conversion that runs once per collection or global is gated on its own
//! version *and* on a fingerprint of what the pass covered, so a field added or
//! retyped after a pass — including one added to a collection whose table
//! already existed — runs it again. The fingerprint and the field paths it is
//! taken over live here, so no two conversions can disagree on when a shape
//! counts as changed.

use std::fmt::Write as _;

use sha2::{Digest, Sha256};

use crate::core::{BlockDefinition, FieldChildren, FieldDefinition, field_children};

/// Whether a leaf field takes part in a fingerprint.
pub(in crate::db::migrate) type KeepLeaf<'a> = &'a dyn Fn(&FieldDefinition) -> bool;

/// `{version}:{fingerprint}` of `parts` — the meta value of a gated pass. The
/// version leads, so bumping it re-runs every pass whatever its shape.
#[must_use]
pub(in crate::db::migrate) fn versioned_fingerprint(version: &str, parts: &[String]) -> String {
    let mut hasher = Sha256::new();

    for part in parts {
        hasher.update(format!("{part}\n").as_bytes());
    }

    let mut value = format!("{version}:");
    for byte in &hasher.finalize()[..8] {
        let _ = write!(value, "{byte:02x}");
    }

    value
}

/// The leaves of `fields` at any depth that `keep` accepts, named by path.
/// Layout wrappers and tabs are transparent, the way the storage names are.
#[must_use]
pub(in crate::db::migrate) fn field_paths(
    fields: &[FieldDefinition],
    keep: KeepLeaf<'_>,
) -> String {
    let mut paths = Vec::new();

    for field in fields {
        match field_children(field) {
            FieldChildren::Leaf if keep(field) => paths.push(leaf_path(field)),
            FieldChildren::Leaf => {}
            FieldChildren::Group(sub) | FieldChildren::Array(sub) => {
                paths.push(format!("{}({})", field.name, field_paths(sub, keep)));
            }
            FieldChildren::Wrapper(sub) => paths.push(field_paths(sub, keep)),
            FieldChildren::Tabs(tabs) => {
                paths.extend(tabs.iter().map(|tab| field_paths(&tab.fields, keep)));
            }
            FieldChildren::Blocks(defs) => {
                paths.push(format!("{}[{}]", field.name, block_paths(defs, keep)));
            }
        }
    }

    paths.join(",")
}

/// Whether a leaf `keep` accepts sits anywhere in `fields`, at any depth.
#[must_use]
pub(in crate::db::migrate) fn holds_leaf(fields: &[FieldDefinition], keep: KeepLeaf<'_>) -> bool {
    fields.iter().any(|field| match field_children(field) {
        FieldChildren::Group(sub) | FieldChildren::Wrapper(sub) | FieldChildren::Array(sub) => {
            holds_leaf(sub, keep)
        }
        FieldChildren::Tabs(tabs) => tabs.iter().any(|tab| holds_leaf(&tab.fields, keep)),
        FieldChildren::Blocks(defs) => defs.iter().any(|d| holds_leaf(&d.fields, keep)),
        FieldChildren::Leaf => keep(field),
    })
}

/// The leaves of every block definition, each named by its block type.
#[must_use]
pub(in crate::db::migrate) fn block_paths(defs: &[BlockDefinition], keep: KeepLeaf<'_>) -> String {
    defs.iter()
        .map(|d| format!("{}:{}", d.block_type, field_paths(&d.fields, keep)))
        .collect::<Vec<_>>()
        .join(";")
}

/// A leaf's name and the shape of the value it stores: its type, `[]` when it
/// holds a list, `@tz` when a zone is stored beside it. Each of the three
/// decides how a conversion rewrites the value, so each has to change the
/// fingerprint.
fn leaf_path(field: &FieldDefinition) -> String {
    let mut path = format!("{}:{}", field.name, field.field_type.as_str());

    if field.has_many {
        path.push_str("[]");
    }

    if field.has_tz_companion() {
        path.push_str("@tz");
    }

    path
}

#[cfg(test)]
mod tests {
    use std::slice;

    use super::*;
    use crate::core::{FieldTab, FieldType};

    fn field(name: &str, field_type: FieldType) -> FieldDefinition {
        FieldDefinition::builder(name, field_type).build()
    }

    fn keep_all(_: &FieldDefinition) -> bool {
        true
    }

    fn text_only(field: &FieldDefinition) -> bool {
        field.field_type == FieldType::Text
    }

    /// The version leads the value, and two different shapes fingerprint
    /// differently while the same shape fingerprints the same.
    #[test]
    fn the_fingerprint_follows_the_shape() {
        let one = versioned_fingerprint("1", &["posts.title=text".to_string()]);
        let again = versioned_fingerprint("1", &["posts.title=text".to_string()]);
        let other = versioned_fingerprint("1", &["posts.title=number".to_string()]);
        let bumped = versioned_fingerprint("2", &["posts.title=text".to_string()]);

        assert!(one.starts_with("1:"), "{one}");
        assert_eq!(one, again);
        assert_ne!(one, other);
        assert_ne!(one, bumped);
    }

    /// A leaf carries its type, its list-ness and its zone companion, so
    /// retyping a field, turning it into a list or giving it a zone each
    /// change the paths.
    #[test]
    fn a_leaf_path_carries_the_stored_shape() {
        let plain = field("tags", FieldType::Select);
        let list = FieldDefinition::builder("tags", FieldType::Select)
            .has_many(true)
            .build();
        let zoned = FieldDefinition::builder("starts", FieldType::Date)
            .timezone(true)
            .build();

        assert_eq!(field_paths(&[plain], &keep_all), "tags:select");
        assert_eq!(field_paths(&[list], &keep_all), "tags:select[]");
        assert_eq!(field_paths(&[zoned], &keep_all), "starts:date@tz");
    }

    /// Groups and arrays name their children, blocks name them per block type,
    /// and a layout wrapper or tab adds no name of its own.
    #[test]
    fn nesting_is_named_the_way_storage_is() {
        let group = FieldDefinition::builder("meta", FieldType::Group)
            .fields(vec![field("note", FieldType::Text)])
            .build();
        let wrapper = FieldDefinition::builder("row", FieldType::Row)
            .fields(vec![field("x", FieldType::Text)])
            .build();
        let tabs = FieldDefinition::builder("layout", FieldType::Tabs)
            .tabs(vec![FieldTab::new(
                "One",
                vec![field("y", FieldType::Text)],
            )])
            .build();
        let blocks = FieldDefinition::builder("content", FieldType::Blocks)
            .blocks(vec![BlockDefinition::new(
                "quote",
                vec![field("body", FieldType::Text)],
            )])
            .build();

        assert_eq!(field_paths(&[group], &keep_all), "meta(note:text)");
        assert_eq!(field_paths(&[wrapper], &keep_all), "x:text");
        assert_eq!(field_paths(&[tabs], &keep_all), "y:text");
        assert_eq!(
            field_paths(&[blocks], &keep_all),
            "content[quote:body:text]"
        );
    }

    /// A leaf is found at any depth — through groups, layout wrappers, tabs,
    /// arrays and blocks — and only when the predicate accepts it.
    #[test]
    fn holds_leaf_searches_every_depth() {
        let blocks = FieldDefinition::builder("content", FieldType::Blocks)
            .blocks(vec![BlockDefinition::new(
                "quote",
                vec![field("body", FieldType::Text)],
            )])
            .build();
        let group = FieldDefinition::builder("meta", FieldType::Group)
            .fields(vec![blocks])
            .build();

        assert!(holds_leaf(slice::from_ref(&group), &text_only));
        assert!(!holds_leaf(&[group], &|f: &FieldDefinition| f.field_type
            == FieldType::Number));
    }

    /// A leaf the predicate turns down is left out, so a pass that covers only
    /// some types isn't re-run by a change to the others.
    #[test]
    fn a_turned_down_leaf_is_left_out() {
        let fields = vec![
            field("title", FieldType::Text),
            field("n", FieldType::Number),
        ];

        assert_eq!(field_paths(&fields, &text_only), "title:text");
    }
}
