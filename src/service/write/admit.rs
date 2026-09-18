//! The gate every document update passes before its before-write hooks run:
//! the pending draft is adopted as the write's base, the locale lock and the
//! `update` access rule are applied, and write-denied fields are stripped from
//! the request and from the draft it publishes. The single-document update and
//! the bulk update share it, so a rule enforced on one cannot be missing on
//! the other.

use serde_json::{Map, Value};

use crate::{
    core::{CollectionDefinition, DocumentFields},
    db::{DbConnection, LocaleContext},
    service::{
        ServiceContext, ServiceError, WriteInput,
        hooks::{SnapshotLocales, WriteHooks},
        write::{
            adopt_pending_draft, check_update_access, reject_locale_locked_fields,
            stored_fields_for_update_rules,
        },
    },
};

use super::validate::canonicalize_write_input;

/// Admit an update: canonicalize, adopt the pending draft, apply the locale
/// lock and the `update` access rule, then strip write-denied fields.
///
/// Returns the drafted snapshot the publish writes back — already stripped by
/// the publisher's field-level write access — or `None` when no draft is
/// pending or this is not a publish.
///
/// # Errors
///
/// Returns the locale-lock validation error or the access denial.
pub(super) fn admit_update(
    ctx: &ServiceContext,
    conn: &dyn DbConnection,
    id: &str,
    input: &mut WriteInput<'_>,
) -> Result<Option<Value>, ServiceError> {
    let def = ctx.collection_def()?;
    let write_hooks = ctx.write_hooks()?;

    // Canonicalize incoming data to nested groups up front (idempotent); the
    // whole pipeline sees one shape, the DB edge flattens to columns.
    canonicalize_write_input(input, def);

    // Publishing takes the pending draft as the write's base and lets the
    // request's own fields win over it — the file that draft stored included,
    // whose server-derived columns come from the snapshot, read here after the
    // strip so they are the server's own values and not something a caller
    // sent. Everything the draft contributes then passes the locale lock, the
    // access gates and validation exactly like a field the caller sent.
    let pending_draft = adopt_pending_draft(ctx, def, id, input)?;

    reject_locale_locked_fields(&def.fields, &input.data, input.locale_ctx)?;

    check_update_access(
        ctx,
        write_hooks,
        def,
        id,
        &input.data,
        input.locale_ctx.map(LocaleContext::access_locale),
        input.ui_locale.as_deref(),
    )?;

    // Strip write-denied fields before hook processing (data-aware: each
    // `access.update` rule sees `ctx.data` = its level and `ctx.document` = the
    // stored document, never the patch it is judging).
    let stored = stored_fields_for_update_rules(conn, ctx.slug, def, id, input.locale_ctx)?;
    write_hooks.strip_write_access_update(
        &def.fields,
        &mut input.data,
        &stored,
        ctx.slug,
        ctx.user,
        input.locale_ctx.map(LocaleContext::access_locale),
    );

    Ok(strip_publishing_draft(
        ctx,
        write_hooks,
        def,
        &stored,
        pending_draft,
        SnapshotLocales::for_write(input.locale_ctx),
    ))
}

/// The pending draft goes live as ONE unit, so the locales the request does
/// not target take their values from the snapshot too. The publisher's own
/// field-level write access decides there as well: the same rules that just
/// stripped the merged data run over the snapshot, judged — like every
/// `access.update` rule — against the stored row rather than the content they
/// are judging. Without this strip the write-back would publish exactly the
/// drafted change the request strip refused.
fn strip_publishing_draft(
    ctx: &ServiceContext,
    write_hooks: &dyn WriteHooks,
    def: &CollectionDefinition,
    stored: &DocumentFields,
    pending_draft: Option<Map<String, Value>>,
    locales: SnapshotLocales<'_>,
) -> Option<Value> {
    let mut snapshot = Value::Object(pending_draft?);

    write_hooks.strip_write_access_value(
        &def.fields,
        &mut snapshot,
        stored,
        ctx.slug,
        ctx.user,
        locales,
    );

    Some(snapshot)
}
