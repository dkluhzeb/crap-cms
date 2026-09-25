//! Config-level defaults resolved into the loaded definitions at init.
//!
//! A field that leaves a setting to the config gets the config's value once,
//! when the registry is built, so every reader of the definition sees the
//! effective value instead of re-deriving it from the config.

use std::sync::Arc;

use crate::{
    config::CrapConfig,
    core::{DEFAULT_JOIN_LIMIT, FieldDefinition, SharedRegistry},
};

/// The config values a field inherits when it sets none itself.
struct FieldDefaults<'a> {
    /// `[admin] default_timezone` for a timezone-aware date, if configured.
    timezone: Option<&'a str>,
    /// The listed-documents limit of a join without its own `limit`, when the
    /// built-in default does not fit `[pagination] max_limit`.
    join_limit: Option<u32>,
}

impl<'a> FieldDefaults<'a> {
    fn new(timezone: Option<&'a str>, join_limit: Option<u32>) -> Self {
        Self {
            timezone,
            join_limit,
        }
    }

    /// Whether any default applies.
    fn is_empty(&self) -> bool {
        self.timezone.is_none() && self.join_limit.is_none()
    }
}

/// The limit a join without its own `limit` takes under `max_limit`: `None`
/// while [`DEFAULT_JOIN_LIMIT`] fits, else `max_limit` — so lowering
/// `[pagination] max_limit` below the default never breaks the boot, and no
/// join lists more than a list read may return.
fn join_limit_cap(max_limit: i64) -> Option<u32> {
    if max_limit >= i64::from(DEFAULT_JOIN_LIMIT) {
        return None;
    }

    Some(u32::try_from(max_limit.max(1)).unwrap_or(1))
}

/// Apply `defaults` to `field` itself.
fn apply_to_field(field: &mut FieldDefinition, defaults: &FieldDefaults<'_>) {
    if let Some(tz) = defaults.timezone
        && field.has_tz_companion()
        && field.default_timezone.is_none()
    {
        field.default_timezone = Some(tz.to_string());
    }

    if let Some(cap) = defaults.join_limit
        && let Some(join) = &mut field.join
        && join.limit.is_none()
    {
        join.limit = Some(cap);
    }
}

/// Apply `defaults` to every field in `fields`, at any depth — groups, rows,
/// tabs, arrays and blocks' sub-fields included.
fn apply_to_fields(fields: &mut [FieldDefinition], defaults: &FieldDefaults<'_>) {
    for field in fields.iter_mut() {
        apply_to_field(field, defaults);
        apply_to_fields(&mut field.fields, defaults);

        for tab in &mut field.tabs {
            apply_to_fields(&mut tab.fields, defaults);
        }

        // Blocks sub-fields live under `blocks[].fields`, not `fields`.
        for block in &mut field.blocks {
            apply_to_fields(&mut block.fields, defaults);
        }
    }
}

/// Resolve the config-level defaults into the loaded definitions: `[admin]
/// default_timezone` into date fields that set no timezone of their own, and
/// a `[pagination] max_limit` below the default join limit into joins that set
/// no `limit`.
pub(super) fn apply_config_defaults(registry: &SharedRegistry, config: &CrapConfig) {
    let timezone = Some(config.admin.default_timezone.as_str()).filter(|tz| !tz.is_empty());
    let defaults = FieldDefaults::new(timezone, join_limit_cap(config.pagination.max_limit));

    if defaults.is_empty() {
        return;
    }

    let Ok(mut reg) = registry.write() else {
        return;
    };

    // Init phase: the registry is being built and nothing else holds a
    // reference to these Arcs yet, so `make_mut` mutates in place without
    // cloning.
    for def in reg.collections.values_mut() {
        apply_to_fields(&mut Arc::make_mut(def).fields, &defaults);
    }
    for def in reg.globals.values_mut() {
        apply_to_fields(&mut Arc::make_mut(def).fields, &defaults);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{BlockDefinition, FieldType, JoinConfig};

    fn join(limit: Option<u32>) -> FieldDefinition {
        let mut config = JoinConfig::new("posts", "author");
        config.limit = limit;

        FieldDefinition::builder("posts", FieldType::Join)
            .join(config)
            .build()
    }

    #[test]
    fn default_timezone_applies_to_date_inside_blocks() {
        let mut fields = vec![
            FieldDefinition::builder("body", FieldType::Blocks)
                .blocks(vec![BlockDefinition::new(
                    "event",
                    vec![
                        FieldDefinition::builder("starts_at", FieldType::Date)
                            .timezone(true)
                            .build(),
                    ],
                )])
                .build(),
        ];

        apply_to_fields(
            &mut fields,
            &FieldDefaults::new(Some("America/New_York"), None),
        );

        let date = &fields[0].blocks[0].fields[0];
        assert_eq!(
            date.default_timezone.as_deref(),
            Some("America/New_York"),
            "a timezone Date nested in a Blocks field must inherit the config default"
        );
    }

    /// Regression: a `max_limit` below the default join limit failed the boot
    /// for every join without an explicit `limit`. Such a join now takes
    /// `max_limit`; an explicit limit is kept as written.
    #[test]
    fn a_small_max_limit_caps_joins_without_their_own_limit() {
        assert_eq!(join_limit_cap(5), Some(5));
        assert_eq!(join_limit_cap(i64::from(DEFAULT_JOIN_LIMIT)), None);
        assert_eq!(join_limit_cap(1000), None);

        let group = FieldDefinition::builder("meta", FieldType::Group)
            .fields(vec![join(None)])
            .build();
        let mut fields = vec![join(None), join(Some(3)), group];

        apply_to_fields(&mut fields, &FieldDefaults::new(None, Some(5)));

        let limit = |f: &FieldDefinition| f.join.as_ref().and_then(|j| j.limit);
        assert_eq!(limit(&fields[0]), Some(5));
        assert_eq!(limit(&fields[1]), Some(3), "an explicit limit is kept");
        assert_eq!(limit(&fields[2].fields[0]), Some(5), "at any depth");
    }
}
