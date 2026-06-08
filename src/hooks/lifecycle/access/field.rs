//! Field-level read/write access checks plus the recursive helpers used by
//! `WriteHooks::check_access` to enforce per-field denied lists.

use mlua::Lua;
use tracing::warn;

use crate::{
    core::{Document, FieldDefinition, FieldType},
    db::{AccessResult, query::helpers::prefixed_name},
    hooks::lifecycle::{AccessCheckInput, access::collection::check_access_with_lua},
};

pub(crate) fn check_field_read_access_with_lua(
    lua: &Lua,
    fields: &[FieldDefinition],
    user: Option<&Document>,
    locale: Option<&str>,
) -> Vec<String> {
    collect_field_access_denied(lua, fields, user, locale, extract_read_access, "", "read")
}

/// Check field-level write access using an already-held `&Lua` reference.
/// Returns a list of field names that should be stripped from the input.
/// Recurses into Group (with `__` prefix) and transparent layout containers (Row/Collapsible/Tabs).
pub(crate) fn check_field_write_access_with_lua(
    lua: &Lua,
    fields: &[FieldDefinition],
    user: Option<&Document>,
    locale: Option<&str>,
    operation: &str,
) -> Vec<String> {
    let extractor: fn(&FieldDefinition) -> Option<&str> = match operation {
        "create" => extract_create_access,
        "update" => extract_update_access,
        _ => return Vec::new(),
    };

    collect_field_access_denied(lua, fields, user, locale, extractor, "", "write")
}

fn extract_read_access(f: &FieldDefinition) -> Option<&str> {
    f.access.read.as_deref()
}

fn extract_create_access(f: &FieldDefinition) -> Option<&str> {
    f.access.create.as_deref()
}

fn extract_update_access(f: &FieldDefinition) -> Option<&str> {
    f.access.update.as_deref()
}

/// Check whether any field (including nested sub-fields of Groups and transparent
/// containers) has an access function for the given extractor.
///
/// Mirrors `collect_field_access_denied`'s traversal pattern:
/// - Group: recurse into sub-fields.
/// - Row/Collapsible/Tabs: recurse (transparent containers).
/// - Array/Blocks: skip (separate join tables, no column-level stripping).
pub(crate) fn has_any_field_access(
    fields: &[FieldDefinition],
    extractor: fn(&FieldDefinition) -> Option<&str>,
) -> bool {
    for field in fields {
        if extractor(field).is_some() {
            return true;
        }

        match field.field_type {
            FieldType::Group | FieldType::Row | FieldType::Collapsible
                if has_any_field_access(&field.fields, extractor) =>
            {
                return true;
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    if has_any_field_access(&tab.fields, extractor) {
                        return true;
                    }
                }
            }
            _ => {} // Array/Blocks — separate join tables
        }
    }

    false
}

/// Recursively collect field names denied by an access check function.
///
/// - Group fields recurse with `parent__` prefix (matching DB column names).
/// - Row/Collapsible/Tabs are transparent — recurse with the same prefix.
/// - Array/Blocks have separate join tables and don't need column-level stripping.
fn collect_field_access_denied(
    lua: &Lua,
    fields: &[FieldDefinition],
    user: Option<&Document>,
    locale: Option<&str>,
    extractor: fn(&FieldDefinition) -> Option<&str>,
    prefix: &str,
    operation: &str,
) -> Vec<String> {
    let mut denied = Vec::new();

    for field in fields {
        let full_name = prefixed_name(prefix, &field.name);

        if let Some(ref_str) = extractor(field) {
            let result = check_access_with_lua(
                lua,
                &AccessCheckInput {
                    access_ref: Some(ref_str),
                    user,
                    id: None,
                    data: None,
                    locale,
                    operation,
                    // Field-access functions are registered on a specific field of
                    // a specific collection, so they don't consult ctx.collection.
                    collection: "",
                    ui_locale: None,
                },
            );
            match result {
                Ok(AccessResult::Allowed | AccessResult::Constrained(_)) => {}
                Ok(AccessResult::Denied) => {
                    denied.push(full_name.clone());

                    continue; // Parent denied → skip sub-fields
                }
                Err(e) => {
                    warn!(
                        "Field access function '{}' error (treating as denied): {}",
                        ref_str, e
                    );

                    denied.push(full_name.clone());

                    continue;
                }
            }
        }

        // Recurse into containers with sub-fields
        match field.field_type {
            FieldType::Group => {
                denied.extend(collect_field_access_denied(
                    lua,
                    &field.fields,
                    user,
                    locale,
                    extractor,
                    &full_name,
                    operation,
                ));
            }
            FieldType::Row | FieldType::Collapsible => {
                denied.extend(collect_field_access_denied(
                    lua,
                    &field.fields,
                    user,
                    locale,
                    extractor,
                    prefix,
                    operation,
                ));
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    denied.extend(collect_field_access_denied(
                        lua,
                        &tab.fields,
                        user,
                        locale,
                        extractor,
                        prefix,
                        operation,
                    ));
                }
            }
            _ => {} // Array/Blocks don't need column-level stripping
        }
    }

    denied
}

