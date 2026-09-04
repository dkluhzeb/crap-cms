//! Bulk `CreateMany` RPC handler.
//!
//! Codec over [`op::run_blocking`].

use std::sync::Arc;

use tonic::{Request, Response, Status};

use crate::{
    api::{
        content,
        handlers::{
            ContentService,
            collection::helpers::extract_auth_password,
            proto::{data_map_to_json_map, document_to_proto},
        },
    },
    core::{DocumentFields, collection::Surface},
    db::LocaleContext,
    service::{
        CreateManyItem,
        jobs::bulk_queue::{BulkJobData, BulkOpKind, QueuedBy},
        op::{self, CreateMany, CreateManyArgs, Credentials, Principal, TargetRef},
    },
};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Bulk create multiple documents. Runs per-document lifecycle hooks by default.
    pub(in crate::api::handlers) async fn create_many_impl(
        &self,
        request: Request<content::CreateManyRequest>,
    ) -> Result<Response<content::CreateManyResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let headers = self.metadata_headers(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        // Auth collections: split each item's `password` off so the service
        // create chokepoint validates it against the password policy and hashes
        // it — parity with single Create and Lua `create_many`. Bulk create is
        // per-item (distinct passwords per user), so seeding auth users with
        // policed passwords in one transaction is a legitimate operation; only
        // `update_many` (a broadcast that would set one password on many rows)
        // rejects a password. A non-auth collection keeps a legitimate
        // `password` field as ordinary data.
        let is_auth = def.is_auth_collection();
        let mut items: Vec<CreateManyItem> = Vec::with_capacity(req.documents.len());
        for s in &req.documents {
            let mut data: DocumentFields = data_map_to_json_map(s).into();

            // Shared with single Create: a non-string password coerces to ""
            // and fails the policy (InvalidArgument) instead of being silently
            // dropped — the old inline `as_str()` extraction created a
            // passwordless auth document from `{"password": 12345}`.
            let password =
                extract_auth_password(&mut data, is_auth, &self.infra.password_policy, false)?;

            items.push(CreateManyItem { data, password });
        }

        // Honor the request's write locale exactly like single Create — the
        // proto field existed but was silently ignored before the wire model.
        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.infra.locale_config)
                .map_err(|e| Status::invalid_argument(e.to_string()))?;

        let principal = Principal::Credentials(Credentials {
            surface: Surface::Grpc,
            bearer: token,
            session_cookie: None,
            headers,
        });

        if req.queue.unwrap_or(false) {
            // Queued path: the payload persists in the jobs table until
            // execution — plaintext passwords must never land there.
            if items.iter().any(|i| i.password.is_some()) {
                return Err(Status::invalid_argument(
                    "queue=true cannot be combined with per-item passwords — run synchronously instead",
                ));
            }

            let job = BulkJobData {
                op: BulkOpKind::CreateMany,
                collection: req.collection.clone(),
                queued_by: QueuedBy::System, // stamped for real in queue_bulk_run
                ui_locale: None,             // ditto
                locale: req.locale.clone(),
                draft: req.draft.unwrap_or(false),
                hooks: req.hooks.unwrap_or(true),
                events: req.events.unwrap_or(false),
                max_documents: self.server_config.bulk_max_documents,
                documents: Some(items.into_iter().map(|i| i.data).collect()),
                where_clause: None,
                data: None,
                force_hard_delete: false,
            };

            let job_id = self.queue_bulk_run(principal, job).await?;

            return Ok(Response::new(content::CreateManyResponse {
                created: 0,
                documents: Vec::new(),
                job_id: Some(job_id),
            }));
        }

        let args = CreateManyArgs::builder(items)
            .run_hooks(req.hooks.unwrap_or(true))
            .draft(req.draft.unwrap_or(false))
            .locale_ctx(locale_ctx)
            .max_documents(self.server_config.bulk_max_documents)
            .events(req.events.unwrap_or(false))
            .build();

        let result = op::run_blocking::<CreateMany>(
            Arc::clone(&self.infra),
            principal,
            TargetRef::collection(req.collection.clone()),
            args,
        )
        .await
        .map_err(|e| self.core_error_status(e))?;

        let documents: Vec<content::Document> = result
            .documents
            .iter()
            .map(|doc| document_to_proto(doc, &req.collection))
            .collect();

        Ok(Response::new(content::CreateManyResponse {
            job_id: None,
            created: result.created,
            documents,
        }))
    }
}
