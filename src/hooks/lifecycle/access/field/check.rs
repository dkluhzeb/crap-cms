//! Lua-evaluated field-access checks: per-field access decisions and the
//! data-aware denied-NAME collectors (the `field_*_denied` introspection API).
//! Shares the pure walkers in [`super::walk`].

use mlua::Lua;

use tracing::warn;

use super::walk::{collect_denials_flat, extract_read_access};
use crate::core::{Document, DocumentFields, FieldDefinition, FieldDenial, HookRef};
use crate::db::AccessResult;
use crate::hooks::lifecycle::{AccessCheckInput, access::collection::check_access_with_lua};

pub(crate) fn check_field_read_access_with_lua(
    lua: &Lua,
    fields: &[FieldDefinition],
    collection: &str,
    user: Option<&Document>,
    locale: Option<&str>,
) -> Vec<FieldDenial> {
    collect_field_access_denied(
        lua,
        fields,
        collection,
        user,
        locale,
        extract_read_access,
        "read",
    )
}

/// Check field-level write access using an already-held `&Lua` reference.
/// Returns the fields to strip from the input. Recurses into Group (with `__`
/// prefix), transparent layout containers, and array/blocks rows.
pub(crate) fn check_field_write_access_with_lua(
    lua: &Lua,
    fields: &[FieldDefinition],
    collection: &str,
    user: Option<&Document>,
    locale: Option<&str>,
    operation: &str,
) -> Vec<FieldDenial> {
    let Some(extractor) = FieldDefinition::write_access_extractor(operation) else {
        return Vec::new();
    };

    collect_field_access_denied(lua, fields, collection, user, locale, extractor, operation)
}

