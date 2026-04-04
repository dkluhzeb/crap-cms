//! Bulk DeleteMany RPC handler.

use std::collections::HashMap;

use anyhow::Context as _;
use serde_json::Value;
use tokio::task;
use tonic::{Request, Response, Status};
use tracing::{error, warn};

use crate::{
    api::{content, service::ContentService},
    core::{Document, collection::CollectionDefinition, event::EventOperation, upload},
    db::{AccessResult, DbConnection, query},
    hooks::{HookContext, HookEvent, HookRunner},
};

use super::helpers::{check_per_doc_access, find_matching_docs, map_db_error, publish_bulk_events};
use crate::api::service::collection::filter_builder::FilterBuilder;

/// Shared context for per-document delete processing.
struct DeleteDocCtx<'a> {
    tx: &'a dyn DbConnection,
    collection: &'a str,
    def: &'a CollectionDefinition,
    soft_delete: bool,
    hook_runner: &'a HookRunner,
    user_doc: Option<&'a Document>,
    run_hooks: bool,
    locale_cfg: &'a crate::config::LocaleConfig,
    db_kind: &'a str,
}

/// Process a single document deletion within a bulk transaction.
/// Returns `(hard_deleted, soft_deleted, skipped)` — exactly one will be 1.
fn delete_single_doc(ctx: &DeleteDocCtx, doc: &Document) -> Result<(i64, i64, i64), Status> {
    if !ctx.soft_delete {
        let ref_count = query::ref_count::get_ref_count(ctx.tx, ctx.collection, &doc.id)
            .map_err(|e| map_db_error(e, "DeleteMany ref count error", ctx.db_kind))?
            .unwrap_or(0);

        if ref_count > 0 {
            return Ok((0, 0, 1));
        }
    }

    let mut hook_data: HashMap<String, Value> =
        [("id".to_string(), Value::String(doc.id.to_string()))].into();

    if ctx.soft_delete {
        hook_data.insert("soft_delete".to_string(), Value::Bool(true));
    }

    if ctx.run_hooks {
        run_delete_hook(ctx, &hook_data, HookEvent::BeforeDelete)?;
    }

    let (hard, soft) = if ctx.soft_delete {
        query::soft_delete(ctx.tx, ctx.collection, &doc.id)
            .map_err(|e| map_db_error(e, "DeleteMany error", ctx.db_kind))?;
        (0, 1)
    } else {
        query::ref_count::before_hard_delete(
            ctx.tx,
            ctx.collection,
            &doc.id,
            &ctx.def.fields,
            ctx.locale_cfg,
        )
        .map_err(|e| map_db_error(e, "DeleteMany ref count error", ctx.db_kind))?;

        query::delete(ctx.tx, ctx.collection, &doc.id)
            .map_err(|e| map_db_error(e, "DeleteMany error", ctx.db_kind))?;
        (1, 0)
    };

    if ctx.tx.supports_fts() {
        query::fts::fts_delete(ctx.tx, ctx.collection, &doc.id)
            .map_err(|e| map_db_error(e, "DeleteMany error", ctx.db_kind))?;
    }

    if ctx.def.is_upload_collection() {
        let _ = query::images::delete_entries_for_document(ctx.tx, ctx.collection, &doc.id);
    }

    if ctx.run_hooks {
        run_delete_hook(ctx, &hook_data, HookEvent::AfterDelete)?;
    }

    Ok((hard, soft, 0))
}

/// Run a delete lifecycle hook (BeforeDelete or AfterDelete).
fn run_delete_hook(
    ctx: &DeleteDocCtx,
    hook_data: &HashMap<String, Value>,
    event: HookEvent,
) -> Result<(), Status> {
    let hook_ctx = HookContext::builder(ctx.collection, "delete")
        .data(hook_data.clone())
        .user(ctx.user_doc)
        .build();

    ctx.hook_runner
        .run_hooks_with_conn(&ctx.def.hooks, event, hook_ctx, ctx.tx)
        .map_err(|e| map_db_error(e, "DeleteMany hook error", ctx.db_kind))?;

    Ok(())
}

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Bulk delete matching documents. Runs per-document lifecycle hooks by default.
    pub(in crate::api::service) async fn delete_many_impl(
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
        let soft_delete = will_soft_delete;
        let def_owned = def;

        let (hard_count, soft_count, skipped_count, deleted_ids) =
            task::spawn_blocking(move || -> Result<(i64, i64, i64, Vec<String>), Status> {
                let mut conn = pool.get().map_err(|e| map_db_error(e, "Pool", &db_kind))?;

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

                let filters = FilterBuilder::new(&def_owned.fields, &read_access)
                    .where_json(req_where.as_deref())
                    .draft_filter(def_owned.has_drafts(), true)
                    .build()?;

                let tx = conn
                    .transaction_immediate()
                    .context("Start transaction")
                    .map_err(|e| map_db_error(e, "DeleteMany error", &db_kind))?;

                let docs =
                    find_matching_docs(&tx, &collection, &def_owned, filters, None, &db_kind)?;

                let user_doc = auth_user.as_ref().map(|au| &au.user_doc);

                check_per_doc_access(
                    &docs,
                    access_owned.as_deref(),
                    user_doc,
                    &hook_runner,
                    &tx,
                    &deny_msg_owned,
                )?;

                let mut hard_count = 0i64;
                let mut soft_count = 0i64;
                let mut skipped_count = 0i64;
                let mut hard_deleted_indices = Vec::new();
                let mut deleted_ids = Vec::new();

                let delete_ctx = DeleteDocCtx {
                    tx: &tx,
                    collection: &collection,
                    def: &def_owned,
                    soft_delete,
                    hook_runner: &hook_runner,
                    user_doc,
                    run_hooks,
                    locale_cfg: &locale_cfg,
                    db_kind: &db_kind,
                };

                for (idx, doc) in docs.iter().enumerate() {
                    let (h, s, sk) = delete_single_doc(&delete_ctx, doc)?;

                    hard_count += h;
                    soft_count += s;
                    skipped_count += sk;

                    if sk == 0 {
                        deleted_ids.push(doc.id.to_string());
                    }

                    if h > 0 {
                        hard_deleted_indices.push(idx);
                    }
                }

                tx.commit()
                    .context("Commit transaction")
                    .map_err(|e| map_db_error(e, "DeleteMany error", &db_kind))?;

                if def_owned.is_upload_collection() {
                    for idx in &hard_deleted_indices {
                        upload::delete_upload_files(&*storage, &docs[*idx].fields);
                    }
                }

                Ok((hard_count, soft_count, skipped_count, deleted_ids))
            })
            .await
            .map_err(|e| {
                error!("Task error: {}", e);
                Status::internal("Internal error")
            })??;

        if let Err(e) = self.cache.clear() {
            warn!("Cache clear failed: {:#}", e);
        }

        publish_bulk_events(self, &req.collection, &deleted_ids, EventOperation::Delete);

        Ok(Response::new(content::DeleteManyResponse {
            deleted: hard_count,
            soft_deleted: soft_count,
            skipped: skipped_count,
        }))
    }
}
