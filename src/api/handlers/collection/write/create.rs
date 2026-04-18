//! Create handler — create a new document in a collection.

use prost_types::value::Kind;
use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        handlers::{
            ContentService,
            collection::helpers::extract_auth_password,
            convert::{document_to_proto, prost_struct_to_hashmap, prost_struct_to_json_map},
        },
    },
    db::LocaleContext,
    service::{self, ServiceContext, ServiceError, WriteInput},
};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Create a new document, running before/after hooks within a transaction.
    pub(in crate::api::handlers) async fn create_impl(
        &self,
        request: Request<content::CreateRequest>,
    ) -> Result<Response<content::CreateResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        let join_data = req
            .data
            .as_ref()
            .map(prost_struct_to_json_map)
            .unwrap_or_default();

        let mut data = req
            .data
            .map(|s| prost_struct_to_hashmap(&s))
            .unwrap_or_default();

        let password = extract_auth_password(
            &mut data,
            def.is_auth_collection(),
            &self.password_policy,
            false,
        )?;

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config)
                .map_err(|e| Status::invalid_argument(e.to_string()))?;

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let token_provider = self.token_provider.clone();
        let registry = self.registry.clone();
        let db_kind = self.db_kind.clone();
        let event_transport = self.event_transport.clone();
        let collection = req.collection.clone();
        let def_owned = def;

        let proto_doc = task::spawn_blocking(move || -> Result<_, Status> {
            let conn = pool
                .get()
                .map_err(|e| Status::from(ServiceError::classify(e, &db_kind)))?;

            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;

            let user_doc = auth_user.as_ref().map(|au| au.user_doc.clone());
            let auth_user_ui_locale = auth_user.as_ref().map(|au| au.ui_locale.clone());
            let ui_locale = user_doc.as_ref().and_then(|_| auth_user_ui_locale.clone());

            let ctx = ServiceContext::collection(&collection, &def_owned)
                .pool(&pool)
                .runner(&runner)
                .user(user_doc.as_ref())
                .event_transport(event_transport)
                .build();

            let (doc, _req_context) = service::create_document(
                &ctx,
                WriteInput::builder(data, &join_data)
                    .password(password.as_deref())
                    .locale_ctx(locale_ctx.as_ref())
                    .draft(req.draft.unwrap_or(false))
                    .ui_locale(ui_locale)
                    .build(),
            )
            .map_err(|e| Status::from(e.reclassify(&db_kind)))?;

            let proto_doc = document_to_proto(&doc, &collection);

            Ok(proto_doc)
        })
        .await
        .inspect_err(|e| error!("Task error: {}", e))
        .map_err(|_| Status::internal("Internal error"))??;

        self.on_collection_mutation();

        self.maybe_send_verification(&req.collection, &proto_doc);

        Ok(Response::new(content::CreateResponse {
            document: Some(proto_doc),
        }))
    }

    /// Send verification email if this is an auth collection with verify_email enabled.
    fn maybe_send_verification(&self, collection: &str, proto_doc: &content::Document) {
        let Ok(def) = self.get_collection_def(collection) else {
            return;
        };

        let should_verify =
            def.is_auth_collection() && def.auth.as_ref().is_some_and(|a| a.verify_email);

        if !should_verify {
            return;
        }

        let email_val = proto_doc
            .fields
            .as_ref()
            .and_then(|s| s.fields.get("email"))
            .and_then(|v| {
                if let Some(Kind::StringValue(s)) = &v.kind {
                    Some(s.clone())
                } else {
                    None
                }
            });

        if let Some(user_email) = email_val {
            service::send_verification_email(
                self.pool.clone(),
                self.email_config.clone(),
                self.email_renderer.clone(),
                self.server_config.clone(),
                collection.to_string(),
                proto_doc.id.clone(),
                user_email,
            );
        }
    }
}
