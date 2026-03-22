//! Bulk collection RPC handlers: UpdateMany, DeleteMany.

use anyhow::Context as _;
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
};

use super::{filter_builder::FilterBuilder, helpers::map_db_error};

/// Untestable as unit: async methods require full ContentService with pool, registry,
/// hook_runner, and JWT secret. Covered by integration tests in tests/ directory.
#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Bulk update matching documents (all-or-nothing access check, no per-document hooks).
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

            let mut count = 0i64;
            for doc in &docs {
                let updated = query::update(
                    &tx,
                    &collection,
                    &def_owned,
                    &doc.id,
                    &data,
                    locale_ctx.as_ref(),
                )
                .map_err(|e| map_db_error(e, "UpdateMany error", &db_kind))?;
                query::save_join_table_data(
                    &tx,
                    &collection,
                    &def_owned.fields,
                    &doc.id,
                    &join_data,
                    locale_ctx.as_ref(),
                )
                .map_err(|e| map_db_error(e, "UpdateMany error", &db_kind))?;
                if tx.supports_fts() {
                    query::fts::fts_upsert(&tx, &collection, &updated, Some(&def_owned))
                        .map_err(|e| map_db_error(e, "UpdateMany error", &db_kind))?;
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

    /// Bulk delete matching documents (all-or-nothing access check, no per-document hooks).
    pub(in crate::api::service) async fn delete_many_impl(
        &self,
        request: Request<content::DeleteManyRequest>,
    ) -> Result<Response<content::DeleteManyResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        let pool = self.pool.clone();
        let hook_runner = self.hook_runner.clone();
        let jwt_secret = self.jwt_secret.clone();
        let registry = self.registry.clone();
        let db_kind = self.db_kind.clone();
        let collection = req.collection.clone();
        let req_where = req.r#where.clone();
        let config_dir = self.config_dir.clone();
        let def_owned = def;
        let deleted = tokio::task::spawn_blocking(move || -> Result<i64, Status> {
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
            let docs = query::find(&tx, &collection, &def_owned, &find_query, None)
                .map_err(|e| map_db_error(e, "DeleteMany error", &db_kind))?;

            // All-or-nothing delete access check
            if def_owned.access.delete.is_some() {
                let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
                for doc in &docs {
                    let result = hook_runner
                        .check_access(
                            def_owned.access.delete.as_deref(),
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
                        return Err(Status::permission_denied("Delete access denied"));
                    }
                }
            }

            let mut count = 0i64;
            for doc in &docs {
                query::delete(&tx, &collection, &doc.id)
                    .map_err(|e| map_db_error(e, "DeleteMany error", &db_kind))?;
                if tx.supports_fts() {
                    query::fts::fts_delete(&tx, &collection, &doc.id)
                        .map_err(|e| map_db_error(e, "DeleteMany error", &db_kind))?;
                }
                count += 1;
            }

            tx.commit()
                .context("Commit transaction")
                .map_err(|e| map_db_error(e, "DeleteMany error", &db_kind))?;

            // Clean up upload files for deleted documents
            if def_owned.is_upload_collection() {
                for doc in &docs {
                    upload::delete_upload_files(&config_dir, &doc.fields);
                }
            }

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

        Ok(Response::new(content::DeleteManyResponse { deleted }))
    }
}
