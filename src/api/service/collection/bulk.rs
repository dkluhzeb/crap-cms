//! Bulk collection RPC handlers: UpdateMany, DeleteMany.

use anyhow::Context as _;
use serde_json::Value;
use tonic::{Request, Response, Status};

use crate::{
    api::{
        content,
        service::{
            ContentService,
            convert::{prost_struct_to_hashmap, prost_struct_to_json_map},
        },
    },
    core::upload,
    db::{AccessResult, DbConnection, FindQuery, LocaleContext, query},
    hooks::{HookContext, HookEvent, ValidationCtx},
    service::{self, versions},
};

use super::{filter_builder::FilterBuilder, helpers::map_db_error};

/// Safety limit for bulk operations to prevent unbounded queries.
/// Bulk ops load all matching documents into memory; this caps the maximum.
const BULK_QUERY_LIMIT: i64 = 10_000;

/// Untestable as unit: async methods require full ContentService with pool, registry,
/// hook_runner, and JWT secret. Covered by integration tests in tests/ directory.
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

        // Reject password updates in bulk operations — use single-document Update instead
        if def.is_auth_collection() && data.contains_key("password") {
            return Err(Status::invalid_argument(
                "Password updates are not supported in UpdateMany. Use Update for individual documents.",
            ));
        }
        // Defense in depth: strip password from join_data even though it shouldn't be there
        if def.is_auth_collection() {
            data.remove("password");
            join_data.remove("password");
        }

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config);
        let run_hooks = req.hooks.unwrap_or(true);

        let pool = self.pool.clone();
        let hook_runner = self.hook_runner.clone();
        let jwt_secret = self.jwt_secret.clone();
        let registry = self.registry.clone();
        let db_kind = self.db_kind.clone();
        let collection = req.collection.clone();
        let req_where = req.r#where.clone();
        let def_owned = def;
        let modified = tokio::task::spawn_blocking(move || -> Result<i64, Status> {
            let mut conn = pool.get().map_err(|e| map_db_error(e, "Pool", &db_kind))?;

            // Auth + read access (all on blocking thread)
            let auth_user =
                ContentService::resolve_auth_user(token, &jwt_secret, &registry, &conn)?;
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
                .build()?;

            let tx = conn
                .transaction_immediate()
                .context("Start transaction")
                .map_err(|e| map_db_error(e, "UpdateMany error", &db_kind))?;

            let mut find_query = FindQuery::new();
            find_query.filters = filters;
            find_query.limit = Some(BULK_QUERY_LIMIT);
            let docs = query::find(
                &tx,
                &collection,
                &def_owned,
                &find_query,
                locale_ctx.as_ref(),
            )
            .map_err(|e| map_db_error(e, "UpdateMany error", &db_kind))?;

            // All-or-nothing update access check
            if def_owned.access.update.is_some() {
                let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
                for doc in &docs {
                    let result = hook_runner
                        .check_access(
                            def_owned.access.update.as_deref(),
                            user_doc,
                            Some(&doc.id),
                            None,
                            &tx,
                        )
                        .map_err(|e| {
                            tracing::error!("Access check error: {}", e);
                            Status::internal("Internal error")
                        })?;

                    if matches!(result, AccessResult::Denied) {
                        return Err(Status::permission_denied("Update access denied"));
                    }
                }
            }

            // Strip field-level update-denied fields (same as single Update)
            let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
            let denied =
                hook_runner.check_field_write_access(&def_owned.fields, user_doc, "update", &tx);
            for name in &denied {
                data.remove(name);
                join_data.remove(name);
            }

            let mut count = 0i64;
            for doc in &docs {
                let hook_data = service::build_hook_data(&data, &join_data);

                // Run the full before-write lifecycle: BeforeValidate → validation → BeforeChange.
                // Captures modified data from hooks so it flows into the actual DB write.
                let (write_data, write_join_data) = if run_hooks {
                    let hook_ctx = HookContext::builder(&collection, "update")
                        .data(hook_data)
                        .user(user_doc)
                        .build();
                    let val_ctx = ValidationCtx::builder(&tx, &collection)
                        .exclude_id(Some(&doc.id))
                        .locale_ctx(locale_ctx.as_ref())
                        .soft_delete(def_owned.soft_delete)
                        .build();
                    let final_ctx = hook_runner
                        .run_before_write(&def_owned.hooks, &def_owned.fields, hook_ctx, &val_ctx)
                        .map_err(|e| map_db_error(e, "UpdateMany hook error", &db_kind))?;
                    let final_data = final_ctx.to_string_map(&def_owned.fields);
                    (final_data, final_ctx.data)
                } else {
                    (data.clone(), join_data.clone())
                };

                // Snapshot outgoing refs before mutation for ref count adjustment
                let locale_cfg = locale_ctx
                    .as_ref()
                    .map(|lc| lc.config.clone())
                    .unwrap_or_default();
                let old_refs = query::ref_count::snapshot_outgoing_refs(
                    &tx,
                    &collection,
                    &doc.id,
                    &def_owned.fields,
                    &locale_cfg,
                )
                .map_err(|e| map_db_error(e, "UpdateMany ref snapshot error", &db_kind))?;

                let updated = query::update_partial(
                    &tx,
                    &collection,
                    &def_owned,
                    &doc.id,
                    &write_data,
                    locale_ctx.as_ref(),
                )
                .map_err(|e| map_db_error(e, "UpdateMany error", &db_kind))?;
                query::save_join_table_data(
                    &tx,
                    &collection,
                    &def_owned.fields,
                    &doc.id,
                    &write_join_data,
                    locale_ctx.as_ref(),
                )
                .map_err(|e| map_db_error(e, "UpdateMany error", &db_kind))?;

                // Adjust ref counts based on before/after diff
                query::ref_count::after_update(
                    &tx,
                    &collection,
                    &doc.id,
                    &def_owned.fields,
                    &locale_cfg,
                    old_refs,
                )
                .map_err(|e| map_db_error(e, "UpdateMany ref count error", &db_kind))?;
                if tx.supports_fts() {
                    query::fts::fts_upsert(&tx, &collection, &updated, Some(&def_owned))
                        .map_err(|e| map_db_error(e, "UpdateMany error", &db_kind))?;
                }

                if def_owned.has_versions() {
                    let vs_ctx = versions::VersionSnapshotCtx::builder(&collection, &updated.id)
                        .fields(&def_owned.fields)
                        .versions(def_owned.versions.as_ref())
                        .has_drafts(def_owned.has_drafts())
                        .build();
                    versions::create_version_snapshot(&tx, &vs_ctx, "published", &updated)
                        .map_err(|e| map_db_error(e, "UpdateMany version error", &db_kind))?;
                }

                if run_hooks {
                    let mut after_data = updated.fields.clone();
                    after_data.insert("id".to_string(), Value::String(updated.id.to_string()));
                    let after_ctx = HookContext::builder(&collection, "update")
                        .data(after_data)
                        .user(user_doc)
                        .build();
                    hook_runner
                        .run_after_write(
                            &def_owned.hooks,
                            &def_owned.fields,
                            HookEvent::AfterChange,
                            after_ctx,
                            &tx,
                        )
                        .map_err(|e| map_db_error(e, "UpdateMany hook error", &db_kind))?;
                }

                count += 1;
            }

            tx.commit()
                .context("Commit transaction")
                .map_err(|e| map_db_error(e, "UpdateMany error", &db_kind))?;
            Ok(count)
        })
        .await
        .map_err(|e| {
            tracing::error!("Task error: {}", e);
            Status::internal("Internal error")
        })??;

        if let Some(c) = &self.populate_cache {
            c.clear();
        }

        Ok(Response::new(content::UpdateManyResponse { modified }))
    }

    /// Bulk delete matching documents. Runs per-document lifecycle hooks by default.
    ///
    /// Respects `soft_delete` collection config: soft-deletes go to trash unless
    /// `force_hard_delete` is set. Permission check uses `access.trash` for soft
    /// deletes and `access.delete` for hard deletes, matching single-delete behavior.
    pub(in crate::api::service) async fn delete_many_impl(
        &self,
        request: Request<content::DeleteManyRequest>,
    ) -> Result<Response<content::DeleteManyResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let req = request.into_inner();
        let mut def = self.get_collection_def(&req.collection)?;
        let run_hooks = req.hooks.unwrap_or(true);

        // Determine soft vs hard delete (mirrors single delete_impl logic)
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

        // Override soft_delete on the def when force_hard_delete is requested
        if req.force_hard_delete && def.soft_delete {
            def.soft_delete = false;
        }

        let pool = self.pool.clone();
        let hook_runner = self.hook_runner.clone();
        let jwt_secret = self.jwt_secret.clone();
        let registry = self.registry.clone();
        let db_kind = self.db_kind.clone();
        let collection = req.collection.clone();
        let req_where = req.r#where.clone();
        let config_dir = self.config_dir.clone();
        let locale_cfg = self.locale_config.clone();
        let access_owned = access_ref.map(|s| s.to_string());
        let deny_msg_owned = deny_msg.to_string();
        let soft_delete = will_soft_delete;
        let def_owned = def;

        let (hard_count, soft_count) =
            tokio::task::spawn_blocking(move || -> Result<(i64, i64), Status> {
                let mut conn = pool.get().map_err(|e| map_db_error(e, "Pool", &db_kind))?;

                // Auth + read access (all on blocking thread)
                let auth_user =
                    ContentService::resolve_auth_user(token, &jwt_secret, &registry, &conn)?;
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
                    .build()?;

                let tx = conn
                    .transaction_immediate()
                    .context("Start transaction")
                    .map_err(|e| map_db_error(e, "DeleteMany error", &db_kind))?;

                let mut find_query = FindQuery::new();
                find_query.filters = filters;
                find_query.limit = Some(BULK_QUERY_LIMIT);
                let docs = query::find(&tx, &collection, &def_owned, &find_query, None)
                    .map_err(|e| map_db_error(e, "DeleteMany error", &db_kind))?;

                // All-or-nothing access check (using the resolved permission)
                if access_owned.is_some() {
                    let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
                    for doc in &docs {
                        let result = hook_runner
                            .check_access(
                                access_owned.as_deref(),
                                user_doc,
                                Some(&doc.id),
                                None,
                                &tx,
                            )
                            .map_err(|e| {
                                tracing::error!("Access check error: {}", e);
                                Status::internal("Internal error")
                            })?;

                        if matches!(result, AccessResult::Denied) {
                            return Err(Status::permission_denied(deny_msg_owned));
                        }
                    }
                }

                let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
                let mut hard_count = 0i64;
                let mut soft_count = 0i64;
                let mut hard_deleted_indices = Vec::new();

                for (idx, doc) in docs.iter().enumerate() {
                    // Ref count protection only applies to hard deletes — soft-deleted
                    // docs remain referenceable (ref counts are NOT decremented).
                    if !soft_delete {
                        let ref_count = query::ref_count::get_ref_count(&tx, &collection, &doc.id)
                            .map_err(|e| map_db_error(e, "DeleteMany ref count error", &db_kind))?;
                        if ref_count > 0 {
                            continue;
                        }
                    }

                    let mut hook_data: std::collections::HashMap<String, Value> =
                        [("id".to_string(), Value::String(doc.id.to_string()))].into();
                    if soft_delete {
                        hook_data.insert("soft_delete".to_string(), Value::Bool(true));
                    }

                    if run_hooks {
                        let hook_ctx = HookContext::builder(&collection, "delete")
                            .data(hook_data.clone())
                            .user(user_doc)
                            .build();
                        hook_runner
                            .run_hooks_with_conn(
                                &def_owned.hooks,
                                HookEvent::BeforeDelete,
                                hook_ctx,
                                &tx,
                            )
                            .map_err(|e| map_db_error(e, "DeleteMany hook error", &db_kind))?;
                    }

                    if soft_delete {
                        query::soft_delete(&tx, &collection, &doc.id)
                            .map_err(|e| map_db_error(e, "DeleteMany error", &db_kind))?;
                        soft_count += 1;
                    } else {
                        // Decrement ref counts on targets before hard deleting
                        query::ref_count::before_hard_delete(
                            &tx,
                            &collection,
                            &doc.id,
                            &def_owned.fields,
                            &locale_cfg,
                        )
                        .map_err(|e| map_db_error(e, "DeleteMany ref count error", &db_kind))?;

                        query::delete(&tx, &collection, &doc.id)
                            .map_err(|e| map_db_error(e, "DeleteMany error", &db_kind))?;
                        hard_deleted_indices.push(idx);
                        hard_count += 1;
                    }

                    // Clean up FTS index in both hard-delete and soft-delete cases
                    if tx.supports_fts() {
                        query::fts::fts_delete(&tx, &collection, &doc.id)
                            .map_err(|e| map_db_error(e, "DeleteMany error", &db_kind))?;
                    }

                    if run_hooks {
                        let after_ctx = HookContext::builder(&collection, "delete")
                            .data(hook_data)
                            .user(user_doc)
                            .build();
                        hook_runner
                            .run_hooks_with_conn(
                                &def_owned.hooks,
                                HookEvent::AfterDelete,
                                after_ctx,
                                &tx,
                            )
                            .map_err(|e| map_db_error(e, "DeleteMany hook error", &db_kind))?;
                    }
                }

                tx.commit()
                    .context("Commit transaction")
                    .map_err(|e| map_db_error(e, "DeleteMany error", &db_kind))?;

                // Clean up upload files AFTER commit — only for hard-deleted docs
                // (soft-deleted docs keep their files for potential restore).
                if def_owned.is_upload_collection() {
                    for idx in &hard_deleted_indices {
                        upload::delete_upload_files(&config_dir, &docs[*idx].fields);
                    }
                }

                Ok((hard_count, soft_count))
            })
            .await
            .map_err(|e| {
                tracing::error!("Task error: {}", e);
                Status::internal("Internal error")
            })??;

        if let Some(c) = &self.populate_cache {
            c.clear();
        }

        Ok(Response::new(content::DeleteManyResponse {
            deleted: hard_count,
            soft_deleted: soft_count,
        }))
    }
}