#[cfg(test)]
mod tests {
    use super::super::test_helpers::*;
    use super::*;
    use crate::core::{FieldAccess, FieldType};
    #[test]
    fn field_read_no_access_config_allows_all() {
        let lua = setup_lua();
        let fields = vec![
            make_field("title", FieldAccess::default()),
            make_field("body", FieldAccess::default()),
        ];
        let denied = check_field_read_access_with_lua(&lua, &fields, None, None);
        assert!(denied.is_empty());
    }

    #[test]
    fn field_read_allowed_not_in_denied() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "title",
            FieldAccess {
                read: Some("test_access.allow".to_string()),
                ..Default::default()
            },
        )];
        let denied = check_field_read_access_with_lua(&lua, &fields, None, None);
        assert!(denied.is_empty());
    }

    #[test]
    fn field_read_denied_in_list() {
        let lua = setup_lua();
        let fields = vec![
            make_field(
                "secret",
                FieldAccess {
                    read: Some("test_access.deny".to_string()),
                    ..Default::default()
                },
            ),
            make_field("title", FieldAccess::default()),
        ];
        let denied = check_field_read_access_with_lua(&lua, &fields, None, None);
        assert_eq!(denied, vec!["secret"]);
    }

    #[test]
    fn field_read_constrained_counts_as_allowed() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "status",
            FieldAccess {
                read: Some("test_access.constrained_string".to_string()),
                ..Default::default()
            },
        )];
        let denied = check_field_read_access_with_lua(&lua, &fields, None, None);
        assert!(denied.is_empty());
    }

    #[test]
    fn field_read_error_counts_as_denied() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "broken",
            FieldAccess {
                read: Some("test_access.throw_error".to_string()),
                ..Default::default()
            },
        )];
        let denied = check_field_read_access_with_lua(&lua, &fields, None, None);
        assert_eq!(denied, vec!["broken"]);
    }

    #[test]
    fn field_read_mixed_access() {
        let lua = setup_lua();
        let fields = vec![
            make_field(
                "public",
                FieldAccess {
                    read: Some("test_access.allow".to_string()),
                    ..Default::default()
                },
            ),
            make_field(
                "secret",
                FieldAccess {
                    read: Some("test_access.deny".to_string()),
                    ..Default::default()
                },
            ),
            make_field("plain", FieldAccess::default()),
        ];
        let denied = check_field_read_access_with_lua(&lua, &fields, None, None);
        assert_eq!(denied, vec!["secret"]);
    }

    #[test]
    fn field_read_with_user_context() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "admin_only",
            FieldAccess {
                read: Some("test_access.check_user".to_string()),
                ..Default::default()
            },
        )];

        let admin = make_user_doc("admin");
        let denied = check_field_read_access_with_lua(&lua, &fields, Some(&admin), None);
        assert!(denied.is_empty());

        let viewer = make_user_doc("viewer");
        let denied = check_field_read_access_with_lua(&lua, &fields, Some(&viewer), None);
        assert_eq!(denied, vec!["admin_only"]);
    }

    // ── check_field_write_access_with_lua ───────────────────────────────

    #[test]
    fn field_write_no_access_config_allows_all() {
        let lua = setup_lua();
        let fields = vec![make_field("title", FieldAccess::default())];
        let denied = check_field_write_access_with_lua(&lua, &fields, None, None, "create");
        assert!(denied.is_empty());
        let denied = check_field_write_access_with_lua(&lua, &fields, None, None, "update");
        assert!(denied.is_empty());
    }

    #[test]
    fn field_write_create_denied() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "locked",
            FieldAccess {
                create: Some("test_access.deny".to_string()),
                ..Default::default()
            },
        )];
        let denied = check_field_write_access_with_lua(&lua, &fields, None, None, "create");
        assert_eq!(denied, vec!["locked"]);
    }

    #[test]
    fn field_write_update_denied() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "immutable",
            FieldAccess {
                update: Some("test_access.deny".to_string()),
                ..Default::default()
            },
        )];
        let denied = check_field_write_access_with_lua(&lua, &fields, None, None, "update");
        assert_eq!(denied, vec!["immutable"]);
    }

    #[test]
    fn field_write_create_allowed() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "title",
            FieldAccess {
                create: Some("test_access.allow".to_string()),
                ..Default::default()
            },
        )];
        let denied = check_field_write_access_with_lua(&lua, &fields, None, None, "create");
        assert!(denied.is_empty());
    }

    #[test]
    fn field_write_unknown_operation_allows() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "title",
            FieldAccess {
                create: Some("test_access.deny".to_string()),
                update: Some("test_access.deny".to_string()),
                ..Default::default()
            },
        )];
        // Unknown operation = None access_ref = allowed (no restriction)
        let denied = check_field_write_access_with_lua(&lua, &fields, None, None, "delete");
        assert!(denied.is_empty());
    }

    #[test]
    fn field_write_error_counts_as_denied() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "broken",
            FieldAccess {
                create: Some("test_access.throw_error".to_string()),
                ..Default::default()
            },
        )];
        let denied = check_field_write_access_with_lua(&lua, &fields, None, None, "create");
        assert_eq!(denied, vec!["broken"]);
    }

    #[test]
    fn field_write_create_vs_update_different_access() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "role",
            FieldAccess {
                create: Some("test_access.allow".to_string()),
                update: Some("test_access.deny".to_string()),
                ..Default::default()
            },
        )];
        let denied = check_field_write_access_with_lua(&lua, &fields, None, None, "create");
        assert!(denied.is_empty());

        let denied = check_field_write_access_with_lua(&lua, &fields, None, None, "update");
        assert_eq!(denied, vec!["role"]);
    }

    #[test]
    fn field_write_constrained_counts_as_allowed() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "status",
            FieldAccess {
                create: Some("test_access.constrained_string".to_string()),
                ..Default::default()
            },
        )];
        let denied = check_field_write_access_with_lua(&lua, &fields, None, None, "create");
        assert!(denied.is_empty());
    }

    #[test]
    fn field_write_with_user_context() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "admin_only",
            FieldAccess {
                update: Some("test_access.check_user".to_string()),
                ..Default::default()
            },
        )];
        let admin = make_user_doc("admin");
        let denied = check_field_write_access_with_lua(&lua, &fields, Some(&admin), None, "update");
        assert!(denied.is_empty());

        let viewer = make_user_doc("viewer");
        let denied =
            check_field_write_access_with_lua(&lua, &fields, Some(&viewer), None, "update");
        assert_eq!(denied, vec!["admin_only"]);
    }

    // ── recursive field access ────────────────────────────────────────

    #[test]
    fn field_read_recurses_into_group_with_prefix() {
        let lua = setup_lua();
        let fields = vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![make_field(
                    "title",
                    FieldAccess {
                        read: Some("test_access.deny".to_string()),
                        ..Default::default()
                    },
                )])
                .build(),
        ];
        let denied = check_field_read_access_with_lua(&lua, &fields, None, None);
        assert_eq!(denied, vec!["seo__title"]);
    }

    #[test]
    fn field_read_recurses_through_row_without_prefix() {
        let lua = setup_lua();
        let fields = vec![
            FieldDefinition::builder("layout", FieldType::Row)
                .fields(vec![make_field(
                    "secret",
                    FieldAccess {
                        read: Some("test_access.deny".to_string()),
                        ..Default::default()
                    },
                )])
                .build(),
        ];
        let denied = check_field_read_access_with_lua(&lua, &fields, None, None);
        assert_eq!(denied, vec!["secret"]);
    }

    #[test]
    fn field_write_recurses_into_group() {
        let lua = setup_lua();
        let fields = vec![
            FieldDefinition::builder("config", FieldType::Group)
                .fields(vec![make_field(
                    "debug",
                    FieldAccess {
                        create: Some("test_access.deny".to_string()),
                        ..Default::default()
                    },
                )])
                .build(),
        ];
        let denied = check_field_write_access_with_lua(&lua, &fields, None, None, "create");
        assert_eq!(denied, vec!["config__debug"]);
    }

    #[test]
    fn field_read_does_not_recurse_into_array() {
        let lua = setup_lua();
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![make_field(
                    "name",
                    FieldAccess {
                        read: Some("test_access.deny".to_string()),
                        ..Default::default()
                    },
                )])
                .build(),
        ];
        // Array sub-fields have separate join tables — no column-level stripping
        let denied = check_field_read_access_with_lua(&lua, &fields, None, None);
        assert!(denied.is_empty());
    }

    // ── has_any_field_access ─────────────────────────────────────────

    #[test]
    fn has_any_no_access_configured() {
        let fields = vec![
            make_field("title", FieldAccess::default()),
            make_field("body", FieldAccess::default()),
        ];
        assert!(!has_any_field_access(&fields, |f| f.access.read.as_deref()));
    }

    #[test]
    fn has_any_top_level_read() {
        let fields = vec![make_field(
            "secret",
            FieldAccess {
                read: Some("test_access.deny".to_string()),
                ..Default::default()
            },
        )];
        assert!(has_any_field_access(&fields, |f| f.access.read.as_deref()));
    }

    #[test]
    fn has_any_nested_in_group() {
        // Group "seo" has no access, but sub-field "canonical_url" does.
        let fields = vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![make_field(
                    "canonical_url",
                    FieldAccess {
                        read: Some("test_access.deny".to_string()),
                        ..Default::default()
                    },
                )])
                .build(),
        ];
        assert!(has_any_field_access(&fields, |f| f.access.read.as_deref()));
    }

    #[test]
    fn has_any_nested_in_row() {
        let fields = vec![
            FieldDefinition::builder("layout", FieldType::Row)
                .fields(vec![make_field(
                    "secret",
                    FieldAccess {
                        create: Some("test_access.deny".to_string()),
                        ..Default::default()
                    },
                )])
                .build(),
        ];
        assert!(has_any_field_access(&fields, |f| f
            .access
            .create
            .as_deref()));
    }

    #[test]
    fn has_any_skips_array_sub_fields() {
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![make_field(
                    "name",
                    FieldAccess {
                        read: Some("test_access.deny".to_string()),
                        ..Default::default()
                    },
                )])
                .build(),
        ];
        // Array sub-fields have separate join tables — not included
        assert!(!has_any_field_access(&fields, |f| f.access.read.as_deref()));
    }

    #[test]
    fn has_any_deeply_nested_group_in_row() {
        // Row > Group > sub-field with access
        let fields = vec![
            FieldDefinition::builder("row", FieldType::Row)
                .fields(vec![
                    FieldDefinition::builder("grp", FieldType::Group)
                        .fields(vec![make_field(
                            "deep",
                            FieldAccess {
                                update: Some("test_access.deny".to_string()),
                                ..Default::default()
                            },
                        )])
                        .build(),
                ])
                .build(),
        ];
        assert!(has_any_field_access(&fields, |f| f
            .access
            .update
            .as_deref()));
    }

    #[test]
    fn field_read_recurses_into_tabs_sub_fields() {
        let lua = setup_lua();
        let fields = vec![
            FieldDefinition::builder("layout", FieldType::Tabs)
                .tabs(vec![crate::core::FieldTab {
                    label: "Main".to_string(),
                    description: None,
                    fields: vec![make_field(
                        "secret",
                        FieldAccess {
                            read: Some("test_access.deny".to_string()),
                            ..Default::default()
                        },
                    )],
                }])
                .build(),
        ];
        let denied = check_field_read_access_with_lua(&lua, &fields, None, None);
        assert_eq!(denied, vec!["secret"]);
    }

    #[test]
    fn field_write_recurses_into_tabs_sub_fields() {
        let lua = setup_lua();
        let fields = vec![
            FieldDefinition::builder("layout", FieldType::Tabs)
                .tabs(vec![crate::core::FieldTab {
                    label: "Settings".to_string(),
                    description: None,
                    fields: vec![make_field(
                        "locked",
                        FieldAccess {
                            create: Some("test_access.deny".to_string()),
                            ..Default::default()
                        },
                    )],
                }])
                .build(),
        ];
        let denied = check_field_write_access_with_lua(&lua, &fields, None, None, "create");
        assert_eq!(denied, vec!["locked"]);
    }

    #[test]
    fn has_any_nested_in_tabs() {
        let fields = vec![
            FieldDefinition::builder("layout", FieldType::Tabs)
                .tabs(vec![crate::core::FieldTab {
                    label: "SEO".to_string(),
                    description: None,
                    fields: vec![make_field(
                        "meta_title",
                        FieldAccess {
                            read: Some("test_access.deny".to_string()),
                            ..Default::default()
                        },
                    )],
                }])
                .build(),
        ];
        assert!(has_any_field_access(&fields, |f| f.access.read.as_deref()));
    }

    #[test]
    fn has_any_write_checks_correct_extractor() {
        let fields = vec![make_field(
            "title",
            FieldAccess {
                create: Some("test_access.deny".to_string()),
                ..Default::default()
            },
        )];
        // Has create access, but checking update should return false
        assert!(!has_any_field_access(&fields, |f| f
            .access
            .update
            .as_deref()));
        assert!(has_any_field_access(&fields, |f| f
            .access
            .create
            .as_deref()));
    }
}
