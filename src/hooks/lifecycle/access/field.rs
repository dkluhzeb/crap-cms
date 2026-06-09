//! Field-level read/write access checks plus the recursive helpers that build
//! the per-field denied list — flat columns/groups and fields nested inside
//! array/blocks rows at any depth. `WriteHooks::check_access` and the read
//! post-processing apply the resulting [`FieldDenial`]s to strip denied fields.

use mlua::Lua;
use tracing::warn;

use crate::{
    core::{DenialSeg, Document, FieldDefinition, FieldDenial, FieldType, HookRef},
    db::{AccessResult, query::helpers::prefixed_name},
    hooks::lifecycle::{AccessCheckInput, access::collection::check_access_with_lua},
};

pub(crate) fn check_field_read_access_with_lua(
    lua: &Lua,
    fields: &[FieldDefinition],
    user: Option<&Document>,
    locale: Option<&str>,
) -> Vec<FieldDenial> {
    collect_field_access_denied(lua, fields, user, locale, extract_read_access, "read")
}

/// Check field-level write access using an already-held `&Lua` reference.
/// Returns the fields to strip from the input. Recurses into Group (with `__`
/// prefix), transparent layout containers, and array/blocks rows.
pub(crate) fn check_field_write_access_with_lua(
    lua: &Lua,
    fields: &[FieldDefinition],
    user: Option<&Document>,
    locale: Option<&str>,
    operation: &str,
) -> Vec<FieldDenial> {
    let extractor: fn(&FieldDefinition) -> Option<&HookRef> = match operation {
        "create" => extract_create_access,
        "update" => extract_update_access,
        _ => return Vec::new(),
    };

    collect_field_access_denied(lua, fields, user, locale, extractor, operation)
}

fn extract_read_access(f: &FieldDefinition) -> Option<&HookRef> {
    f.access.read.as_ref()
}

fn extract_create_access(f: &FieldDefinition) -> Option<&HookRef> {
    f.access.create.as_ref()
}

fn extract_update_access(f: &FieldDefinition) -> Option<&HookRef> {
    f.access.update.as_ref()
}

/// Evaluate one field's access function. `true` = denied (or errored, which is
/// fail-closed to denied).
fn access_denied(
    lua: &Lua,
    hook: &HookRef,
    user: Option<&Document>,
    locale: Option<&str>,
    operation: &str,
) -> bool {
    match check_access_with_lua(
        lua,
        &AccessCheckInput {
            access: Some(hook),
            user,
            id: None,
            data: None,
            locale,
            operation,
            // Field-access functions are registered on a specific field of a
            // specific collection, so they don't consult ctx.collection.
            collection: "",
            ui_locale: None,
        },
    ) {
        Ok(AccessResult::Allowed | AccessResult::Constrained(_)) => false,
        Ok(AccessResult::Denied) => true,
        Err(e) => {
            warn!(
                "Field access function '{}' error (treating as denied): {}",
                hook.reference(),
                e
            );

            true
        }
    }
}

/// Collect denials by evaluating each field's access function via Lua.
fn collect_field_access_denied(
    lua: &Lua,
    fields: &[FieldDefinition],
    user: Option<&Document>,
    locale: Option<&str>,
    extractor: fn(&FieldDefinition) -> Option<&HookRef>,
    operation: &str,
) -> Vec<FieldDenial> {
    let is_denied = |field: &FieldDefinition| {
        extractor(field).is_some_and(|hook| access_denied(lua, hook, user, locale, operation))
    };

    let mut denied = Vec::new();
    collect_denials_flat(fields, &is_denied, "", &mut denied);

    denied
}

/// Check whether any field — at any depth, including inside array/blocks rows —
/// has an access function for the given extractor.
pub(crate) fn has_any_field_access(
    fields: &[FieldDefinition],
    extractor: fn(&FieldDefinition) -> Option<&HookRef>,
) -> bool {
    fields.iter().any(|field| {
        extractor(field).is_some()
            || match field.field_type {
                FieldType::Group | FieldType::Row | FieldType::Collapsible | FieldType::Array => {
                    has_any_field_access(&field.fields, extractor)
                }
                FieldType::Tabs => field
                    .tabs
                    .iter()
                    .any(|tab| has_any_field_access(&tab.fields, extractor)),
                FieldType::Blocks => field
                    .blocks
                    .iter()
                    .any(|block| has_any_field_access(&block.fields, extractor)),
                _ => false,
            }
    })
}

