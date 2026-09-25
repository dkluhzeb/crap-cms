//! Strict keys: unknown or misplaced keys on a field table and its sub-tables are refused.

use mlua::{Lua, Table};

use crate::{core::HookRef, hooks::lua_api::parse::fields::single::parse_field_access};

use super::helpers::field_with;

#[test]
fn test_parse_field_access() {
    let lua = Lua::new();
    let tbl = lua.create_table().unwrap();
    tbl.set("read", "hooks.access.check_role").unwrap();
    tbl.set("create", "hooks.access.admin_only").unwrap();
    let access = parse_field_access(&tbl).unwrap();
    assert_eq!(
        access.read.as_ref().map(HookRef::reference),
        Some("hooks.access.check_role")
    );
    assert_eq!(
        access.create.as_ref().map(HookRef::reference),
        Some("hooks.access.admin_only")
    );
    assert!(access.update.is_none());
}

/// Regression: a non-string field access rule is a hard error, not
/// silently dropped (parity with collection/global `access`).
#[test]
fn test_parse_field_access_non_string_errors() {
    let lua = Lua::new();
    let tbl = lua.create_table().unwrap();
    tbl.set("update", 1i64).unwrap();
    let err = parse_field_access(&tbl).unwrap_err().to_string();
    assert!(err.contains("field access"), "got: {err}");
    assert!(err.contains("update"), "got: {err}");
}

