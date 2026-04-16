//! Bulk DeleteMany RPC handler.

use anyhow::Context as _;
use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{content, handlers::ContentService},
    core::{event::EventOperation, upload},
    db::AccessResult,
    service::{RunnerWriteHooks, ServiceContext, ServiceError, delete_document_core},
};

use super::helpers::{build_bulk_filters, check_per_doc_access};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Bulk delete matching documents. Runs per-document lifecycle hooks by default.
    pub(in crate::api::handlers) async fn delete_many_impl(
        &self,
        request: Request<content::DeleteManyRequest>,
    ) -> Result<Response<content::DeleteManyResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let req = request.into_inner();
        let mut def = self.get_collection_def(&req.collection)?;
        let run_hooks = req.hooks.unwrap_or(true);

        let will_soft_delete = def.soft_delete && !req.force_hard_delete;
        let access_ref = if will_soft_delete {
            def.access.resolve_trash()
        } else {
            def.access.delete.as_deref()
        };
        let deny_msg = if will_soft_delete {
            "Trash access denied"
        } else {
            "Delete access denied"
        };

        if req.force_hard_delete && def.soft_delete {
            def.soft_delete = false;
        }

        let pool = self.pool.clone();
        let hook_runner = self.hook_runner.clone();
        let token_provider = self.token_provider.clone();
        let registry = self.registry.clone();
        let db_kind = self.db_kind.clone();
        let collection = req.collection.clone();
        let req_where = req.r#where.clone();
        let storage = self.storage.clone();
        let locale_cfg = self.locale_config.clone();
        let access_owned = access_ref.map(|s| s.to_string());
        let deny_msg_owned = deny_msg.to_string();
        let def_owned = def;
        let invalidation_transport = self.invalidation_transport.clone();

        let (hard_count, soft_count, skipped_count, deleted_ids) =
            task::spawn_blocking(move || -> Result<(i64, i64, i64, Vec<String>), Status> {
                let mut conn = pool
                    .get()
                    .map_err(|e| Status::from(ServiceError::classify(e, &db_kind)))?;

                let auth_user =
                    ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;

                let read_access = ContentService::check_access_blocking(
                    def_owned.access.read.as_deref(),
                    &auth_user,
                    None,
                    None,
                    &hook_runner,
                    &mut conn,
                )?;

                if matches!(read_access, AccessResult::Denied) {
                    return Err(Status::permission_denied("Read access denied"));
                }

                let user_doc = auth_user.as_ref().map(|au| &au.user_doc);

                // Process in batches: find a chunk of matching docs, delete
                // them, commit, repeat. This avoids loading all documents into
                // memory at once and keeps transactions short.
                const BATCH_SIZE: i64 = 500;

                let mut hard_count = 0i64;
                let mut soft_count = 0i64;
                let mut skipped_count = 0i64;
                let mut upload_fields_to_clean = Vec::new();
                let mut deleted_ids = Vec::new();

                loop {
                    let tx = conn
                        .transaction_immediate()
                        .context("Start delete transaction")
                        .map_err(|e| Status::from(ServiceError::classify(e, &db_kind)))?;

                    let batch_filters = build_bulk_filters(
                        &collection,
                        &def_owned,
                        &read_access,
                        req_where.as_deref(),
                        true,
                    )?;

                    let batch_query = crate::db::query::FindQuery::builder()
                        .filters(batch_filters)
                        .limit(BATCH_SIZE)
                        .build();

                    let docs =
                        crate::db::query::find(&tx, &collection, &def_owned, &batch_query, None)
                            .map_err(|e| Status::from(ServiceError::classify(e, &db_kind)))?;

                    if docs.is_empty() {
                        tx.commit()
                            .context("Commit final transaction")
                            .map_err(|e| Status::from(ServiceError::classify(e, &db_kind)))?;

                        break;
                    }

                    // Per-doc access check for this batch.
                    check_per_doc_access(
                        &docs,
                        access_owned.as_deref(),
                        user_doc,
                        &hook_runner,
                        &tx,
                        &deny_msg_owned,
                    )?;

                    let wh = RunnerWriteHooks::new(&hook_runner)
                        .with_hooks_enabled(run_hooks)
                        .with_conn(&tx);

                    let ctx = ServiceContext::collection(&collection, &def_owned)
                        .conn(&tx)
                        .write_hooks(&wh)
                        .user(user_doc)
                        .invalidation_transport(Some(invalidation_transport.clone()))
                        .build();

                    for doc in &docs {
                        match delete_document_core(&ctx, &doc.id, Some(&locale_cfg)) {
                            Ok(result) => {
                                if def_owned.soft_delete {
                                    soft_count += 1;
                                } else {
                                    hard_count += 1;
                                    if let Some(fields) = result.upload_doc_fields {
                                        upload_fields_to_clean.push(fields);
                                    }
                                }
                                deleted_ids.push(doc.id.to_string());
                            }
                            Err(ServiceError::Referenced { .. }) => {
                                skipped_count += 1;
                            }
                            Err(e) => return Err(Status::from(e.reclassify(&db_kind))),
                        }
                    }

                    tx.commit()
                        .context("Commit delete transaction")
                        .map_err(|e| Status::from(ServiceError::classify(e, &db_kind)))?;
                }

                for fields in &upload_fields_to_clean {
                    upload::delete_upload_files(&*storage, fields);
                }

                Ok((hard_count, soft_count, skipped_count, deleted_ids))
            })
            .await
            .inspect_err(|e| error!("Task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))??;

        self.publish_bulk_mutation_events(&req.collection, &deleted_ids, EventOperation::Delete);

        Ok(Response::new(content::DeleteManyResponse {
            deleted: hard_count,
            soft_deleted: soft_count,
            skipped: skipped_count,
        }))
    }
}
