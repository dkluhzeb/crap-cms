//! Find a specific version by ID.

use serde_json::Value;

use crate::{
    core::document::VersionSnapshot,
    db::{AccessResult, query},
    service::{ServiceContext, ServiceError, helpers},
};

/// Look up a single version snapshot by its ID.
///
/// Checks read access and strips read-denied fields from the snapshot.
/// Derives the version table from `ctx.slug` + `ctx.def`.
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

    let mut version = match query::find_version_by_id(conn, &table, version_id)? {
        Some(v) => v,
        None => return Ok(None),
    };

    // Strip read-denied fields from the snapshot JSON
    let mut denied = hooks.field_read_denied(ctx.fields(), ctx.user);
    denied.extend(helpers::collect_hidden_field_names(ctx.fields(), ""));

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
