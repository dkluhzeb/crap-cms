//! Write-oriented collection RPC handlers: Create, Update, Delete.

use anyhow::Context as _;
use std::collections::HashMap;
use tonic::{Request, Response, Status};

use crate::api::content;
use crate::api::service::ContentService;
use crate::api::service::convert::{
    document_to_proto, prost_struct_to_hashmap, prost_struct_to_json_map,
};
use crate::db::query::{AccessResult, LocaleContext};

use super::helpers::map_db_error;

/// Untestable as unit: async methods require full ContentService with pool, registry,
/// hook_runner, and JWT secret. Covered by integration tests in tests/ directory.
#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Create a new document, running before/after hooks within a transaction.
    pub(in crate::api::service) async fn create_impl(
        &self,
        request: Request<content::CreateRequest>,
    ) -> Result<Response<content::CreateResponse>, Status> {
        let metadata = request.metadata().clone();
        let auth_user = self.extract_auth_user(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        // Check create access
        let access_result =
            self.require_access(def.access.create.as_deref(), &auth_user, None, None)?;
        if matches!(access_result, AccessResult::Denied) {
            return Err(Status::permission_denied("Create access denied"));
        }

        // Extract join table data (preserves structured arrays/objects)
        let join_data = req
            .data
            .as_ref()
            .map(prost_struct_to_json_map)
            .unwrap_or_default();

        let mut data = req
            .data
            .map(|s| prost_struct_to_hashmap(&s))
            .unwrap_or_default();

        // Extract password for auth collections
        let password = if def.is_auth_collection() {
            data.remove("password")
        } else {
            None
        };

        // Validate password against policy
        if let Some(ref pw) = password {
            if let Err(e) = self.password_policy.validate(pw) {
                return Err(Status::invalid_argument(e.to_string()));
            }
        }

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config);

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let collection = req.collection.clone();
        let def_fields = def.fields.clone();
        let def_owned = def;
        let user_doc = auth_user.as_ref().map(|au| au.user_doc.clone());
        let auth_user_ui_locale = auth_user.as_ref().map(|au| au.ui_locale.clone());
        let (doc, _req_context) = tokio::task::spawn_blocking(move || {
            // Strip field-level create-denied fields inside spawn_blocking
            // to avoid pool.get() on the async thread
            {
                let mut conn = pool.get().context("DB connection for field access")?;
                let tx = conn.transaction().context("Transaction for field access")?;
                let denied = runner.check_field_write_access(
                    &def_owned.fields,
                    user_doc.as_ref(),
                    "create",
                    &tx,
                );
                tx.commit().context("Commit field access transaction")?;
                for name in &denied {
                    data.remove(name);
                }
            }
            let ui_locale = user_doc.as_ref().and_then(|_| auth_user_ui_locale.clone());
            crate::service::create_document(
                &pool,
                &runner,
                &collection,
                &def_owned,
                crate::service::WriteInput {
                    data,
                    join_data: &join_data,
                    password: password.as_deref(),
                    locale_ctx: locale_ctx.as_ref(),
                    locale: None,
                    draft: req.draft.unwrap_or(false),
                    ui_locale,
                },
                user_doc.as_ref(),
            )
        })
        .await
        .map_err(|e| {
            tracing::error!("Task error: {}", e);
            Status::internal("Internal error")
        })?
        .map_err(|e| map_db_error(e, "Create error"))?;

        if let Some(c) = &self.populate_cache {
            c.clear();
        }

        {
            let def = self.get_collection_def(&req.collection);
            let (hooks, should_verify, live) = match &def {
                Ok(d) => (
                    d.hooks.clone(),
                    d.is_auth_collection() && d.auth.as_ref().is_some_and(|a| a.verify_email),
                    d.live.clone(),
                ),
                Err(_) => (Default::default(), false, None),
            };
            self.hook_runner.publish_event(
                &self.event_bus,
                &hooks,
                live.as_ref(),
                crate::core::event::EventTarget::Collection,
                crate::core::event::EventOperation::Create,
                req.collection.clone(),
                doc.id.clone(),
                doc.fields.clone(),
                Self::event_user_from(&auth_user),
            );

            // Auto-send verification email for auth collections with verify_email
            if should_verify {
                if let Some(user_email) = doc.fields.get("email").and_then(|v| v.as_str()) {
                    crate::service::send_verification_email(
                        self.pool.clone(),
                        self.email_config.clone(),
                        self.email_renderer.clone(),
                        self.server_config.clone(),
                        req.collection.clone(),
                        doc.id.clone(),
                        user_email.to_string(),
                    );
                }
            }
        }

        let mut proto_doc = document_to_proto(&doc, &req.collection);
        self.strip_denied_read_fields(&mut proto_doc, &def_fields, &auth_user);

        Ok(Response::new(content::CreateResponse {
            document: Some(proto_doc),
        }))
    }

    /// Update an existing document by ID, running before/after hooks within a transaction.
    pub(in crate::api::service) async fn update_impl(
        &self,
        request: Request<content::UpdateRequest>,
    ) -> Result<Response<content::UpdateResponse>, Status> {
        let metadata = request.metadata().clone();
        let auth_user = self.extract_auth_user(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        // Check update access
        let access_result = self.require_access(
            def.access.update.as_deref(),
            &auth_user,
            Some(&req.id),
            None,
        )?;
        if matches!(access_result, AccessResult::Denied) {
            return Err(Status::permission_denied("Update access denied"));
        }

        // Extract join table data (preserves structured arrays/objects)
        let join_data = req
            .data
            .as_ref()
            .map(prost_struct_to_json_map)
            .unwrap_or_default();

        let mut data = req
            .data
            .map(|s| prost_struct_to_hashmap(&s))
            .unwrap_or_default();

        let password = if def.is_auth_collection() {
            data.remove("password")
        } else {
            None
        };

        // Validate password against policy (only if non-empty)
        if let Some(ref pw) = password {
            if !pw.is_empty() {
                if let Err(e) = self.password_policy.validate(pw) {
                    return Err(Status::invalid_argument(e.to_string()));
                }
            }
        }

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config);

        // Handle unpublish: set status to draft, create version, return
        if req.unpublish.unwrap_or(false) && def.has_versions() {
            let pool = self.pool.clone();
            let runner = self.hook_runner.clone();
            let collection = req.collection.clone();
            let id = req.id.clone();
            let def_fields = def.fields.clone();
            let def_owned = def;
            let user_doc = auth_user.as_ref().map(|au| au.user_doc.clone());
            let doc = tokio::task::spawn_blocking(move || {
                crate::service::unpublish_document(
                    &pool,
                    &runner,
                    &collection,
                    &id,
                    &def_owned,
                    user_doc.as_ref(),
                )
            })
            .await
            .map_err(|e| {
                tracing::error!("Task error: {}", e);
                Status::internal("Internal error")
            })?
            .map_err(|e| map_db_error(e, "Unpublish error"))?;

            if let Some(c) = &self.populate_cache {
                c.clear();
            }

            self.hook_runner.publish_event(
                &self.event_bus,
                &self
                    .get_collection_def(&req.collection)
                    .map(|d| d.hooks.clone())
                    .unwrap_or_default(),
                self.get_collection_def(&req.collection)
                    .ok()
                    .and_then(|d| d.live.clone())
                    .as_ref(),
                crate::core::event::EventTarget::Collection,
                crate::core::event::EventOperation::Update,
                req.collection.clone(),
                req.id.clone(),
                doc.fields.clone(),
                Self::event_user_from(&auth_user),
            );

            let mut proto_doc = document_to_proto(&doc, &req.collection);
            self.strip_denied_read_fields(&mut proto_doc, &def_fields, &auth_user);

            return Ok(Response::new(content::UpdateResponse {
                document: Some(proto_doc),
            }));
        }

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let collection = req.collection.clone();
        let id = req.id.clone();
        let def_fields = def.fields.clone();
        let def_owned = def;
        let user_doc = auth_user.as_ref().map(|au| au.user_doc.clone());
        let auth_user_ui_locale = auth_user.as_ref().map(|au| au.ui_locale.clone());
        let (doc, _req_context) = tokio::task::spawn_blocking(move || {
            // Strip field-level update-denied fields inside spawn_blocking
            // to avoid pool.get() on the async thread
            {
                let mut conn = pool.get().context("DB connection for field access")?;
                let tx = conn.transaction().context("Transaction for field access")?;
                let denied = runner.check_field_write_access(
                    &def_owned.fields,
                    user_doc.as_ref(),
                    "update",
                    &tx,
                );
                tx.commit().context("Commit field access transaction")?;
                for name in &denied {
                    data.remove(name);
                }
            }
            let ui_locale = user_doc.as_ref().and_then(|_| auth_user_ui_locale.clone());
            crate::service::update_document(
                &pool,
                &runner,
                &collection,
                &id,
                &def_owned,
                crate::service::WriteInput {
                    data,
                    join_data: &join_data,
                    password: password.as_deref(),
                    locale_ctx: locale_ctx.as_ref(),
                    locale: None,
                    draft: req.draft.unwrap_or(false),
                    ui_locale,
                },
                user_doc.as_ref(),
            )
        })
        .await
        .map_err(|e| {
            tracing::error!("Task error: {}", e);
            Status::internal("Internal error")
        })?
        .map_err(|e| map_db_error(e, "Update error"))?;

        if let Some(c) = &self.populate_cache {
            c.clear();
        }

        {
            let def = self.get_collection_def(&req.collection);
            let (hooks, live) = match &def {
                Ok(d) => (d.hooks.clone(), d.live.clone()),
                Err(_) => (Default::default(), None),
            };
            self.hook_runner.publish_event(
                &self.event_bus,
                &hooks,
                live.as_ref(),
                crate::core::event::EventTarget::Collection,
                crate::core::event::EventOperation::Update,
                req.collection.clone(),
                req.id.clone(),
                doc.fields.clone(),
                Self::event_user_from(&auth_user),
            );
        }

        let mut proto_doc = document_to_proto(&doc, &req.collection);
        self.strip_denied_read_fields(&mut proto_doc, &def_fields, &auth_user);

        Ok(Response::new(content::UpdateResponse {
            document: Some(proto_doc),
        }))
    }

    /// Delete a document by ID, running before/after delete hooks.
    pub(in crate::api::service) async fn delete_impl(
        &self,
        request: Request<content::DeleteRequest>,
    ) -> Result<Response<content::DeleteResponse>, Status> {
        let metadata = request.metadata().clone();
        let auth_user = self.extract_auth_user(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        // Check delete access
        let access_result = self.require_access(
            def.access.delete.as_deref(),
            &auth_user,
            Some(&req.id),
            None,
        )?;
        if matches!(access_result, AccessResult::Denied) {
            return Err(Status::permission_denied("Delete access denied"));
        }

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let def_clone = def.clone();
        let collection = req.collection.clone();
        let id = req.id.clone();
        let user_doc = auth_user.as_ref().map(|au| au.user_doc.clone());
        let config_dir = self.config_dir.clone();
        let _req_context = tokio::task::spawn_blocking(move || {
            crate::service::delete_document(
                &pool,
                &runner,
                &collection,
                &id,
                &def_clone,
                user_doc.as_ref(),
                Some(&config_dir),
            )
        })
        .await
        .map_err(|e| {
            tracing::error!("Task error: {}", e);
            Status::internal("Internal error")
        })?
        .map_err(|e| map_db_error(e, "Delete error"))?;

        if let Some(c) = &self.populate_cache {
            c.clear();
        }

        self.hook_runner.publish_event(
            &self.event_bus,
            &def.hooks,
            def.live.as_ref(),
            crate::core::event::EventTarget::Collection,
            crate::core::event::EventOperation::Delete,
            req.collection.clone(),
            req.id.clone(),
            HashMap::new(),
            Self::event_user_from(&auth_user),
        );

        Ok(Response::new(content::DeleteResponse { success: true }))
    }
}
