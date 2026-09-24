//! Find the join-table field a filter path starts at — an array, blocks or
//! has-many relationship/upload, at the top level or inside groups.
//!
//! A field nested in a group keeps its rows in the join table named after its
//! groups and itself joined by `__` (`{collection}_{group}__{field}`). A path
//! reaches it through the groups spelled either way — `seo.items.name` or
//! `seo__items.name` — and continues below it after a `.`.

use crate::core::{FieldChildren, FieldDefinition, field_children, find_field};

/// The join-table field a filter path starts at, and the path below it.
#[derive(Debug)]
pub(in crate::db::query::filter) struct ContainerRoot<'p, 'f> {
    /// The field's groups and itself joined by `__` — its join table's suffix,
    /// and its key in a group-flattened document.
    pub(in crate::db::query::filter) name: String,
    /// The path up to and including the field, as written.
    pub(in crate::db::query::filter) written: &'p str,
    /// The array, blocks or has-many relationship/upload field.
    pub(in crate::db::query::filter) field: &'f FieldDefinition,
    /// The path below the field.
    pub(in crate::db::query::filter) rest: &'p str,
}

/// The join-table field `path` starts at, through any number of groups, with
/// a `.` after it — `None` when the path starts at anything else (a value
/// column, a group's value, an unknown name) or does not continue below the
/// field.
pub(in crate::db::query::filter) fn container_root<'p, 'f>(
    path: &'p str,
    fields: &'f [FieldDefinition],
) -> Option<ContainerRoot<'p, 'f>> {
    let mut current = fields;
    let mut names: Vec<&str> = Vec::new();
    let mut start = 0;

    loop {
        let remaining = path.get(start..)?;
        let (segment, separator) = next_segment(remaining);
        let field = find_field(segment, current)?;
        let end = start + segment.len();

        names.push(segment);

        if let FieldChildren::Group(sub) = field_children(field) {
            current = sub;
            start = end + separator?.len();
            continue;
        }

        if !holds_join_rows(field) || separator != Some(Separator::Dot) {
            return None;
        }

        return Some(ContainerRoot {
            name: names.join("__"),
            written: path.get(..end)?,
            field,
            rest: path.get(end + 1..)?,
        });
    }
}

/// Whether `field` keeps its values in a join table.
fn holds_join_rows(field: &FieldDefinition) -> bool {
    matches!(
        field_children(field),
        FieldChildren::Array(_) | FieldChildren::Blocks(_)
    ) || field.is_has_many_reference()
}

/// What separates a path segment from the next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Separator {
    Dot,
    Group,
}

impl Separator {
    fn len(self) -> usize {
        match self {
            Self::Dot => 1,
            Self::Group => 2,
        }
    }
}

/// The first segment of `path`, up to the first `.` or `__`, and the
/// separator after it (`None` at the end of the path).
fn next_segment(path: &str) -> (&str, Option<Separator>) {
    let dot = path.find('.').map(|at| (at, Separator::Dot));
    let group = path.find("__").map(|at| (at, Separator::Group));

    let first = [dot, group].into_iter().flatten().min_by_key(|(at, _)| *at);

    match first {
        Some((at, separator)) => (&path[..at], Some(separator)),
        None => (path, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{FieldType, RelationshipConfig};

    fn fields() -> Vec<FieldDefinition> {
        let items = FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("url", FieldType::Text).build(),
            ])
            .build();
        let tags = FieldDefinition::builder("tags", FieldType::Relationship)
            .relationship(RelationshipConfig::new("tags", true))
            .build();
        let inner = FieldDefinition::builder("inner", FieldType::Group)
            .fields(vec![items.clone()])
            .build();
        let seo = FieldDefinition::builder("seo", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("title", FieldType::Text).build(),
                items.clone(),
                tags,
                inner,
            ])
            .build();

        vec![items, seo]
    }

    fn root(path: &str) -> Option<(String, String, String)> {
        let fields = fields();

        container_root(path, &fields).map(|found| {
            (
                found.name,
                found.written.to_string(),
                found.rest.to_string(),
            )
        })
    }

    fn found(name: &str, written: &str, rest: &str) -> (String, String, String) {
        (name.to_string(), written.to_string(), rest.to_string())
    }

    /// A top-level container is found as before; one inside groups by either
    /// spelling of the group part, at any group depth.
    #[test]
    fn containers_are_found_through_groups_in_either_spelling() {
        assert_eq!(root("items.url"), Some(found("items", "items", "url")));
        assert_eq!(
            root("seo.items.url"),
            Some(found("seo__items", "seo.items", "url"))
        );
        assert_eq!(
            root("seo__items.url"),
            Some(found("seo__items", "seo__items", "url"))
        );
        assert_eq!(
            root("seo.tags.id"),
            Some(found("seo__tags", "seo.tags", "id"))
        );
        assert_eq!(
            root("seo.inner__items.url"),
            Some(found("seo__inner__items", "seo.inner__items", "url"))
        );
    }

    /// A value path, a group's value, an unknown name, a container without a
    /// `.` after it — none starts at a join-table field.
    #[test]
    fn other_paths_have_no_container_root() {
        for path in [
            "seo.title",
            "seo__title",
            "seo.nope.x",
            "nope.x",
            "seo.items",
            "items__url",
            "seo.items__url",
        ] {
            assert!(root(path).is_none(), "{path}");
        }
    }
}
