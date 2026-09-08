//! Preparing a user document for an auth RPC response.
//!
//! `LoginResponse.user` / `MeResponse.user` carry the same contract every
//! other `Document` on the wire does: hydrated join fields, field-level read
//! access applied, API-hidden columns removed. The login and MFA paths load
//! the row through the credential lookups (`find_by_email`,
//! `reload_authenticated_user`), which are raw reads — without this they
//! would ship a `hidden` field or an `access.read`-denied one that `Me`
//! strips.

use serde_json::{Map, Value};

use crate::{
    core::{CollectionDefinition, Document},
    db::{BoxedConnection, LocaleContext, query},
    hooks::lifecycle::access::ReadStripInput,
    service::{AppInfra, helpers::collect_api_hidden_field_names},
};

/// Hydrate join fields, then apply the read strips, in place.
///
/// The user document is its own access context: a field rule on an auth
/// collection typically compares `ctx.user` with the row being read.
pub(super) fn prepare_user_document(
    infra: &AppInfra,
    def: &CollectionDefinition,
    collection: &str,
    doc: &mut Document,
    conn: &BoxedConnection,
) {
    let locale_ctx = LocaleContext::default_for(&infra.locale_config);

    if let Err(e) = query::hydrate_document(
        conn,
        collection,
        &def.fields,
        doc,
        None,
        locale_ctx.as_ref(),
    ) {
        // Hydration failure costs join fields, not correctness of the strip
        // below — log and continue rather than fail an otherwise good login.
        tracing::error!("user document hydrate error for {collection}: {e:#}");
    }

    strip_user_document(infra, def, collection, doc, conn);
}

/// Apply field-read access rules (with the user's own document as context)
/// and the API-hidden strip.
pub(super) fn strip_user_document(
    infra: &AppInfra,
    def: &CollectionDefinition,
    collection: &str,
    doc: &mut Document,
    conn: &BoxedConnection,
) {
    let user_snapshot = doc.clone();
    let mut level: Map<String, Value> = std::mem::take(&mut doc.fields)
        .into_inner()
        .into_iter()
        .collect();

    infra.hook_runner.strip_read_access(
        &def.fields,
        &mut level,
        &ReadStripInput {
            document: &user_snapshot.fields,
            collection,
            user: Some(&user_snapshot),
            locale: None,
        },
        conn,
    );

    doc.fields = level.into_iter().collect();
    doc.strip_fields(&collect_api_hidden_field_names(&def.fields, ""));
}