/// Shared denial-tree walker at the document level (flat columns).
///
/// Parameterized by `is_denied` so the Lua-evaluated path and the fail-closed
/// deny-all path (`runner::access`) share the identical container-recursion
/// rules — there is exactly one place that encodes how access descends the
/// field tree.
///
/// - Group recurses with a `parent__` column prefix.
/// - Row/Collapsible/Tabs are transparent (same prefix).
/// - Array/Blocks switch to [`collect_denials_nested`]: their data is JSON, so
///   denials become per-row paths keyed by the array's data key.
pub(crate) fn collect_denials_flat<F: Fn(&FieldDefinition) -> bool>(
    fields: &[FieldDefinition],
    is_denied: &F,
    prefix: &str,
    out: &mut Vec<FieldDenial>,
) {
    for field in fields {
        let full_name = prefixed_name(prefix, &field.name);

        if is_denied(field) {
            out.push(FieldDenial::Flat(full_name));

            continue; // Parent denied → its sub-fields go with it.
        }

        match field.field_type {
            FieldType::Group => collect_denials_flat(&field.fields, is_denied, &full_name, out),
            FieldType::Row | FieldType::Collapsible => {
                collect_denials_flat(&field.fields, is_denied, prefix, out);
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    collect_denials_flat(&tab.fields, is_denied, prefix, out);
                }
            }
            FieldType::Array => {
                collect_denials_nested(&field.fields, is_denied, &full_name, None, &[], out);
            }
            FieldType::Blocks => {
                for block in &field.blocks {
                    collect_denials_nested(
                        &block.fields,
                        is_denied,
                        &full_name,
                        Some(&block.block_type),
                        &[],
                        out,
                    );
                }
            }
            _ => {}
        }
    }
}