#[test]
fn test_unknown_field_key_is_rejected() {
    let lua = Lua::new();
    // `requird` is a typo for `required`.
    let err = field_with(&lua, |f| {
        f.set("type", "text").unwrap();
        f.set("requird", true).unwrap();
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("requird"), "{err}");
    assert!(
        err.contains("required"),
        "should suggest closest key: {err}"
    );
}

#[test]
fn test_misplaced_key_rejected_per_type() {
    let lua = Lua::new();
    // `options` is valid on select, not on text — per-type check must reject it.
    let err = field_with(&lua, |f| {
        f.set("type", "text").unwrap();
        let opts = lua.create_table().unwrap();
        f.set("options", opts).unwrap();
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("options"), "{err}");
}

#[test]
fn test_type_specific_key_accepted_on_right_type() {
    let lua = Lua::new();
    // `options` on a select field is valid.
    assert!(
        field_with(&lua, |f| {
            f.set("type", "select").unwrap();
            let opts = lua.create_table().unwrap();
            f.set("options", opts).unwrap();
        })
        .is_ok()
    );
    // Legacy `relation_to` on a relationship field stays valid.
    assert!(
        field_with(&lua, |f| {
            f.set("type", "relationship").unwrap();
            f.set("relation_to", "users").unwrap();
        })
        .is_ok()
    );
}

/// A row/collapsible/tabs child for a hook-placement test.
fn set_layout_with_hook(lua: &Lua, f: &Table, ty: &str) {
    f.set("type", ty).unwrap();

    let fields = lua.create_table().unwrap();
    let child = lua.create_table().unwrap();
    child.set("name", "title").unwrap();
    child.set("type", "text").unwrap();
    fields.set(1, child).unwrap();

    if ty == "tabs" {
        // Tabs nest fields under a tab, not directly.
        let tab = lua.create_table().unwrap();
        tab.set("label", "Tab").unwrap();
        tab.set("fields", fields).unwrap();
        let tabs = lua.create_table().unwrap();
        tabs.set(1, tab).unwrap();
        f.set("tabs", tabs).unwrap();
    } else {
        f.set("fields", fields).unwrap();
    }

    let hooks = lua.create_table().unwrap();
    let bc = lua.create_table().unwrap();
    bc.set(1, "hooks.transform").unwrap();
    hooks.set("before_change", bc).unwrap();
    f.set("hooks", hooks).unwrap();
}

/// Regression: a lifecycle hook placed on a transparent layout wrapper
/// (row/collapsible/tabs) is rejected at parse time. These wrappers carry no
/// value, so the hook could never fire — it must fail loudly, not be a
/// silent no-op.
#[test]
fn test_lifecycle_hook_on_layout_wrapper_rejected() {
    for ty in ["row", "collapsible", "tabs"] {
        let lua = Lua::new();
        let err = field_with(&lua, |f| set_layout_with_hook(&lua, f, ty))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("layout wrapper") || err.contains("transparent"),
            "{ty}: {err}"
        );
    }
}

/// A lifecycle hook on a Group is valid — the group carries a value and runs
/// its own hook (parity with array/blocks).
#[test]
fn test_lifecycle_hook_on_group_accepted() {
    let lua = Lua::new();
    assert!(
        field_with(&lua, |f| set_layout_with_hook(&lua, f, "group")).is_ok(),
        "a hook on a group field must be accepted"
    );
}

#[test]
fn test_unknown_admin_key_is_rejected() {
    let lua = Lua::new();
    let err = field_with(&lua, |f| {
        f.set("type", "text").unwrap();
        let admin = lua.create_table().unwrap();
        admin.set("lable", "Title").unwrap(); // typo for `label`
        f.set("admin", admin).unwrap();
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("lable"), "{err}");
}

/// A field table `{ name = "title", admin = { position = "sidebar" } }`.
fn sidebar_text(lua: &Lua) -> Table {
    let admin = lua.create_table().unwrap();
    admin.set("position", "sidebar").unwrap();
    let child = lua.create_table().unwrap();
    child.set("name", "title").unwrap();
    child.set("admin", admin).unwrap();
    child
}

/// Regression: `admin.position` was accepted on a nested field, where it
/// has no effect — only top-level fields are split into the sidebar. A
/// top-level field keeps it; a group, row or block child is refused.
#[test]
fn test_admin_position_only_on_top_level_fields() {
    let lua = Lua::new();

    assert!(
        field_with(&lua, |f| {
            f.set("type", "text").unwrap();
            let admin = lua.create_table().unwrap();
            admin.set("position", "sidebar").unwrap();
            f.set("admin", admin).unwrap();
        })
        .is_ok()
    );

    for container in ["group", "row"] {
        let err = field_with(&lua, |f| {
            f.set("type", container).unwrap();
            f.set(
                "fields",
                lua.create_sequence_from([sidebar_text(&lua)]).unwrap(),
            )
            .unwrap();
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("admin.position"), "{container}: {err}");
        assert!(err.contains("'title'"), "{container}: {err}");
    }

    let err = field_with(&lua, |f| {
        f.set("type", "blocks").unwrap();
        let block = lua.create_table().unwrap();
        block.set("type", "hero").unwrap();
        block
            .set(
                "fields",
                lua.create_sequence_from([sidebar_text(&lua)]).unwrap(),
            )
            .unwrap();
        f.set("blocks", lua.create_sequence_from([block]).unwrap())
            .unwrap();
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("admin.position"), "{err}");
}

#[test]
fn test_admin_extra_escape_hatch_allows_custom_keys() {
    let lua = Lua::new();
    // Arbitrary keys under admin.extra are the plugin escape hatch.
    assert!(
        field_with(&lua, |f| {
            f.set("type", "text").unwrap();
            let admin = lua.create_table().unwrap();
            let extra = lua.create_table().unwrap();
            extra.set("max_stars", 5).unwrap();
            admin.set("extra", extra).unwrap();
            f.set("admin", admin).unwrap();
        })
        .is_ok()
    );
}

#[test]
fn test_unknown_field_nested_keys_rejected() {
    let lua = Lua::new();

    // field hooks
    let err = field_with(&lua, |f| {
        f.set("type", "text").unwrap();
        let hooks = lua.create_table().unwrap();
        hooks
            .set("before_save", lua.create_table().unwrap())
            .unwrap(); // not a real field hook
        f.set("hooks", hooks).unwrap();
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("before_save"), "{err}");

    // field access
    assert!(
        field_with(&lua, |f| {
            f.set("type", "text").unwrap();
            let access = lua.create_table().unwrap();
            access.set("delete", "hooks.x").unwrap(); // field access has no `delete`
            f.set("access", access).unwrap();
        })
        .is_err()
    );

    // field mcp
    assert!(
        field_with(&lua, |f| {
            f.set("type", "text").unwrap();
            let mcp = lua.create_table().unwrap();
            mcp.set("desc", "x").unwrap(); // not `description`
            f.set("mcp", mcp).unwrap();
        })
        .is_err()
    );
}

#[test]
fn test_unknown_relationship_key_rejected() {
    let lua = Lua::new();
    let err = field_with(&lua, |f| {
        f.set("type", "relationship").unwrap();
        let rel = lua.create_table().unwrap();
        rel.set("collection", "users").unwrap();
        rel.set("depth", 2).unwrap(); // not `max_depth`
        f.set("relationship", rel).unwrap();
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("depth"), "{err}");
}

#[test]
fn test_unknown_select_option_key_rejected() {
    let lua = Lua::new();
    let err = field_with(&lua, |f| {
        f.set("type", "select").unwrap();
        let opts = lua.create_table().unwrap();
        let opt = lua.create_table().unwrap();
        opt.set("label", "Draft").unwrap();
        opt.set("val", "draft").unwrap(); // not `value`
        opts.set(1, opt).unwrap();
        f.set("options", opts).unwrap();
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("val"), "{err}");
}

#[test]
fn test_unknown_block_and_tab_keys_rejected() {
    let lua = Lua::new();

    // block definition
    let err = field_with(&lua, |f| {
        f.set("type", "blocks").unwrap();
        let blocks = lua.create_table().unwrap();
        let block = lua.create_table().unwrap();
        block.set("type", "hero").unwrap();
        block.set("labl", "Hero").unwrap(); // typo for `label`
        blocks.set(1, block).unwrap();
        f.set("blocks", blocks).unwrap();
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("labl"), "{err}");

    // tab definition
    assert!(
        field_with(&lua, |f| {
            f.set("type", "tabs").unwrap();
            let tabs = lua.create_table().unwrap();
            let tab = lua.create_table().unwrap();
            tab.set("label", "General").unwrap();
            tab.set("desc", "x").unwrap(); // not `description`
            tabs.set(1, tab).unwrap();
            f.set("tabs", tabs).unwrap();
        })
        .is_err()
    );
}
