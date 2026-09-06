//! Document validation without persistence.

use std::collections::HashMap;

use crate::{
    core::{
        CollectionDefinition, Document, FieldDefinition, RequiredLocales, collection::Hooks,
        nest_group_fields,
    },
    db::{DbConnection, LocaleContext},
    hooks::{HookContext, ValidationCtx},
    service::{WriteInput, hooks::WriteHooks},
};

use super::ServiceError;

type Result<T> = std::result::Result<T, ServiceError>;

/// Remove the server-derived upload columns from an untrusted write's data.
///
/// On an upload collection, `url` / `{size}[_fmt]_url` / `filename` / dimensions
/// are computed by the upload pipeline (`inject_upload_metadata`) from the
/// processed file — a caller must never set them directly. The serve access
/// gate authorizes a file request by matching it against the stored
/// `url`/`*_url` columns, and `delete_upload_files` deletes the files those
/// columns name, so a forged value there discloses or deletes another
/// document's file. Only the multipart upload handlers (which processed a real
/// file and set `trusted_upload_metadata`) bypass this; every other surface
/// (Lua, gRPC, MCP, generic admin) has these columns stripped here — the one
/// chokepoint all write paths pass through. `focal_x`/`focal_y` are left
/// writable: the focal point is a legitimate user setting, not file-derived.
pub(super) fn strip_untrusted_upload_metadata(
    input: &mut WriteInput<'_>,
    def: &CollectionDefinition,
) {
    if input.trusted_upload_metadata {
        return;
    }

    let Some(upload) = def.upload.as_ref() else {
        return;
    };

    for name in upload.derived_field_names() {
        input.data.remove(&name);
    }
}

/// The single input-canonicalization step every persisting write path runs up
/// front: nest group fields to the in-memory shape (idempotent — already-nested
/// input passes through, the DB edge flattens back to columns), then strip
/// server-derived upload columns from untrusted input.
///
/// Both halves MUST happen together on every write. When the strip was bolted
/// on beside `nest_group_fields` at each call site instead, the bulk-update path
/// was missed and a forged `url` bypassed the serve gate — so the two are fused
/// here and the `write_paths_canonicalize_before_persist` guard test pins that
/// any `persist_*` caller in this module also calls this, closing the class to
/// a future write path.
pub(super) fn canonicalize_write_input(input: &mut WriteInput<'_>, def: &CollectionDefinition) {
    input.data = nest_group_fields(&input.data, &def.fields);
    strip_untrusted_upload_metadata(input, def);
}

/// Split a [`validate_document`] result into the surface-agnostic
/// `(valid, per_field_errors)` pair shared by every validate handler
/// (gRPC / MCP, collection / global). `Ok(())` is valid; a `Validation` error
/// becomes the field-error map; any other error propagates for the caller to map
/// to its own transport (gRPC `reclassify` → `Status`, MCP `into_anyhow`).
///
/// # Errors
///
/// Returns the non-validation `ServiceError` unchanged.
pub fn validate_outcome(result: Result<()>) -> Result<(bool, HashMap<String, String>)> {
    match result {
        Ok(()) => Ok((true, HashMap::new())),
        Err(ServiceError::Validation(ve)) => Ok((false, ve.to_field_map())),
        Err(e) => Err(e),
    }
}

/// Context for a validate-only run (no persist).
pub struct ValidateContext<'a> {
    pub slug: &'a str,
    /// Table name for unique checks — collection slug or `_global_{slug}`.
    pub table_name: &'a str,
    pub fields: &'a [FieldDefinition],
    pub hooks: &'a Hooks,
    pub operation: &'a str,
    /// Exclude this document from unique checks (update path).
    pub exclude_id: Option<&'a str>,
    pub soft_delete: bool,
    /// Whether the collection/global supports drafts (`has_drafts()`). The
    /// dry-run clamps `draft && supports_drafts` here — the one chokepoint —
    /// mirroring the real write path, so `draft=true` on a draft-disabled
    /// collection can't relax required-field checks on one surface but not
    /// another.
    pub supports_drafts: bool,
    /// Collection-level `required_locales` default, so the dry-run mirrors the
    /// completeness check that `create`/`update` apply. `None` for globals.
    pub required_locales: Option<&'a RequiredLocales>,
}