/// Shared denial-tree walker inside array/blocks rows. Emits [`FieldDenial::Nested`]
/// keyed by the top-level `array_key` and the within-row `row_path` (groups and
/// nested arrays), so the strip can reach every row at any depth.
pub(crate) fn collect_denials_nested<F: Fn(&FieldDefinition) -> bool>(
    fields: &[FieldDefinition],
    is_denied: &F,
    array_key: &str,
    array_block_type: Option<&str>,
    row_path: &[DenialSeg],
    out: &mut Vec<FieldDenial>,
) {
    for field in fields {
        if is_denied(field) {
            out.push(FieldDenial::Nested {
                array_key: array_key.to_string(),
                array_block_type: array_block_type.map(str::to_string),
                row_path: row_path.to_vec(),
                leaf: field.name.clone(),
            });

            continue;
        }

        match field.field_type {
            FieldType::Group => {
                let mut path = row_path.to_vec();
                path.push(DenialSeg::Group(field.name.clone()));
                collect_denials_nested(
                    &field.fields,
                    is_denied,
                    array_key,
                    array_block_type,
                    &path,
                    out,
                );
            }
            FieldType::Row | FieldType::Collapsible => {
                collect_denials_nested(
                    &field.fields,
                    is_denied,
                    array_key,
                    array_block_type,
                    row_path,
                    out,
                );
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    collect_denials_nested(
                        &tab.fields,
                        is_denied,
                        array_key,
                        array_block_type,
                        row_path,
                        out,
                    );
                }
            }
            FieldType::Array => {
                let mut path = row_path.to_vec();
                path.push(DenialSeg::Rows {
                    key: field.name.clone(),
                    block_type: None,
                });
                collect_denials_nested(
                    &field.fields,
                    is_denied,
                    array_key,
                    array_block_type,
                    &path,
                    out,
                );
            }
            FieldType::Blocks => {
                // One `Rows` step per block type so the strip only touches rows
                // of that type — a denial in one block must not strip a
                // same-named field from a sibling block type.
                for block in &field.blocks {
                    let mut path = row_path.to_vec();
                    path.push(DenialSeg::Rows {
                        key: field.name.clone(),
                        block_type: Some(block.block_type.clone()),
                    });
                    collect_denials_nested(
                        &block.fields,
                        is_denied,
                        array_key,
                        array_block_type,
                        &path,
                        out,
                    );
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_helpers::*;
    use super::*;
    use crate::core::{FieldAccess, FieldTab, FieldType};

    fn flat(name: &str) -> FieldDenial {
        FieldDenial::Flat(name.to_string())
    }

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
                read: Some("test_access.allow".into()),
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
                    read: Some("test_access.deny".into()),
                    ..Default::default()
                },
            ),
            make_field("title", FieldAccess::default()),
        ];
        let denied = check_field_read_access_with_lua(&lua, &fields, None, None);
        assert_eq!(denied, vec![flat("secret")]);
    }

    #[test]
    fn field_read_constrained_counts_as_allowed() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "status",
            FieldAccess {
                read: Some("test_access.constrained_string".into()),
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
                read: Some("test_access.throw_error".into()),
                ..Default::default()
            },
        )];
        let denied = check_field_read_access_with_lua(&lua, &fields, None, None);
        assert_eq!(denied, vec![flat("broken")]);
    }

    #[test]
    fn field_read_mixed_access() {
        let lua = setup_lua();
        let fields = vec![
            make_field(
                "public",
                FieldAccess {
                    read: Some("test_access.allow".into()),
                    ..Default::default()
                },
            ),
            make_field(
                "secret",
                FieldAccess {
                    read: Some("test_access.deny".into()),
                    ..Default::default()
                },
            ),
            make_field("plain", FieldAccess::default()),
        ];
        let denied = check_field_read_access_with_lua(&lua, &fields, None, None);
        assert_eq!(denied, vec![flat("secret")]);
    }

    #[test]
    fn field_read_with_user_context() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "admin_only",
            FieldAccess {
                read: Some("test_access.check_user".into()),
                ..Default::default()
            },
        )];

        let admin = make_user_doc("admin");
        let denied = check_field_read_access_with_lua(&lua, &fields, Some(&admin), None);
        assert!(denied.is_empty());

        let viewer = make_user_doc("viewer");
        let denied = check_field_read_access_with_lua(&lua, &fields, Some(&viewer), None);
        assert_eq!(denied, vec![flat("admin_only")]);
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
                create: Some("test_access.deny".into()),
                ..Default::default()
            },
        )];
        let denied = check_field_write_access_with_lua(&lua, &fields, None, None, "create");
        assert_eq!(denied, vec![flat("locked")]);
    }

    #[test]
    fn field_write_update_denied() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "immutable",
            FieldAccess {
                update: Some("test_access.deny".into()),
                ..Default::default()
            },
        )];
        let denied = check_field_write_access_with_lua(&lua, &fields, None, None, "update");
        assert_eq!(denied, vec![flat("immutable")]);
    }

    #[test]
    fn field_write_create_allowed() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "title",
            FieldAccess {
                create: Some("test_access.allow".into()),
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
                create: Some("test_access.deny".into()),
                update: Some("test_access.deny".into()),
                ..Default::default()
            },
        )];
        // Unknown operation = no extractor = allowed (no restriction).
        let denied = check_field_write_access_with_lua(&lua, &fields, None, None, "delete");
        assert!(denied.is_empty());
    }

    #[test]
    fn field_write_error_counts_as_denied() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "broken",
            FieldAccess {
                create: Some("test_access.throw_error".into()),
                ..Default::default()
            },
        )];
        let denied = check_field_write_access_with_lua(&lua, &fields, None, None, "create");
        assert_eq!(denied, vec![flat("broken")]);
    }

    #[test]
    fn field_write_create_vs_update_different_access() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "role",
            FieldAccess {
                create: Some("test_access.allow".into()),
                update: Some("test_access.deny".into()),
                ..Default::default()
            },
        )];
        let denied = check_field_write_access_with_lua(&lua, &fields, None, None, "create");
        assert!(denied.is_empty());

        let denied = check_field_write_access_with_lua(&lua, &fields, None, None, "update");
        assert_eq!(denied, vec![flat("role")]);
    }

    #[test]
    fn field_write_constrained_counts_as_allowed() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "status",
            FieldAccess {
                create: Some("test_access.constrained_string".into()),
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
                update: Some("test_access.check_user".into()),
                ..Default::default()
            },
        )];
        let admin = make_user_doc("admin");
        let denied = check_field_write_access_with_lua(&lua, &fields, Some(&admin), None, "update");
        assert!(denied.is_empty());

        let viewer = make_user_doc("viewer");
        let denied =
            check_field_write_access_with_lua(&lua, &fields, Some(&viewer), None, "update");
        assert_eq!(denied, vec![flat("admin_only")]);
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
                        read: Some("test_access.deny".into()),
                        ..Default::default()
                    },
                )])
                .build(),
        ];
        let denied = check_field_read_access_with_lua(&lua, &fields, None, None);
        assert_eq!(denied, vec![flat("seo__title")]);
    }

    #[test]
    fn field_read_recurses_through_row_without_prefix() {
        let lua = setup_lua();
        let fields = vec![
            FieldDefinition::builder("layout", FieldType::Row)
                .fields(vec![make_field(
                    "secret",
                    FieldAccess {
                        read: Some("test_access.deny".into()),
                        ..Default::default()
                    },
                )])
                .build(),
        ];
        let denied = check_field_read_access_with_lua(&lua, &fields, None, None);
        assert_eq!(denied, vec![flat("secret")]);
    }

    #[test]
    fn field_write_recurses_into_group() {
        let lua = setup_lua();
        let fields = vec![
            FieldDefinition::builder("config", FieldType::Group)
                .fields(vec![make_field(
                    "debug",
                    FieldAccess {
                        create: Some("test_access.deny".into()),
                        ..Default::default()
                    },
                )])
                .build(),
        ];
        let denied = check_field_write_access_with_lua(&lua, &fields, None, None, "create");
        assert_eq!(denied, vec![flat("config__debug")]);
    }

    #[test]
    fn field_read_recurses_into_array_sub_field() {
        // A denied relationship/upload/scalar inside an array row must be
        // stripped from every row — emitted as a Nested denial keyed by the
        // array's data key.
        let lua = setup_lua();
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![make_field(
                    "name",
                    FieldAccess {
                        read: Some("test_access.deny".into()),
                        ..Default::default()
                    },
                )])
                .build(),
        ];
        let denied = check_field_read_access_with_lua(&lua, &fields, None, None);
        assert_eq!(
            denied,
            vec![FieldDenial::Nested {
                array_key: "items".into(),
                array_block_type: None,
                row_path: vec![],
                leaf: "name".into(),
            }]
        );
    }

    #[test]
    fn field_read_recurses_into_group_inside_array() {
        let lua = setup_lua();
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("meta", FieldType::Group)
                        .fields(vec![make_field(
                            "secret",
                            FieldAccess {
                                read: Some("test_access.deny".into()),
                                ..Default::default()
                            },
                        )])
                        .build(),
                ])
                .build(),
        ];
        let denied = check_field_read_access_with_lua(&lua, &fields, None, None);
        assert_eq!(
            denied,
            vec![FieldDenial::Nested {
                array_key: "items".into(),
                array_block_type: None,
                row_path: vec![DenialSeg::Group("meta".into())],
                leaf: "secret".into(),
            }]
        );
    }

    #[test]
    fn field_write_recurses_into_array_in_array() {
        let lua = setup_lua();
        let fields = vec![
            FieldDefinition::builder("outer", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("inner", FieldType::Array)
                        .fields(vec![make_field(
                            "locked",
                            FieldAccess {
                                create: Some("test_access.deny".into()),
                                ..Default::default()
                            },
                        )])
                        .build(),
                ])
                .build(),
        ];
        let denied = check_field_write_access_with_lua(&lua, &fields, None, None, "create");
        assert_eq!(
            denied,
            vec![FieldDenial::Nested {
                array_key: "outer".into(),
                array_block_type: None,
                row_path: vec![DenialSeg::Rows {
                    key: "inner".into(),
                    block_type: None
                }],
                leaf: "locked".into(),
            }]
        );
    }

    // ── has_any_field_access ─────────────────────────────────────────

    #[test]
    fn has_any_no_access_configured() {
        let fields = vec![
            make_field("title", FieldAccess::default()),
            make_field("body", FieldAccess::default()),
        ];
        assert!(!has_any_field_access(&fields, |f| f.access.read.as_ref()));
    }

    #[test]
    fn has_any_top_level_read() {
        let fields = vec![make_field(
            "secret",
            FieldAccess {
                read: Some("test_access.deny".into()),
                ..Default::default()
            },
        )];
        assert!(has_any_field_access(&fields, |f| f.access.read.as_ref()));
    }

    #[test]
    fn has_any_nested_in_group() {
        let fields = vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![make_field(
                    "canonical_url",
                    FieldAccess {
                        read: Some("test_access.deny".into()),
                        ..Default::default()
                    },
                )])
                .build(),
        ];
        assert!(has_any_field_access(&fields, |f| f.access.read.as_ref()));
    }

    #[test]
    fn has_any_nested_in_row() {
        let fields = vec![
            FieldDefinition::builder("layout", FieldType::Row)
                .fields(vec![make_field(
                    "secret",
                    FieldAccess {
                        create: Some("test_access.deny".into()),
                        ..Default::default()
                    },
                )])
                .build(),
        ];
        assert!(has_any_field_access(&fields, |f| f.access.create.as_ref()));
    }

    #[test]
    fn has_any_includes_array_sub_fields() {
        // Field access inside an array IS enforced (nested-JSON stripping).
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![make_field(
                    "name",
                    FieldAccess {
                        read: Some("test_access.deny".into()),
                        ..Default::default()
                    },
                )])
                .build(),
        ];
        assert!(has_any_field_access(&fields, |f| f.access.read.as_ref()));
    }

    #[test]
    fn has_any_deeply_nested_group_in_row() {
        let fields = vec![
            FieldDefinition::builder("row", FieldType::Row)
                .fields(vec![
                    FieldDefinition::builder("grp", FieldType::Group)
                        .fields(vec![make_field(
                            "deep",
                            FieldAccess {
                                update: Some("test_access.deny".into()),
                                ..Default::default()
                            },
                        )])
                        .build(),
                ])
                .build(),
        ];
        assert!(has_any_field_access(&fields, |f| f.access.update.as_ref()));
    }

    #[test]
    fn field_read_recurses_into_tabs_sub_fields() {
        let lua = setup_lua();
        let fields = vec![
            FieldDefinition::builder("layout", FieldType::Tabs)
                .tabs(vec![FieldTab {
                    label: "Main".to_string(),
                    description: None,
                    fields: vec![make_field(
                        "secret",
                        FieldAccess {
                            read: Some("test_access.deny".into()),
                            ..Default::default()
                        },
                    )],
                }])
                .build(),
        ];
        let denied = check_field_read_access_with_lua(&lua, &fields, None, None);
        assert_eq!(denied, vec![flat("secret")]);
    }

    #[test]
    fn field_write_recurses_into_tabs_sub_fields() {
        let lua = setup_lua();
        let fields = vec![
            FieldDefinition::builder("layout", FieldType::Tabs)
                .tabs(vec![FieldTab {
                    label: "Settings".to_string(),
                    description: None,
                    fields: vec![make_field(
                        "locked",
                        FieldAccess {
                            create: Some("test_access.deny".into()),
                            ..Default::default()
                        },
                    )],
                }])
                .build(),
        ];
        let denied = check_field_write_access_with_lua(&lua, &fields, None, None, "create");
        assert_eq!(denied, vec![flat("locked")]);
    }

    #[test]
    fn has_any_nested_in_tabs() {
        let fields = vec![
            FieldDefinition::builder("layout", FieldType::Tabs)
                .tabs(vec![FieldTab {
                    label: "SEO".to_string(),
                    description: None,
                    fields: vec![make_field(
                        "meta_title",
                        FieldAccess {
                            read: Some("test_access.deny".into()),
                            ..Default::default()
                        },
                    )],
                }])
                .build(),
        ];
        assert!(has_any_field_access(&fields, |f| f.access.read.as_ref()));
    }

    #[test]
    fn has_any_write_checks_correct_extractor() {
        let fields = vec![make_field(
            "title",
            FieldAccess {
                create: Some("test_access.deny".into()),
                ..Default::default()
            },
        )];
        assert!(!has_any_field_access(&fields, |f| f.access.update.as_ref()));
        assert!(has_any_field_access(&fields, |f| f.access.create.as_ref()));
    }
}
