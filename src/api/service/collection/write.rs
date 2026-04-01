//! Write-oriented collection RPC handlers: Create, Update, Delete, Restore.

use anyhow::Context as _;
use tonic::{Request, Response, Status};

use crate::{
    api::{
        content,
        service::{
            ContentService,
            convert::{document_to_proto, prost_struct_to_hashmap, prost_struct_to_json_map},
        },
    },
    core::event::{EventOperation, EventTarget},
    db::{AccessResult, LocaleContext},
    hooks::lifecycle::PublishEventInput,
    service::{self, WriteInput},
};

use super::helpers::{extract_auth_password, map_db_error, strip_denied_proto_fields};

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
        let token = Self::extract_token(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        // Extract join table data (preserves structured arrays/objects)
        let mut join_data = req
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
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config);

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let jwt_secret = self.jwt_secret.clone();
        let registry = self.registry.clone();
        let db_kind = self.db_kind.clone();
        let collection = req.collection.clone();
        let def_fields = def.fields.clone();
        let def_owned = def;
        let (proto_doc, auth_user) = tokio::task::spawn_blocking(move || -> Result<_, Status> {
            let mut conn = pool.get().map_err(|e| map_db_error(e, "Pool", &db_kind))?;

            // Auth + access (all on blocking thread)
            let auth_user =
                ContentService::resolve_auth_user(token, &jwt_secret, &registry, &conn)?;
            let access_result = ContentService::check_access_blocking(
                def_owned.access.create.as_deref(),
                &auth_user,
                None,
                None,
                &runner,
                &mut conn,
            )?;

            if matches!(access_result, AccessResult::Denied) {
                return Err(Status::permission_denied("Create access denied"));
            }

            // Strip field-level create-denied fields
            {
                let tx = conn
                    .transaction()
                    .context("Transaction for field access")
                    .map_err(|e| {
                        tracing::error!("Field access tx error: {}", e);
                        Status::internal("Internal error")
                    })?;
                let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
                let denied =
                    runner.check_field_write_access(&def_owned.fields, user_doc, "create", &tx);
                tx.commit()
                    .context("Commit field access transaction")
                    .map_err(|e| {
                        tracing::error!("Field access commit error: {}", e);
                        Status::internal("Internal error")
                    })?;
                for name in &denied {
                    data.remove(name);
                    join_data.remove(name);
                }
            }

            let user_doc = auth_user.as_ref().map(|au| au.user_doc.clone());
            let auth_user_ui_locale = auth_user.as_ref().map(|au| au.ui_locale.clone());
            let ui_locale = user_doc.as_ref().and_then(|_| auth_user_ui_locale.clone());

            let (doc, _req_context) = service::create_document_with_conn(
                &mut conn,
                &runner,
                &collection,
                &def_owned,
                WriteInput::builder(data, &join_data)
                    .password(password.as_deref())
                    .locale_ctx(locale_ctx.as_ref())
                    .draft(req.draft.unwrap_or(false))
                    .ui_locale(ui_locale)
                    .build(),
                user_doc.as_ref(),
            )
            .map_err(|e| map_db_error(e, "Create error", &db_kind))?;

            // Proto conversion + field stripping
            let mut proto_doc = document_to_proto(&doc, &collection);
            let user_doc_ref = auth_user.as_ref().map(|au| &au.user_doc);
            let mut conn = pool.get().map_err(|e| map_db_error(e, "Pool", &db_kind))?;
            let tx = conn.transaction().map_err(|e| {
                tracing::error!("Field read access tx error: {}", e);
                Status::internal("Internal error")
            })?;
            let denied = runner.check_field_read_access(&def_fields, user_doc_ref, &tx);
            tx.commit().map_err(|e| {
                tracing::error!("Field read access tx commit failed: {e}");
                Status::internal("Internal error")
            })?;
            strip_denied_proto_fields(&mut proto_doc, &denied);

            Ok((proto_doc, auth_user))
        })
        .await
        .map_err(|e| {
            tracing::error!("Task error: {}", e);
            Status::internal("Internal error")
        })??;

        if let Err(e) = self.cache.clear() {
            tracing::warn!("Cache clear failed: {:#}", e);
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
            // Extract doc ID from the proto_doc before publishing
            let doc_id = proto_doc.id.clone();
            let doc_fields = proto_doc
                .fields
                .as_ref()
                .map(|s| {
                    s.fields
                        .iter()
                        .map(|(k, v)| {
                            (
                                k.clone(),
                                crate::api::service::convert::prost_value_to_json(v),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            self.hook_runner.publish_event(
                &self.event_bus,
                &hooks,
                live.as_ref(),
                PublishEventInput::builder(EventTarget::Collection, EventOperation::Create)
                    .collection(req.collection.clone())
                    .document_id(doc_id.clone())
                    .data(doc_fields)
                    .edited_by(Self::event_user_from(&auth_user))
                    .build(),
            );

            // Auto-send verification email for auth collections with verify_email
            if should_verify {
                let email_val = proto_doc
                    .fields
                    .as_ref()
                    .and_then(|s| s.fields.get("email"))
                    .and_then(|v| {
                        if let Some(prost_types::value::Kind::StringValue(s)) = &v.kind {
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
                        req.collection.clone(),
                        doc_id.to_string(),
                        user_email,
                    );
                }
            }
        }

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
        let token = Self::extract_token(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        // Extract join table data (preserves structured arrays/objects)
        let mut join_data = req
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
            true,
        )?;

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config);

        // Handle unpublish: set status to draft, create version, return
        if req.unpublish.unwrap_or(false) && def.has_versions() {
            let pool = self.pool.clone();
            let runner = self.hook_runner.clone();
            let jwt_secret = self.jwt_secret.clone();
            let registry = self.registry.clone();
            let db_kind = self.db_kind.clone();
            let collection = req.collection.clone();
            let id = req.id.clone();
            let def_fields = def.fields.clone();
            let def_owned = def;
            let (proto_doc, auth_user) =
                tokio::task::spawn_blocking(move || -> Result<_, Status> {
                    let mut conn = pool.get().map_err(|e| map_db_error(e, "Pool", &db_kind))?;

                    let auth_user =
                        ContentService::resolve_auth_user(token, &jwt_secret, &registry, &conn)?;
                    let access_result = ContentService::check_access_blocking(
                        def_owned.access.update.as_deref(),
                        &auth_user,
                        Some(&id),
                        None,
                        &runner,
                        &mut conn,
                    )?;

                    if matches!(access_result, AccessResult::Denied) {
                        return Err(Status::permission_denied("Update access denied"));
                    }

                    let user_doc = auth_user.as_ref().map(|au| au.user_doc.clone());
                    // Release connection before service call (which acquires its own)
                    drop(conn);
                    let doc = service::unpublish_document(
                        &pool,
                        &runner,
                        &collection,
                        &id,
                        &def_owned,
                        user_doc.as_ref(),
                    )
                    .map_err(|e| map_db_error(e, "Unpublish error", &db_kind))?;

                    let mut proto_doc = document_to_proto(&doc, &collection);
                    let user_doc_ref = auth_user.as_ref().map(|au| &au.user_doc);
                    let mut conn = pool.get().map_err(|e| map_db_error(e, "Pool", &db_kind))?;
                    let tx = conn.transaction().map_err(|e| {
                        tracing::error!("Field read access tx error: {}", e);
                        Status::internal("Internal error")
                    })?;
                    let denied = runner.check_field_read_access(&def_fields, user_doc_ref, &tx);
                    if let Err(e) = tx.commit() {
                        tracing::warn!("tx commit failed: {e}");
                    }
                    strip_denied_proto_fields(&mut proto_doc, &denied);

                    Ok((proto_doc, auth_user))
                })
                .await
                .map_err(|e| {
                    tracing::error!("Task error: {}", e);
                    Status::internal("Internal error")
                })??;

            if let Err(e) = self.cache.clear() {
                tracing::warn!("Cache clear failed: {:#}", e);
            }

            self.hook_runner.publish_event(
                &self.event_bus,
                &self
                    .get_collection_def(&req.collection)
                    .map(|d| d.hooks.clone())
                    .unwrap_or_default(),
                match self.get_collection_def(&req.collection) {
                    Ok(d) => d.live.clone(),
                    Err(e) => {
                        tracing::warn!(
                            "Event publishing: failed to get collection def for '{}': {}",
                            req.collection,
                            e
                        );
                        None
                    }
                }
                .as_ref(),
                PublishEventInput::builder(EventTarget::Collection, EventOperation::Update)
                    .collection(req.collection.clone())
                    .document_id(req.id.clone())
                    .edited_by(Self::event_user_from(&auth_user))
                    .build(),
            );

            return Ok(Response::new(content::UpdateResponse {
                document: Some(proto_doc),
            }));
        }

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let jwt_secret = self.jwt_secret.clone();
        let registry = self.registry.clone();
        let db_kind = self.db_kind.clone();
        let collection = req.collection.clone();
        let id = req.id.clone();
        let def_fields = def.fields.clone();
        let def_owned = def;
        let (proto_doc, auth_user) = tokio::task::spawn_blocking(move || -> Result<_, Status> {
            let mut conn = pool.get().map_err(|e| map_db_error(e, "Pool", &db_kind))?;

            // Auth + access (all on blocking thread)
            let auth_user =
                ContentService::resolve_auth_user(token, &jwt_secret, &registry, &conn)?;
            let access_result = ContentService::check_access_blocking(
                def_owned.access.update.as_deref(),
                &auth_user,
                Some(&id),
                None,
                &runner,
                &mut conn,
            )?;

            if matches!(access_result, AccessResult::Denied) {
                return Err(Status::permission_denied("Update access denied"));
            }

            // Strip field-level update-denied fields
            {
                let tx = conn
                    .transaction()
                    .context("Transaction for field access")
                    .map_err(|e| {
                        tracing::error!("Field access tx error: {}", e);
                        Status::internal("Internal error")
                    })?;
                let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
                let denied =
                    runner.check_field_write_access(&def_owned.fields, user_doc, "update", &tx);
                tx.commit()
                    .context("Commit field access transaction")
                    .map_err(|e| {
                        tracing::error!("Field access commit error: {}", e);
                        Status::internal("Internal error")
                    })?;
                for name in &denied {
                    data.remove(name);
                    join_data.remove(name);
                }
            }

            let user_doc = auth_user.as_ref().map(|au| au.user_doc.clone());
            let auth_user_ui_locale = auth_user.as_ref().map(|au| au.ui_locale.clone());
            let ui_locale = user_doc.as_ref().and_then(|_| auth_user_ui_locale.clone());

            let (doc, _req_context) = service::update_document_with_conn(
                &mut conn,
                &runner,
                &collection,
                &id,
                &def_owned,
                WriteInput::builder(data, &join_data)
                    .password(password.as_deref())
                    .locale_ctx(locale_ctx.as_ref())
                    .draft(req.draft.unwrap_or(false))
                    .ui_locale(ui_locale)
                    .build(),
                user_doc.as_ref(),
            )
            .map_err(|e| map_db_error(e, "Update error", &db_kind))?;

            // Proto conversion + field stripping
            let mut proto_doc = document_to_proto(&doc, &collection);
            let user_doc_ref = auth_user.as_ref().map(|au| &au.user_doc);
            let mut conn = pool.get().map_err(|e| map_db_error(e, "Pool", &db_kind))?;
            let tx = conn.transaction().map_err(|e| {
                tracing::error!("Field read access tx error: {}", e);
                Status::internal("Internal error")
            })?;
            let denied = runner.check_field_read_access(&def_fields, user_doc_ref, &tx);
            tx.commit().map_err(|e| {
                tracing::error!("Field read access tx commit failed: {e}");
                Status::internal("Internal error")
            })?;
            strip_denied_proto_fields(&mut proto_doc, &denied);

            Ok((proto_doc, auth_user))
        })
        .await
        .map_err(|e| {
            tracing::error!("Task error: {}", e);
            Status::internal("Internal error")
        })??;

        if let Err(e) = self.cache.clear() {
            tracing::warn!("Cache clear failed: {:#}", e);
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
                PublishEventInput::builder(EventTarget::Collection, EventOperation::Update)
                    .collection(req.collection.clone())
                    .document_id(req.id.clone())
                    .edited_by(Self::event_user_from(&auth_user))
                    .build(),
            );
        }

        Ok(Response::new(content::UpdateResponse {
            document: Some(proto_doc),
        }))
    }

    /// Delete a document by ID, running before/after delete hooks.
    ///
    /// Permission check depends on the type of deletion:
    /// - Soft delete (trash): check `access.trash`, falling back to `access.update`
    /// - Permanent delete (`force_hard_delete` or no `soft_delete`): check `access.delete`
    pub(in crate::api::service) async fn delete_impl(
        &self,
        request: Request<content::DeleteRequest>,
    ) -> Result<Response<content::DeleteResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let req = request.into_inner();
        let mut def = self.get_collection_def(&req.collection)?;

        // Determine whether this will be a soft delete
        let will_soft_delete = def.soft_delete && !req.force_hard_delete;

        // Resolve permission: trash access for soft delete, delete access for permanent
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

        // When force_hard_delete is requested, override soft_delete on the def
        if req.force_hard_delete && def.soft_delete {
            def.soft_delete = false;
        }

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let jwt_secret = self.jwt_secret.clone();
        let registry = self.registry.clone();
        let db_kind = self.db_kind.clone();
        let def_clone = def.clone();
        let collection = req.collection.clone();
        let id = req.id.clone();
        let storage = self.storage.clone();
        let locale_config = self.locale_config.clone();
        let access_owned = access_ref.map(|s| s.to_string());
        let deny_msg_owned = deny_msg.to_string();

        let auth_user = tokio::task::spawn_blocking(move || -> Result<_, Status> {
            let mut conn = pool.get().map_err(|e| map_db_error(e, "Pool", &db_kind))?;

            // Auth + access (all on blocking thread)
            let auth_user =
                ContentService::resolve_auth_user(token, &jwt_secret, &registry, &conn)?;
            let access_result = ContentService::check_access_blocking(
                access_owned.as_deref(),
                &auth_user,
                Some(&id),
                None,
                &runner,
                &mut conn,
            )?;

            if matches!(access_result, AccessResult::Denied) {
                return Err(Status::permission_denied(deny_msg_owned));
            }

            let user_doc = auth_user.as_ref().map(|au| au.user_doc.clone());

            service::delete_document_with_conn(
                &mut conn,
                &runner,
                &collection,
                &id,
                &def_clone,
                user_doc.as_ref(),
                Some(&*storage),
                Some(&locale_config),
            )
            .map_err(|e| map_db_error(e, "Delete error", &db_kind))?;

            Ok(auth_user)
        })
        .await
        .map_err(|e| {
            tracing::error!("Task error: {}", e);
            Status::internal("Internal error")
        })??;

        if let Err(e) = self.cache.clear() {
            tracing::warn!("Cache clear failed: {:#}", e);
        }

        self.hook_runner.publish_event(
            &self.event_bus,
            &def.hooks,
            def.live.as_ref(),
            PublishEventInput::builder(EventTarget::Collection, EventOperation::Delete)
                .collection(req.collection.clone())
                .document_id(req.id.clone())
                .edited_by(Self::event_user_from(&auth_user))
                .build(),
        );

        Ok(Response::new(content::DeleteResponse {
            success: true,
            soft_deleted: will_soft_delete,
        }))
    }

    /// Restore a soft-deleted document from trash.
    pub(in crate::api::service) async fn restore_impl(
        &self,
        request: Request<content::RestoreRequest>,
    ) -> Result<Response<content::RestoreResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        if !def.soft_delete {
            return Err(Status::failed_precondition(
                "Collection does not have soft_delete enabled",
            ));
        }

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let jwt_secret = self.jwt_secret.clone();
        let registry = self.registry.clone();
        let db_kind = self.db_kind.clone();
        let def_clone = def.clone();
        let collection = req.collection.clone();
        let id = req.id.clone();
        let def_fields = def.fields.clone();
        let trash_access = def.access.resolve_trash().map(|s| s.to_string());

        let (proto_doc, auth_user) = tokio::task::spawn_blocking(move || -> Result<_, Status> {
            let mut conn = pool.get().map_err(|e| map_db_error(e, "Pool", &db_kind))?;

            // Auth + access (trash permission for restore)
            let auth_user =
                ContentService::resolve_auth_user(token, &jwt_secret, &registry, &conn)?;
            let access_result = ContentService::check_access_blocking(
                trash_access.as_deref(),
                &auth_user,
                Some(&id),
                None,
                &runner,
                &mut conn,
            )?;

            if matches!(access_result, AccessResult::Denied) {
                return Err(Status::permission_denied("Restore access denied"));
            }

            // Release connection before service call (which acquires its own)
            drop(conn);
            let doc = service::restore_document(&pool, &collection, &id, &def_clone)
                .map_err(|e| map_db_error(e, "Restore error", &db_kind))?;

            // Proto conversion + field stripping
            let mut proto_doc = document_to_proto(&doc, &collection);
            let user_doc_ref = auth_user.as_ref().map(|au| &au.user_doc);
            let mut conn = pool.get().map_err(|e| map_db_error(e, "Pool", &db_kind))?;
            let tx = conn.transaction().map_err(|e| {
                tracing::error!("Field read access tx error: {}", e);
                Status::internal("Internal error")
            })?;
            let denied = runner.check_field_read_access(&def_fields, user_doc_ref, &tx);
            tx.commit().map_err(|e| {
                tracing::error!("Field read access tx commit failed: {e}");
                Status::internal("Internal error")
            })?;
            strip_denied_proto_fields(&mut proto_doc, &denied);

            Ok((proto_doc, auth_user))
        })
        .await
        .map_err(|e| {
            tracing::error!("Task error: {}", e);
            Status::internal("Internal error")
        })??;

        if let Err(e) = self.cache.clear() {
            tracing::warn!("Cache clear failed: {:#}", e);
        }

        self.hook_runner.publish_event(
            &self.event_bus,
            &def.hooks,
            def.live.as_ref(),
            PublishEventInput::builder(EventTarget::Collection, EventOperation::Update)
                .collection(req.collection.clone())
                .document_id(req.id.clone())
                .edited_by(Self::event_user_from(&auth_user))
                .build(),
        );

        Ok(Response::new(content::RestoreResponse {
            document: Some(proto_doc),
        }))
    }
}
