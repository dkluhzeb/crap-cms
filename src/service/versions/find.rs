//! Find a specific version by ID.

use serde_json::Value;

use crate::{
    core::{FieldDenial, document::VersionSnapshot},
    db::{AccessResult, query},
    hooks::AccessCheckInput,
    service::{
        Def, ServiceContext, ServiceError, helpers,
        versions::gate::{check_versions_gate, draft_snapshots_visible, reject_global_filter},
    },
};

/// Look up a single version snapshot by its ID.
///
/// Checks read access and strips read-denied fields from the snapshot.
/// Derives the version table from `ctx.slug` + `ctx.def`.
///
/// # Errors
///
/// Returns `AccessDenied` or `HookError`, or a backend error if the
/// SELECT fails.
pub fn find_version_by_id(
    ctx: &ServiceContext,
    version_id: &str,
) -> Result<Option<VersionSnapshot>, ServiceError> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let hooks = ctx.read_hooks()?;
    let table = ctx.version_table();

    // The `access.versions` toggle gates history access at all (default allow);
    // the read/draft composite below then scopes *which* snapshots are visible.
    check_versions_gate(ctx, hooks, None, "find_by_id")?;

    // Reading a version snapshot is gated by `access.read`; a DRAFT snapshot
    // additionally requires edit-level access (`access.draft ?? access.update`),
    // so a published-only reader can't fetch a draft snapshot by id. Mirrors the
    // version-list and document draft-visibility gates.
    let access = hooks.check_access(&AccessCheckInput {
        access: ctx.read_access_ref(),
        user: ctx.user,
        id: None,
        data: None,
        locale: None,
        operation: "find_by_id",
        collection: ctx.slug,
        ui_locale: None,
    })?;

    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Read access denied".into()));
    }

    let Some(mut version) = query::find_version_by_id(conn, &table, version_id)? else {
        return Ok(None);
    };

    // Hide a draft snapshot from a reader who lacks edit-level access — they may
    // see published version history, not work-in-progress. A constrained draft
    // rule is enforced against the parent document, so a non-match hides the
    // snapshot ("preview your own drafts" can't reveal another owner's).
    if version.status == "draft" {
        let draft_access = hooks.check_access(&AccessCheckInput {
            access: ctx.draft_access_ref(),
            user: ctx.user,
            id: None,
            data: None,
            locale: None,
            operation: "find_by_id",
            collection: ctx.slug,
            ui_locale: None,
        })?;

        let parent_id = version.parent.to_string();
        if !draft_snapshots_visible(ctx, &draft_access, &parent_id)? {
            return Ok(None);
        }
    }

    // Constrained read: for collections enforce against the version's parent id;
    // for globals, the filter table is meaningless (single row) and is rejected.
    if matches!(access, AccessResult::Constrained(_)) {
        if let Def::Global(_) = &ctx.def {
            return Err(reject_global_filter(ctx.slug));
        }
        let parent_id = version.parent.to_string();
        helpers::enforce_access_constraints(ctx, &parent_id, &access, "Read", false)?;
    }

    // Strip read-denied fields from the snapshot JSON
    let mut denied = hooks.field_read_denied(ctx.fields()?, ctx.user, None);
    denied.extend(helpers::collect_api_hidden_field_names(ctx.fields()?, ""));

    if !denied.is_empty() {
        strip_snapshot_fields(&mut version.snapshot, &denied);
    }

    Ok(Some(version))
}

/// Strip denied fields from a snapshot `Value::Object` (flat, group `__`, and
/// fields nested inside array/blocks rows). Shared with `list_versions`.
pub(super) fn strip_snapshot_fields(snapshot: &mut Value, denied: &[FieldDenial]) {
    let Some(map) = snapshot.as_object_mut() else {
        return;
    };

    for denial in denied {
        denial.strip_from(map);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn strips_top_level_denied_fields() {
        let mut snap = json!({ "title": "Hi", "secret": "x", "body": "text" });
        strip_snapshot_fields(&mut snap, &[FieldDenial::Flat("secret".into())]);
        assert_eq!(snap, json!({ "title": "Hi", "body": "text" }));
    }

    #[test]
    fn strips_nested_group_subfield_via_double_underscore_path() {
        // `meta__token` removes `token` inside the nested `meta` object,
        // leaving the group's other subfields intact.
        let mut snap = json!({
            "meta": { "token": "secret", "author": "ada" },
            "title": "Hi"
        });
        strip_snapshot_fields(&mut snap, &[FieldDenial::Flat("meta__token".into())]);
        assert_eq!(snap, json!({ "meta": { "author": "ada" }, "title": "Hi" }));
    }

    #[test]
    fn strips_deeply_nested_subfield() {
        let mut snap = json!({ "a": { "b": { "c": 1, "d": 2 } } });
        strip_snapshot_fields(&mut snap, &[FieldDenial::Flat("a__b__c".into())]);
        assert_eq!(snap, json!({ "a": { "b": { "d": 2 } } }));
    }

    #[test]
    fn missing_paths_and_non_objects_are_no_ops() {
        let mut snap = json!({ "title": "Hi" });
        strip_snapshot_fields(
            &mut snap,
            &[
                FieldDenial::Flat("nope".into()),
                FieldDenial::Flat("a__b".into()),
            ],
        );
        assert_eq!(snap, json!({ "title": "Hi" }));

        // A non-object snapshot is left untouched.
        let mut arr = json!([1, 2, 3]);
        strip_snapshot_fields(&mut arr, &[FieldDenial::Flat("x".into())]);
        assert_eq!(arr, json!([1, 2, 3]));
    }
}
