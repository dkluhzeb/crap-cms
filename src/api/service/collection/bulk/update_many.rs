//! Bulk UpdateMany RPC handler.

use std::collections::HashMap;

use anyhow::Context as _;
use serde_json::Value;
use tokio::task;
use tonic::{Request, Response, Status};
use tracing::{error, warn};

use crate::{
    api::{
        content,
        service::{
            ContentService,
            convert::{prost_struct_to_hashmap, prost_struct_to_json_map},
        },
    },
    core::{Document, collection::CollectionDefinition},
    db::{AccessResult, DbConnection, LocaleContext},
    hooks::{HookContext, HookEvent, HookRunner, ValidationCtx},
    service,
};

use super::helpers::{check_per_doc_access, find_matching_docs, map_db_error, publish_bulk_events};
use crate::api::service::collection::filter_builder::FilterBuilder;
use crate::core::event::EventOperation;

/// Shared context for per-document update processing.
struct UpdateDocCtx<'a> {
    tx: &'a dyn DbConnection,
    collection: &'a str,
    def: &'a CollectionDefinition,
    data: &'a HashMap<String, String>,
    join_data: &'a HashMap<String, Value>,
    locale_ctx: Option<&'a LocaleContext>,
    hook_runner: &'a HookRunner,
    user_doc: Option<&'a Document>,
    run_hooks: bool,
    draft: bool,
    db_kind: &'a str,
}

/// Process a single document update within a bulk transaction.
fn update_single_doc(ctx: &UpdateDocCtx, doc: &Document) -> Result<Document, Status> {
    let (write_data, write_join_data) = if ctx.run_hooks {
        run_update_hooks(ctx, doc)?
    } else {
        (ctx.data.clone(), ctx.join_data.clone())
    };

    let locale_cfg = ctx
        .locale_ctx
        .map(|lc| lc.config.clone())
        .unwrap_or_default();

    let updated = service::persist_bulk_update(
        ctx.tx,
        ctx.collection,
        &doc.id,
        ctx.def,
        &write_data,
        &write_join_data,
        ctx.locale_ctx,
        &locale_cfg,
    )
    .map_err(|e| map_db_error(e, "UpdateMany error", ctx.db_kind))?;

    if ctx.run_hooks {
        run_after_update_hook(ctx, &updated)?;
    }

    Ok(updated)
}

/// Write data after hook processing: string map for columns + JSON map for joins.
type HookWriteData = (HashMap<String, String>, HashMap<String, Value>);

/// Run before-write lifecycle hooks for a bulk update document.
fn run_update_hooks(ctx: &UpdateDocCtx, doc: &Document) -> Result<HookWriteData, Status> {
    let hook_data = service::build_hook_data(ctx.data, ctx.join_data);

    let hook_ctx = HookContext::builder(ctx.collection, "update")
        .data(hook_data)
        .user(ctx.user_doc)
        .build();

    let val_ctx = ValidationCtx::builder(ctx.tx, ctx.collection)
        .exclude_id(Some(&doc.id))
        .locale_ctx(ctx.locale_ctx)
        .soft_delete(ctx.def.soft_delete)
        .draft(ctx.draft)
        .build();

    let final_ctx = ctx
        .hook_runner
        .run_before_write(&ctx.def.hooks, &ctx.def.fields, hook_ctx, &val_ctx)
        .map_err(|e| map_db_error(e, "UpdateMany hook error", ctx.db_kind))?;

    let final_data = final_ctx.to_string_map(&ctx.def.fields);

    Ok((final_data, final_ctx.data))
}

/// Run after-change hook for a single updated document.
fn run_after_update_hook(ctx: &UpdateDocCtx, updated: &Document) -> Result<(), Status> {
    let mut after_data = updated.fields.clone();
    after_data.insert("id".to_string(), Value::String(updated.id.to_string()));

    let after_ctx = HookContext::builder(ctx.collection, "update")
        .data(after_data)
        .user(ctx.user_doc)
        .build();

    ctx.hook_runner
        .run_after_write(
            &ctx.def.hooks,
            &ctx.def.fields,
            HookEvent::AfterChange,
            after_ctx,
            ctx.tx,
        )
        .map_err(|e| map_db_error(e, "UpdateMany hook error", ctx.db_kind))?;

    Ok(())
}

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Bulk update matching documents. Runs per-document lifecycle hooks by default.
    pub(in crate::api::service) async fn update_many_impl(
        &self,
        request: Request<content::UpdateManyRequest>,
    ) -> Result<Response<content::UpdateManyResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        let mut join_data = req
            .data
            .as_ref()
            .map(prost_struct_to_json_map)
            .unwrap_or_default();

        let mut data = req
            .data
            .map(|s| prost_struct_to_hashmap(&s))
            .unwrap_or_default();

        if def.is_auth_collection() && data.contains_key("password") {
            return Err(Status::invalid_argument(
                "Password updates are not supported in UpdateMany. Use Update for individual documents.",
            ));
        }

        if def.is_auth_collection() {
            data.remove("password");
            join_data.remove("password");
        }

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config);
        let run_hooks = req.hooks.unwrap_or(true);
        let draft = req.draft.unwrap_or(false);

        let pool = self.pool.clone();
        let hook_runner = self.hook_runner.clone();
        let token_provider = self.token_provider.clone();
        let registry = self.registry.clone();
        let db_kind = self.db_kind.clone();
        let collection = req.collection.clone();
        let req_where = req.r#where.clone();
        let def_owned = def;

        let (modified, updated_ids) =
            task::spawn_blocking(move || -> Result<(i64, Vec<String>), Status> {
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
                    .draft_filter(def_owned.has_drafts(), !draft)
                    .build()?;

                let tx = conn
                    .transaction_immediate()
                    .context("Start transaction")
                    .map_err(|e| map_db_error(e, "UpdateMany error", &db_kind))?;

                let docs = find_matching_docs(
                    &tx,
                    &collection,
                    &def_owned,
                    filters,
                    locale_ctx.as_ref(),
                    &db_kind,
                )?;

                let user_doc = auth_user.as_ref().map(|au| &au.user_doc);

                check_per_doc_access(
                    &docs,
                    def_owned.access.update.as_deref(),
                    user_doc,
                    &hook_runner,
                    &tx,
                    "Update access denied",
                )?;

                let denied = hook_runner.check_field_write_access(
                    &def_owned.fields,
                    user_doc,
                    "update",
                    &tx,
                );

                for name in &denied {
                    data.remove(name);
                    join_data.remove(name);
                }

                let update_ctx = UpdateDocCtx {
                    tx: &tx,
                    collection: &collection,
                    def: &def_owned,
                    data: &data,
                    join_data: &join_data,
                    locale_ctx: locale_ctx.as_ref(),
                    hook_runner: &hook_runner,
                    user_doc,
                    run_hooks,
                    draft,
                    db_kind: &db_kind,
                };

                let mut count = 0i64;
                let mut ids = Vec::new();

                for doc in &docs {
                    update_single_doc(&update_ctx, doc)?;

                    ids.push(doc.id.to_string());
                    count += 1;
                }

                tx.commit()
                    .context("Commit transaction")
                    .map_err(|e| map_db_error(e, "UpdateMany error", &db_kind))?;

                Ok((count, ids))
            })
            .await
            .inspect_err(|e| error!("Task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))??;

        if let Err(e) = self.cache.clear() {
            warn!("Cache clear failed: {:#}", e);
        }

        publish_bulk_events(self, &req.collection, &updated_ids, EventOperation::Update);

        Ok(Response::new(content::UpdateManyResponse { modified }))
    }
}