/// Evaluate one field's access function. `true` = denied (or errored, which is
/// fail-closed to denied).
fn access_denied(
    lua: &Lua,
    hook: &HookRef,
    collection: &str,
    user: Option<&Document>,
    locale: Option<&str>,
    operation: &str,
) -> bool {
    match check_access_with_lua(
        lua,
        &AccessCheckInput::builder(operation, collection)
            .access(Some(hook))
            .user(user)
            // `data`/`document` threaded by the data-aware field-strip walker;
            // `None` here on the legacy document-independent path.
            .locale(locale)
            .build(),
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
    collection: &str,
    user: Option<&Document>,
    locale: Option<&str>,
    extractor: fn(&FieldDefinition) -> Option<&HookRef>,
    operation: &str,
) -> Vec<FieldDenial> {
    let is_denied = |field: &FieldDefinition| {
        extractor(field)
            .is_some_and(|hook| access_denied(lua, hook, collection, user, locale, operation))
    };

    let mut denied = Vec::new();
    collect_denials_flat(fields, &is_denied, "", &mut denied);

    denied
}

/// Data-aware collection of read-denied field **paths** for a single `document`,
/// for surfaces that need denial NAMES (the admin form's input dropping, the
/// `crap.access.field_read_denied` introspection API) rather than stripping
/// values in place. Each `access.read` rule is evaluated with `ctx.data` =
/// `ctx.document` = `document` (document-level context; per-row granularity is
/// only meaningful for the in-place value strip, not for a flat name list).
/// Shares the canonical [`collect_denials_flat`] recursion so the reported paths
/// match what [`strip_read_access_with_lua`] removes.
pub(crate) fn collect_read_denied_with_lua(
    lua: &Lua,
    fields: &[FieldDefinition],
    document: &DocumentFields,
    collection: &str,
    user: Option<&Document>,
    locale: Option<&str>,
) -> Vec<FieldDenial> {
    let is_denied = |field: &FieldDefinition| {
        field.access.read.as_ref().is_some_and(|hook| {
            match check_access_with_lua(
                lua,
                &AccessCheckInput::builder("read", collection)
                    .access(Some(hook))
                    .user(user)
                    .data(Some(document))
                    .document(Some(document))
                    .locale(locale)
                    .build(),
            ) {
                Ok(AccessResult::Allowed | AccessResult::Constrained(_)) => false,
                Ok(AccessResult::Denied) => true,
                Err(e) => {
                    warn!(
                        "Field read access function '{}' error (treating as denied): {}",
                        hook.reference(),
                        e
                    );

                    true
                }
            }
        })
    };

    let mut denied = Vec::new();
    collect_denials_flat(fields, &is_denied, "", &mut denied);

    denied
}

/// Data-aware collection of write-denied field **paths** for a single `document`
/// under `operation` (`"create"` / `"update"`) — the write-side mirror of
/// [`collect_read_denied_with_lua`], for the `crap.access.field_write_denied`
/// introspection API. Each `access.create` / `access.update` rule is evaluated
/// with `ctx.data` = `ctx.document` = `document` (document-level context), so the
/// reported names match what [`strip_write_access_with_lua`] would remove for
/// that document. An unknown `operation` yields no denials.
pub(crate) fn collect_write_denied_with_lua(
    lua: &Lua,
    fields: &[FieldDefinition],
    document: &DocumentFields,
    collection: &str,
    user: Option<&Document>,
    locale: Option<&str>,
    operation: &str,
) -> Vec<FieldDenial> {
    let Some(extract) = FieldDefinition::write_access_extractor(operation) else {
        return Vec::new();
    };

    let is_denied = |field: &FieldDefinition| {
        extract(field).is_some_and(|hook| {
            match check_access_with_lua(
                lua,
                &AccessCheckInput::builder(operation, collection)
                    .access(Some(hook))
                    .user(user)
                    .data(Some(document))
                    .document(Some(document))
                    .locale(locale)
                    .build(),
            ) {
                Ok(AccessResult::Allowed | AccessResult::Constrained(_)) => false,
                Ok(AccessResult::Denied) => true,
                Err(e) => {
                    warn!(
                        "Field {operation} access function '{}' error (treating as denied): {}",
                        hook.reference(),
                        e
                    );

                    true
                }
            }
        })
    };

    let mut denied = Vec::new();
    collect_denials_flat(fields, &is_denied, "", &mut denied);

    denied
}

#[cfg(test)]
mod tests {
    use super::super::super::test_helpers::*;
    use super::*;
    use crate::core::{
        DenialSeg, DocumentFields, FieldAccess, FieldDefinition, FieldDenial, FieldTab, FieldType,
    };
    use serde_json::json;

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
        let denied = check_field_read_access_with_lua(&lua, &fields, "", None, None);
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
        let denied = check_field_read_access_with_lua(&lua, &fields, "", None, None);
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
        let denied = check_field_read_access_with_lua(&lua, &fields, "", None, None);
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
        let denied = check_field_read_access_with_lua(&lua, &fields, "", None, None);
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
        let denied = check_field_read_access_with_lua(&lua, &fields, "", None, None);
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
        let denied = check_field_read_access_with_lua(&lua, &fields, "", None, None);
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
        let denied = check_field_read_access_with_lua(&lua, &fields, "", Some(&admin), None);
        assert!(denied.is_empty());

        let viewer = make_user_doc("viewer");
        let denied = check_field_read_access_with_lua(&lua, &fields, "", Some(&viewer), None);
        assert_eq!(denied, vec![flat("admin_only")]);
    }

    // ── check_field_write_access_with_lua ───────────────────────────────

    #[test]
    fn field_write_no_access_config_allows_all() {
        let lua = setup_lua();
        let fields = vec![make_field("title", FieldAccess::default())];
        let denied = check_field_write_access_with_lua(&lua, &fields, "", None, None, "create");
        assert!(denied.is_empty());
        let denied = check_field_write_access_with_lua(&lua, &fields, "", None, None, "update");
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
        let denied = check_field_write_access_with_lua(&lua, &fields, "", None, None, "create");
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
        let denied = check_field_write_access_with_lua(&lua, &fields, "", None, None, "update");
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
        let denied = check_field_write_access_with_lua(&lua, &fields, "", None, None, "create");
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
        let denied = check_field_write_access_with_lua(&lua, &fields, "", None, None, "delete");
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
        let denied = check_field_write_access_with_lua(&lua, &fields, "", None, None, "create");
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
        let denied = check_field_write_access_with_lua(&lua, &fields, "", None, None, "create");
        assert!(denied.is_empty());

        let denied = check_field_write_access_with_lua(&lua, &fields, "", None, None, "update");
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
        let denied = check_field_write_access_with_lua(&lua, &fields, "", None, None, "create");
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
        let denied =
            check_field_write_access_with_lua(&lua, &fields, "", Some(&admin), None, "update");
        assert!(denied.is_empty());

        let viewer = make_user_doc("viewer");
        let denied =
            check_field_write_access_with_lua(&lua, &fields, "", Some(&viewer), None, "update");
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
        let denied = check_field_read_access_with_lua(&lua, &fields, "", None, None);
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
        let denied = check_field_read_access_with_lua(&lua, &fields, "", None, None);
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
        let denied = check_field_write_access_with_lua(&lua, &fields, "", None, None, "create");
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
        let denied = check_field_read_access_with_lua(&lua, &fields, "", None, None);
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
        let denied = check_field_read_access_with_lua(&lua, &fields, "", None, None);
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
        let denied = check_field_write_access_with_lua(&lua, &fields, "", None, None, "create");
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
        let denied = check_field_read_access_with_lua(&lua, &fields, "", None, None);
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
        let denied = check_field_write_access_with_lua(&lua, &fields, "", None, None, "create");
        assert_eq!(denied, vec![flat("locked")]);
    }

    // ── Data-aware denial-NAME collectors (the `field_*_denied` introspection
    //    helpers' doc-aware branch) ───────────────────────────────────────────

    fn doc(value: serde_json::Value) -> DocumentFields {
        serde_json::from_value(value).expect("valid document")
    }

    #[test]
    fn collect_write_denied_routes_by_operation() {
        let lua = setup_lua();
        let fields = vec![
            make_field(
                "auto_slug",
                FieldAccess {
                    create: Some("test_access.deny".into()),
                    ..Default::default()
                },
            ),
            make_field(
                "locked",
                FieldAccess {
                    update: Some("test_access.deny".into()),
                    ..Default::default()
                },
            ),
        ];
        let d = doc(json!({}));

        // create-denied field shows up under "create", not "update".
        let on_create = collect_write_denied_with_lua(&lua, &fields, &d, "", None, None, "create");
        assert_eq!(on_create, vec![flat("auto_slug")]);

        let on_update = collect_write_denied_with_lua(&lua, &fields, &d, "", None, None, "update");
        assert_eq!(on_update, vec![flat("locked")]);
    }

    #[test]
    fn collect_write_denied_unknown_operation_is_empty() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "x",
            FieldAccess {
                create: Some("test_access.deny".into()),
                ..Default::default()
            },
        )];
        let denied =
            collect_write_denied_with_lua(&lua, &fields, &doc(json!({})), "", None, None, "delete");
        assert!(denied.is_empty());
    }

    #[test]
    fn collect_write_denied_is_data_aware() {
        // `check_data` allows the write only when ctx.data.title == "test".
        let lua = setup_lua();
        let fields = vec![make_field(
            "title",
            FieldAccess {
                create: Some("test_access.check_data".into()),
                ..Default::default()
            },
        )];

        let allowed = collect_write_denied_with_lua(
            &lua,
            &fields,
            &doc(json!({ "title": "test" })),
            "",
            None,
            None,
            "create",
        );
        assert!(
            allowed.is_empty(),
            "data-aware rule should allow when ctx.data matches"
        );

        let denied = collect_write_denied_with_lua(
            &lua,
            &fields,
            &doc(json!({ "title": "other" })),
            "",
            None,
            None,
            "create",
        );
        assert_eq!(denied, vec![flat("title")]);
    }

    #[test]
    fn collect_read_denied_is_data_aware() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "title",
            FieldAccess {
                read: Some("test_access.check_data".into()),
                ..Default::default()
            },
        )];

        let allowed = collect_read_denied_with_lua(
            &lua,
            &fields,
            &doc(json!({ "title": "test" })),
            "",
            None,
            None,
        );
        assert!(allowed.is_empty());

        let denied = collect_read_denied_with_lua(
            &lua,
            &fields,
            &doc(json!({ "title": "x" })),
            "",
            None,
            None,
        );
        assert_eq!(denied, vec![flat("title")]);
    }
}
