//! `admin.default_sort` names a column the list view orders by on every
//! request; a name the table does not have would only surface as a backend
//! error on the first list load, so it is checked once at startup like every
//! other definition-level invariant. A `hidden` field is refused too: it is
//! never sortable for anyone, so the list would reject its own default.

use anyhow::{Result, bail};

use crate::{
    core::{CollectionDefinition, Registry},
    db::query::read::is_valid_sort_column,
    service::is_hidden_query_path,
};

/// Reject every collection whose `admin.default_sort` does not name a
/// sortable column of its own table (a leading `-` selects descending order),
/// or names a `hidden` field.
///
/// # Errors
///
/// Returns an error naming each offending collection and value.
pub fn validate_admin_default_sorts(registry: &Registry) -> Result<()> {
    let offenders: Vec<String> = registry
        .collections
        .iter()
        .filter_map(|(slug, def)| default_sort_error(slug, def))
        .collect();

    if offenders.is_empty() {
        return Ok(());
    }

    bail!(
        "admin.default_sort names a column the collection cannot order by: {}",
        offenders.join("; ")
    )
}

fn default_sort_error(slug: &str, def: &CollectionDefinition) -> Option<String> {
    let sort = def.admin.default_sort.as_deref()?;
    let column = sort.strip_prefix('-').unwrap_or(sort);

    if is_hidden_query_path(def, column) {
        return Some(format!("{slug}: `{sort}` (hidden field)"));
    }

    (!is_valid_sort_column(column, def)).then(|| format!("{slug}: `{sort}`"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{FieldDefinition, FieldType, VersionsConfig};

    fn registry_with(def: CollectionDefinition) -> Registry {
        let mut registry = Registry::new();
        registry.register_collection(def);
        registry
    }

    fn posts(default_sort: &str) -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];
        def.admin.default_sort = Some(default_sort.to_string());
        def
    }

    #[test]
    fn a_default_sort_on_a_real_column_passes() {
        assert!(validate_admin_default_sorts(&registry_with(posts("-title"))).is_ok());
        assert!(validate_admin_default_sorts(&registry_with(posts("created_at"))).is_ok());
    }

    /// A column that does not exist — a typo, or `_status` on a collection
    /// without drafts — used to reach the database on the first list load.
    #[test]
    fn a_default_sort_on_a_missing_column_fails_the_boot() {
        let err = validate_admin_default_sorts(&registry_with(posts("_status")))
            .expect_err("no drafts, no _status column");
        assert!(err.to_string().contains("posts: `_status`"), "{err}");

        let mut with_drafts = posts("_status");
        with_drafts.versions = Some(VersionsConfig::new(true, 0));
        assert!(validate_admin_default_sorts(&registry_with(with_drafts)).is_ok());

        assert!(validate_admin_default_sorts(&registry_with(posts("titel"))).is_err());
    }

    /// Regression: a default sort on a `hidden` field booted fine, then every
    /// list load was refused (hidden fields are never sortable) and rendered
    /// as "no permission to view this collection".
    #[test]
    fn a_default_sort_on_a_hidden_field_fails_the_boot() {
        let mut def = posts("-secret");
        def.fields.push(
            FieldDefinition::builder("secret", FieldType::Text)
                .hidden(true)
                .build(),
        );

        let err = validate_admin_default_sorts(&registry_with(def))
            .expect_err("a hidden field is never sortable");
        assert!(
            err.to_string().contains("posts: `-secret` (hidden field)"),
            "{err}"
        );
    }
}
