//! A join lists at most its `limit` documents per document; no limit may
//! exceed `[pagination] max_limit`, the most any list read returns.

use anyhow::{Result, bail};

use crate::core::{FieldDefinition, Registry, walk_all_fields};

use super::hook_refs::field_source_label;

/// Reject every join field whose explicit `limit` exceeds `max_limit`. A join
/// without one lists the default number of documents, capped at `max_limit`
/// when the config defaults are applied — so lowering `max_limit` never fails
/// the boot on its own.
///
/// # Errors
///
/// Returns an aggregated error naming each offending collection and field.
pub fn validate_join_limits(registry: &Registry, max_limit: i64) -> Result<()> {
    let mut offenders = Vec::new();

    for (slug, def) in &registry.collections {
        let source = format!("collection '{slug}'");

        collect_over_limit(&source, &def.fields, max_limit, &mut offenders);
    }

    if offenders.is_empty() {
        return Ok(());
    }

    offenders.sort();

    bail!(
        "Join field lists more documents than [pagination] max_limit ({max_limit}) allows — \
         set a smaller `limit`:\n  - {}",
        offenders.join("\n  - ")
    )
}

/// Record every join in `fields` (at any depth) whose explicit `limit` exceeds
/// `max_limit`. A join without one takes the default, which the config
/// defaults already brought within `max_limit`.
fn collect_over_limit(
    source: &str,
    fields: &[FieldDefinition],
    max_limit: i64,
    out: &mut Vec<String>,
) {
    walk_all_fields(fields, &mut Vec::new(), &mut |field, path| {
        let Some(limit) = field.join.as_ref().and_then(|join| join.limit) else {
            return;
        };

        if i64::from(limit) > max_limit {
            out.push(format!(
                "{}: limit {limit}",
                field_source_label(source, path, field)
            ));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{CollectionDefinition, FieldType, JoinConfig};

    fn registry_with_join(limit: Option<u32>) -> Registry {
        let mut join = JoinConfig::new("posts", "author");
        join.limit = limit;

        let mut users = CollectionDefinition::new("users");
        users.fields = vec![
            FieldDefinition::builder("posts", FieldType::Join)
                .join(join)
                .build(),
        ];

        let mut registry = Registry::new();
        registry.register_collection(users);
        registry
    }

    #[test]
    fn a_limit_within_max_limit_passes() {
        validate_join_limits(&registry_with_join(Some(50)), 1000).expect("within bounds");
        validate_join_limits(&registry_with_join(None), 1000).expect("the default");
    }

    /// A join may not list more documents than any list read returns.
    #[test]
    fn a_limit_over_max_limit_fails_naming_the_field() {
        let err = validate_join_limits(&registry_with_join(Some(5000)), 1000)
            .unwrap_err()
            .to_string();

        assert!(
            err.contains("users") && err.contains("posts") && err.contains("5000"),
            "{err}"
        );
    }

    /// Regression: a `max_limit` below the default join limit failed the boot
    /// for every join without an explicit `limit`. Only an explicit limit
    /// over `max_limit` fails; the default is capped by the config defaults.
    #[test]
    fn only_an_explicit_limit_is_checked_against_a_small_max_limit() {
        validate_join_limits(&registry_with_join(None), 5).expect("the default is capped");
        validate_join_limits(&registry_with_join(Some(5)), 5).expect("explicit limit");
        assert!(validate_join_limits(&registry_with_join(Some(6)), 5).is_err());
    }
}
