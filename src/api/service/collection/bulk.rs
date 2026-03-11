//! Bulk collection RPC handlers: UpdateMany, DeleteMany.

use anyhow::Context as _;
use tonic::{Request, Response, Status};

use crate::api::content;
use crate::api::service::convert::{prost_struct_to_hashmap, prost_struct_to_json_map};
use crate::api::service::ContentService;
use crate::db::query::{self, AccessResult, FindQuery, LocaleContext};

use super::filter_builder::FilterBuilder;
use super::helpers::map_db_error;

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
        let auth_user = self.extract_auth_user(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        // Check read access first (to find matching docs)
        let read_access =
            self.require_access(def.access.read.as_deref(), &auth_user, None, None)?;
        if matches!(read_access, AccessResult::Denied) {
            return Err(Status::permission_denied("Read access denied"));
        }

        let filters = FilterBuilder::new(&def.fields, &read_access)
            .where_json(req.r#where.as_deref())
            .build()?;

        let join_data = req
            .data
            .as_ref()
            .map(prost_struct_to_json_map)
            .unwrap_or_default();
        let data = req
            .data
            .map(|s| prost_struct_to_hashmap(&s))
            .unwrap_or_default();

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config);

        let pool = self.pool.clone();
        let hook_runner = self.hook_runner.clone();
        let collection = req.collection.clone();
        let def_owned = def.clone();
        let auth_user_clone = auth_user.clone();
        let modified = tokio::task::spawn_blocking(move || -> Result<i64, anyhow::Error> {
            let mut conn = pool.get().context("DB connection")?;
            let tx = conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .context("Start transaction")?;

            let mut find_query = FindQuery::new();
            find_query.filters = filters;
            let docs = query::find(
                &tx,
                &collection,
                &def_owned,
                &find_query,
                locale_ctx.as_ref(),
            )?;

            // All-or-nothing update access check
            if def_owned.access.update.is_some() {
                let user_doc = auth_user_clone.as_ref().map(|au| &au.user_doc);
                for doc in &docs {
                    let result = hook_runner.check_access(
                        def_owned.access.update.as_deref(),
                        user_doc,
                        Some(&doc.id),
                        None,
                        &tx,
                    )?;
                    if matches!(result, AccessResult::Denied) {
                        anyhow::bail!("Update access denied for document {}", doc.id);
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
                )?;
                query::save_join_table_data(
                    &tx,
                    &collection,
                    &def_owned.fields,
                    &doc.id,
                    &join_data,
                    locale_ctx.as_ref(),
                )?;
                query::fts::fts_upsert(&tx, &collection, &updated, Some(&def_owned))?;
                count += 1;
            }

            tx.commit().context("Commit transaction")?;
            Ok(count)
        })
        .await
        .map_err(|e| {
            tracing::error!("Task error: {}", e);
            Status::internal("Internal error")
        })?
        .map_err(|e| {
            if e.to_string().contains("access denied") {
                tracing::warn!("UpdateMany access denied: {}", e);
                Status::permission_denied("Update access denied")
            } else {
                map_db_error(e, "UpdateMany error")
            }
        })?;

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
        let auth_user = self.extract_auth_user(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        // Check read access first (to find matching docs)
        let read_access =
            self.require_access(def.access.read.as_deref(), &auth_user, None, None)?;
        if matches!(read_access, AccessResult::Denied) {
            return Err(Status::permission_denied("Read access denied"));
        }

        let filters = FilterBuilder::new(&def.fields, &read_access)
            .where_json(req.r#where.as_deref())
            .build()?;

        let pool = self.pool.clone();
        let hook_runner = self.hook_runner.clone();
        let collection = req.collection.clone();
        let def_owned = def.clone();
        let auth_user_clone = auth_user.clone();
        let deleted = tokio::task::spawn_blocking(move || -> Result<i64, anyhow::Error> {
            let mut conn = pool.get().context("DB connection")?;
            let tx = conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .context("Start transaction")?;

            let mut find_query = FindQuery::new();
            find_query.filters = filters;
            let docs = query::find(&tx, &collection, &def_owned, &find_query, None)?;

            // All-or-nothing delete access check
            if def_owned.access.delete.is_some() {
                let user_doc = auth_user_clone.as_ref().map(|au| &au.user_doc);
                for doc in &docs {
                    let result = hook_runner.check_access(
                        def_owned.access.delete.as_deref(),
                        user_doc,
                        Some(&doc.id),
                        None,
                        &tx,
                    )?;
                    if matches!(result, AccessResult::Denied) {
                        anyhow::bail!("Delete access denied for document {}", doc.id);
                    }
                }
            }

            let mut count = 0i64;
            for doc in &docs {
                query::delete(&tx, &collection, &doc.id)?;
                query::fts::fts_delete(&tx, &collection, &doc.id)?;
                count += 1;
            }

            tx.commit().context("Commit transaction")?;
            Ok(count)
        })
        .await
        .map_err(|e| {
            tracing::error!("Task error: {}", e);
            Status::internal("Internal error")
        })?
        .map_err(|e| {
            if e.to_string().contains("access denied") {
                tracing::warn!("DeleteMany access denied: {}", e);
                Status::permission_denied("Delete access denied")
            } else {
                map_db_error(e, "DeleteMany error")
            }
        })?;

        if let Some(c) = &self.populate_cache {
            c.clear();
        }

        Ok(Response::new(content::DeleteManyResponse { deleted }))
    }
}