/// Validate a document without persisting — runs the full before-write pipeline
/// (field stripping, field hooks, validation, collection hooks) and returns.
///
/// Used by live validation endpoints.
///
/// # Errors
///
/// Returns service-layer errors (validation failures, hook errors) without
/// touching the database.
pub fn validate_document(
    conn: &dyn DbConnection,
    write_hooks: &dyn WriteHooks,
    ctx: &ValidateContext<'_>,
    mut input: WriteInput<'_>,
    user: Option<&Document>,
) -> Result<()> {
    // Note: collection-level access check is intentionally skipped here.
    // Validation endpoints already check access before calling this function.

    // Canonicalize incoming data to nested groups up front (idempotent) so the
    // dry-run pipeline matches the real write path.
    input.data = nest_group_fields(&input.data, ctx.fields);

    let is_draft = input.draft && ctx.supports_drafts;

    // Strip write-denied fields (data-aware: each `access.create`/`access.update`
    // rule sees `ctx.data` = its level and `ctx.document` = the incoming document).
    write_hooks.strip_write_access_data(
        ctx.fields,
        &mut input.data,
        ctx.slug,
        user,
        input.locale_ctx.map(LocaleContext::access_locale),
        ctx.operation,
    );

    let hook_data = input.data.clone();

    let mut hook_ctx_builder = HookContext::builder(ctx.slug, ctx.operation)
        .data(hook_data)
        .locale(input.locale_ctx.map(LocaleContext::access_locale))
        .draft(is_draft)
        .user(user);
    // On an update dry-run, expose the target id (matches the real write path,
    // so a field hook's `ctx.id` agrees between validate and persist).
    if let Some(id) = ctx.exclude_id {
        hook_ctx_builder = hook_ctx_builder.document_id(id);
    }
    let hook_ctx = hook_ctx_builder.build();

    let val_ctx = ValidationCtx::builder(conn, ctx.table_name)
        .exclude_id(ctx.exclude_id)
        .draft(is_draft)
        .locale_ctx(input.locale_ctx)
        .soft_delete(ctx.soft_delete)
        .collection_required_locales(ctx.required_locales)
        .user(user)
        .ui_locale(input.ui_locale.as_deref())
        .build();

    write_hooks.run_before_write(ctx.hooks, ctx.fields, hook_ctx, &val_ctx)?;

    Ok(())
}

#[cfg(test)]
mod strip_tests {
    use super::strip_untrusted_upload_metadata;
    use crate::core::upload::CollectionUpload;
    use crate::core::{CollectionDefinition, DocumentFields};
    use crate::service::WriteInput;
    use serde_json::json;

    fn upload_def() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("media");
        def.upload = Some(CollectionUpload::new());

        def
    }

    /// Untrusted writes (Lua / gRPC / MCP / generic admin) must not set the
    /// server-derived upload columns — a forged `url` there bypasses the serve
    /// access gate and lets `delete_upload_files` target another doc's file.
    /// `focal_x`/`focal_y` stay writable (user-editable focal point).
    #[test]
    fn strip_removes_derived_columns_from_untrusted_write() {
        let def = upload_def();
        let mut data = DocumentFields::new();
        data.insert("url".into(), json!("/uploads/media/victim.jpg"));
        data.insert("filename".into(), json!("victim.jpg"));
        data.insert("focal_x".into(), json!(0.5));
        data.insert("caption".into(), json!("hi"));

        let mut input = WriteInput::builder(data).build();
        strip_untrusted_upload_metadata(&mut input, &def);

        assert!(
            !input.data.contains_key("url"),
            "a forged url must be stripped from an untrusted write"
        );
        assert!(!input.data.contains_key("filename"));
        assert_eq!(
            input.data.get("focal_x"),
            Some(&json!(0.5)),
            "focal point stays writable"
        );
        assert_eq!(
            input.data.get("caption"),
            Some(&json!("hi")),
            "user fields are untouched"
        );
    }

    /// The multipart upload handlers inject real server metadata and mark the
    /// write trusted; the strip must leave those values intact.
    #[test]
    fn strip_preserves_derived_columns_on_trusted_write() {
        let def = upload_def();
        let mut data = DocumentFields::new();
        data.insert("url".into(), json!("/uploads/media/real.jpg"));

        let mut input = WriteInput::builder(data)
            .trusted_upload_metadata(true)
            .build();
        strip_untrusted_upload_metadata(&mut input, &def);

        assert_eq!(
            input.data.get("url"),
            Some(&json!("/uploads/media/real.jpg")),
            "trusted (server-injected) metadata must survive"
        );
    }

    /// A non-upload collection is never touched (a real field named `url`).
    #[test]
    fn strip_is_noop_on_non_upload_collection() {
        let def = CollectionDefinition::new("posts");
        let mut data = DocumentFields::new();
        data.insert("url".into(), json!("/whatever"));

        let mut input = WriteInput::builder(data).build();
        strip_untrusted_upload_metadata(&mut input, &def);

        assert_eq!(input.data.get("url"), Some(&json!("/whatever")));
    }
}
