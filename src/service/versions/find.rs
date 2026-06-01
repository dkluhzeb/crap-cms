//! Find a specific version by ID.

use serde_json::Value;

use crate::{
    core::document::VersionSnapshot,
    db::{AccessResult, query},
    service::{Def, ServiceContext, ServiceError, helpers},
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

    let access = hooks.check_access(ctx.read_access_ref(), ctx.user, None, None)?;

    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Read access denied".into()));
    }

    let Some(mut version) = query::find_version_by_id(conn, &table, version_id)? else {
        return Ok(None);
    };

    // Constrained: for collections enforce against the version's parent id;
    // for globals, the filter table is meaningless (single row) and is rejected.
    if matches!(access, AccessResult::Constrained(_)) {
        if let Def::Global(_) = &ctx.def {
            return Err(ServiceError::HookError(format!(
                "Access hook for global '{}' returned a filter table; globals don't support filter-based access — return true/false based on ctx.user fields instead.",
                ctx.slug
            )));
        }
        let parent_id = version.parent.to_string();
        helpers::enforce_access_constraints(ctx, &parent_id, &access, "Read", false)?;
    }

    // Strip read-denied fields from the snapshot JSON
    let mut denied = hooks.field_read_denied(ctx.fields()?, ctx.user);
    denied.extend(helpers::collect_api_hidden_field_names(ctx.fields()?, ""));

    if !denied.is_empty() {
        strip_snapshot_fields(&mut version.snapshot, &denied);
    }

    Ok(Some(version))
}

/// Strip `__`-separated field names from a snapshot `Value::Object`.
fn strip_snapshot_fields(snapshot: &mut Value, denied: &[String]) {
    let Some(map) = snapshot.as_object_mut() else {
        return;
    };

    for name in denied {
        map.remove(name);

        // Handle nested group subfields (snapshot stores groups as nested objects)
        let segments: Vec<&str> = name.split("__").collect();

        if segments.len() >= 2 {
            strip_nested_snapshot(map, &segments);
        }
    }
}

fn strip_nested_snapshot(map: &mut serde_json::Map<String, Value>, segments: &[&str]) {
    let Some((&first, rest)) = segments.split_first() else {
        return;
    };

    let Some(Value::Object(inner)) = map.get_mut(first) else {
        return;
    };

    if rest.len() == 1 {
        inner.remove(rest[0]);
    } else {
        strip_nested_snapshot(inner, rest);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn strips_top_level_denied_fields() {
        let mut snap = json!({ "title": "Hi", "secret": "x", "body": "text" });
        strip_snapshot_fields(&mut snap, &["secret".to_string()]);
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
        strip_snapshot_fields(&mut snap, &["meta__token".to_string()]);
        assert_eq!(snap, json!({ "meta": { "author": "ada" }, "title": "Hi" }));
    }

    #[test]
    fn strips_deeply_nested_subfield() {
        let mut snap = json!({ "a": { "b": { "c": 1, "d": 2 } } });
        strip_snapshot_fields(&mut snap, &["a__b__c".to_string()]);
        assert_eq!(snap, json!({ "a": { "b": { "d": 2 } } }));
    }

    #[test]
    fn missing_paths_and_non_objects_are_no_ops() {
        let mut snap = json!({ "title": "Hi" });
        strip_snapshot_fields(&mut snap, &["nope".to_string(), "a__b".to_string()]);
        assert_eq!(snap, json!({ "title": "Hi" }));

        // A non-object snapshot is left untouched.
        let mut arr = json!([1, 2, 3]);
        strip_snapshot_fields(&mut arr, &["x".to_string()]);
        assert_eq!(arr, json!([1, 2, 3]));
    }
}
