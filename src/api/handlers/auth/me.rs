//! Me handler — return the currently authenticated user.

use std::{collections::HashMap, sync::Arc};

use serde_json::{Map, Value};
use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        handlers::{ContentService, proto::document_to_proto},
    },
    core::{CollectionDefinition, Document},
    db::{BoxedConnection, query},
    hooks::lifecycle::access::ReadStripInput,
    service::{AppInfra, helpers::collect_api_hidden_field_names},
};

/// Owned bundle for the `Me` spawn-blocking body. Process-stable dependencies
/// come from the shared [`AppInfra`]; the rest is per-call.
struct MeBlockingInput {
    infra: Arc<AppInfra>,
    token: Option<String>,
    headers: HashMap<String, String>,
}

/// Resolve the caller through the shared auth evaluator (so `Me` honors the
/// issuing collection's `methods` / `surfaces` and custom strategies exactly
/// like every other RPC — it used to validate the JWT directly, answering
/// even when the collection had removed `bearer`), then load, hydrate and
/// access-strip the user document.
fn me_blocking(input: &MeBlockingInput) -> Result<(Document, String), Status> {
    let conn = input
        .infra
        .pool
        .get()
        .inspect_err(|e| error!("Me DB connection error: {}", e))
        .map_err(|_| Status::internal("Internal error"))?;

    let auth_user = ContentService::resolve_auth_user(
        input.token.as_deref(),
        &input.headers,
        &*input.infra.token_provider,
        &input.infra.hook_runner,
        &input.infra.registry,
        &conn,
    )?
    .ok_or_else(|| Status::unauthenticated("Missing token"))?;

    let collection = auth_user.claims.collection.to_string();
    let id = auth_user.claims.sub.to_string();
    let def = input
        .infra
        .registry
        .get_collection(&collection)
        .ok_or_else(|| Status::unauthenticated("Invalid or expired token"))?;

    let mut doc = query::find_by_id(&conn, &collection, def, &id, None)
        .inspect_err(|e| error!("Me find_by_id error: {}", e))
        .map_err(|_| Status::internal("Internal error"))?
        .ok_or_else(|| Status::not_found("User not found"))?;

    query::hydrate_document(&conn, &collection, &def.fields, &mut doc, None, None)
        .inspect_err(|e| error!("Me hydrate_document error: {}", e))
        .map_err(|_| Status::internal("Internal error"))?;

    strip_for_response(&input.infra, def, &collection, &mut doc, &conn);

    Ok((doc, collection))
}

/// Apply field-read access rules (with the user's own document as context)
/// and the API-hidden strip to the user document before it leaves the server.
fn strip_for_response(
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

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Return the currently authenticated user. The credential is the
    /// `authorization` metadata (Bearer), the legacy `token` request field, or
    /// any custom strategy matched by the request headers.
    pub(in crate::api::handlers) async fn me_impl(
        &self,
        request: Request<content::MeRequest>,
    ) -> Result<Response<content::MeResponse>, Status> {
        let metadata = request.metadata().clone();
        let req = request.into_inner();

        let token = Self::bearer_or_body_token(&metadata, &req.token);
        let headers = self.metadata_headers(&metadata);

        let input = MeBlockingInput {
            infra: Arc::clone(&self.infra),
            token,
            headers,
        };

        let (doc, collection) = task::spawn_blocking(move || me_blocking(&input))
            .await
            .inspect_err(|e| error!("Me task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::MeResponse {
            user: Some(document_to_proto(&doc, &collection)),
        }))
    }
}
