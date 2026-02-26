//! Shared service layer for collection/global CRUD operations.
//!
//! These synchronous functions encapsulate the transaction lifecycle (open tx → run hooks →
//! DB operation → commit) shared between admin handlers and the gRPC service. They are meant
//! to be called from within `spawn_blocking`.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context as _, Result};

use crate::config::{EmailConfig, ServerConfig};
use crate::core::collection::{CollectionHooks, GlobalDefinition, VersionsConfig};
use crate::core::document::Document;
use crate::core::email::EmailRenderer;
use crate::core::field::FieldDefinition;
use crate::core::CollectionDefinition;
use crate::db::query::{self, LocaleContext};
use crate::db::DbPool;
use crate::hooks::lifecycle::{self, HookContext, HookEvent, HookRunner};

/// Result of a write operation: the document and the request-scoped hook context.
pub type WriteResult = (Document, HashMap<String, serde_json::Value>);

/// Save a draft-only version: merge incoming hook-processed data onto existing doc,
/// create a version snapshot, and prune. `final_ctx_data` contains all hook-modified
/// data including join table fields (arrays, blocks, relationships).
/// merge join data, create a draft version snapshot, and prune.
fn save_draft_version(
    tx: &rusqlite::Transaction,
    table: &str,
    parent_id: &str,
    fields: &[FieldDefinition],
    versions: Option<&VersionsConfig>,
    existing_doc: &Document,
    final_ctx_data: &HashMap<String, serde_json::Value>,
) -> Result<()> {
    let mut snapshot_fields = existing_doc.fields.clone();
    for (k, v) in final_ctx_data {
        snapshot_fields.insert(k.clone(), v.clone());
    }
    let snapshot_doc = Document {
        id: parent_id.to_string(),
        fields: snapshot_fields,
        created_at: existing_doc.created_at.clone(),
        updated_at: existing_doc.updated_at.clone(),
    };

    let mut snapshot = query::build_snapshot(tx, table, fields, &snapshot_doc)?;
    // Merge incoming hook-modified join data (blocks/arrays/has-many) into the snapshot.
    // build_snapshot hydrates from join tables (which have the old/published data),
    // so we must overwrite with the hook-processed data for draft-only saves.
    if let Some(obj) = snapshot.as_object_mut() {
        for field in fields {
            match field.field_type {
                crate::core::field::FieldType::Array
                | crate::core::field::FieldType::Blocks
                | crate::core::field::FieldType::Relationship => {
                    if let Some(v) = final_ctx_data.get(&field.name) {
                        obj.insert(field.name.clone(), v.clone());
                    }
                }
                _ => {}
            }
        }
    }
    query::create_version(tx, table, parent_id, "draft", &snapshot)?;
    if let Some(vc) = versions {
        if vc.max_versions > 0 {
            query::prune_versions(tx, table, parent_id, vc.max_versions)?;
        }
    }
    Ok(())
}

/// Set document status, create a version snapshot, and prune.
/// Used for both initial creates (status may be "draft") and normal updates ("published").
fn create_version_snapshot(
    tx: &rusqlite::Transaction,
    table: &str,
    parent_id: &str,
    fields: &[FieldDefinition],
    versions: Option<&VersionsConfig>,
    has_drafts: bool,
    status: &str,
    doc: &Document,
) -> Result<()> {
    if has_drafts {
        query::set_document_status(tx, table, parent_id, status)?;
    }
    let snapshot = query::build_snapshot(tx, table, fields, doc)?;
    query::create_version(tx, table, parent_id, status, &snapshot)?;
    if let Some(vc) = versions {
        if vc.max_versions > 0 {
            query::prune_versions(tx, table, parent_id, vc.max_versions)?;
        }
    }
    Ok(())
}

/// Create a document within a single transaction: before-hooks → insert → join data → password.
/// When `draft` is true and the collection has drafts enabled, the document is created with
/// `_status = 'draft'` and required-field validation is skipped.
/// Returns the created document and the request-scoped context from before-hooks.
#[allow(clippy::too_many_arguments)]
pub fn create_document(
    pool: &DbPool,
    runner: &HookRunner,
    slug: &str,
    def: &CollectionDefinition,
    data: HashMap<String, String>,
    join_data: &HashMap<String, serde_json::Value>,
    password: Option<&str>,
    locale_ctx: Option<&LocaleContext>,
    locale: Option<String>,
    user: Option<&Document>,
    draft: bool,
) -> Result<WriteResult> {
    let is_draft = draft && def.has_drafts();
    let status = if is_draft { "draft" } else { "published" };

    let mut conn = pool.get().context("DB connection")?;
    let tx = conn.transaction().context("Start transaction")?;

    let mut hook_data: HashMap<String, serde_json::Value> = data.iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();
    // Merge structured join data (blocks, arrays, has-many) so validation and hooks
    // see them as proper JSON arrays rather than flat bracket-notation keys.
    for (k, v) in join_data {
        hook_data.insert(k.clone(), v.clone());
    }
    let hook_ctx = HookContext {
        collection: slug.to_string(),
        operation: "create".to_string(),
        data: hook_data,
        locale: locale.clone(),
        draft: Some(is_draft),
        context: HashMap::new(),
    };
    let final_ctx = runner.run_before_write(
        &def.hooks, &def.fields, hook_ctx, &tx, slug, None, user, is_draft,
    )?;
    let req_context = final_ctx.context.clone();
    let final_data = lifecycle::hook_ctx_to_string_map(&final_ctx, &def.fields);
    let doc = query::create(&tx, slug, def, &final_data, locale_ctx)?;

    // Use hook-modified data so before_change hooks that alter arrays/blocks/relationships
    // have their changes persisted.
    query::save_join_table_data(&tx, slug, &def.fields, &doc.id, &final_ctx.data, locale_ctx)?;

    if let Some(pw) = password {
        if !pw.is_empty() {
            query::update_password(&tx, slug, &doc.id, pw)?;
        }
    }

    // Versioning: set status (only if drafts enabled) and create initial version snapshot
    if def.has_versions() {
        create_version_snapshot(
            &tx, slug, &doc.id, &def.fields,
            def.versions.as_ref(), def.has_drafts(), status, &doc,
        )?;
    }

    // After-hooks: run inside the same transaction, with CRUD access
    let after_ctx = HookContext {
        collection: slug.to_string(),
        operation: "create".to_string(),
        data: doc.fields.clone(),
        locale,
        draft: Some(is_draft),
        context: req_context,
    };
    let after_result = runner.run_after_write(
        &def.hooks, &def.fields, HookEvent::AfterChange,
        after_ctx, &tx, user,
    )?;
    let req_context = after_result.context;

    tx.commit().context("Commit transaction")?;
    Ok((doc, req_context))
}

/// Update a document within a single transaction: before-hooks → update → join data → password.
/// When `draft` is true and the collection has drafts enabled, the update creates a version-only
/// save: the main table is NOT modified, only a new version snapshot is recorded. On publish
/// (`draft=false`), the main table is updated and `_status` set to `"published"`.
#[allow(clippy::too_many_arguments)]
pub fn update_document(
    pool: &DbPool,
    runner: &HookRunner,
    slug: &str,
    id: &str,
    def: &CollectionDefinition,
    data: HashMap<String, String>,
    join_data: &HashMap<String, serde_json::Value>,
    password: Option<&str>,
    locale_ctx: Option<&LocaleContext>,
    locale: Option<String>,
    user: Option<&Document>,
    draft: bool,
) -> Result<WriteResult> {
    let is_draft = draft && def.has_drafts();

    let mut conn = pool.get().context("DB connection")?;
    let tx = conn.transaction().context("Start transaction")?;

    let mut hook_data: HashMap<String, serde_json::Value> = data.iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();
    // Merge structured join data (blocks, arrays, has-many) so validation and hooks
    // see them as proper JSON arrays rather than flat bracket-notation keys.
    for (k, v) in join_data {
        hook_data.insert(k.clone(), v.clone());
    }
    let hook_ctx = HookContext {
        collection: slug.to_string(),
        operation: "update".to_string(),
        data: hook_data,
        locale: locale.clone(),
        draft: Some(is_draft),
        context: HashMap::new(),
    };
    let final_ctx = runner.run_before_write(
        &def.hooks, &def.fields, hook_ctx, &tx, slug, Some(id), user, is_draft,
    )?;
    let req_context = final_ctx.context.clone();
    let final_data = lifecycle::hook_ctx_to_string_map(&final_ctx, &def.fields);

    if is_draft && def.has_versions() {
        // Version-only save: do NOT update the main table.
        let existing_doc = query::find_by_id_raw(&tx, slug, def, id, None)?
            .ok_or_else(|| anyhow::anyhow!("Document {} not found in {}", id, slug))?;

        save_draft_version(
            &tx, slug, id, &def.fields, def.versions.as_ref(),
            &existing_doc, &final_ctx.data,
        )?;

        // After-hooks: run inside the same transaction, with CRUD access
        let after_ctx = HookContext {
            collection: slug.to_string(),
            operation: "update".to_string(),
            data: existing_doc.fields.clone(),
            locale: locale.clone(),
            draft: Some(is_draft),
            context: req_context,
        };
        let after_result = runner.run_after_write(
            &def.hooks, &def.fields, HookEvent::AfterChange,
            after_ctx, &tx, user,
        )?;
        let req_context = after_result.context;

        tx.commit().context("Commit transaction")?;
        Ok((existing_doc, req_context))
    } else {
        // Normal update: write to main table
        let doc = query::update(&tx, slug, def, id, &final_data, locale_ctx)?;

        // Use hook-modified data so before_change hooks that alter arrays/blocks/relationships
        // have their changes persisted.
        query::save_join_table_data(&tx, slug, &def.fields, &doc.id, &final_ctx.data, locale_ctx)?;

        if let Some(pw) = password {
            if !pw.is_empty() {
                query::update_password(&tx, slug, &doc.id, pw)?;
            }
        }

        // Versioning: set status to published and create version
        if def.has_versions() {
            create_version_snapshot(
                &tx, slug, &doc.id, &def.fields,
                def.versions.as_ref(), def.has_drafts(), "published", &doc,
            )?;
        }

        // After-hooks: run inside the same transaction, with CRUD access
        let after_ctx = HookContext {
            collection: slug.to_string(),
            operation: "update".to_string(),
            data: doc.fields.clone(),
            locale: locale.clone(),
            draft: Some(is_draft),
            context: req_context,
        };
        let after_result = runner.run_after_write(
            &def.hooks, &def.fields, HookEvent::AfterChange,
            after_ctx, &tx, user,
        )?;
        let req_context = after_result.context;

        tx.commit().context("Commit transaction")?;
        Ok((doc, req_context))
    }
}

/// Delete a document within a single transaction: before-hooks → delete.
/// Returns the request-scoped context from before-hooks.
pub fn delete_document(
    pool: &DbPool,
    runner: &HookRunner,
    slug: &str,
    id: &str,
    hooks: &CollectionHooks,
    user: Option<&Document>,
) -> Result<HashMap<String, serde_json::Value>> {
    let mut conn = pool.get().context("DB connection")?;
    let tx = conn.transaction().context("Start transaction")?;

    let hook_ctx = HookContext {
        collection: slug.to_string(),
        operation: "delete".to_string(),
        data: [("id".to_string(), serde_json::Value::String(id.to_string()))].into(),
        locale: None,
        draft: None,
        context: HashMap::new(),
    };
    let final_ctx = runner.run_hooks_with_conn(hooks, HookEvent::BeforeDelete, hook_ctx, &tx, user)?;
    query::delete(&tx, slug, id)?;

    // After-hooks: run inside the same transaction, with CRUD access
    let after_ctx = HookContext {
        collection: slug.to_string(),
        operation: "delete".to_string(),
        data: [("id".to_string(), serde_json::Value::String(id.to_string()))].into(),
        locale: None,
        draft: None,
        context: final_ctx.context,
    };
    let after_result = runner.run_hooks_with_conn(hooks, HookEvent::AfterDelete, after_ctx, &tx, user)?;

    tx.commit().context("Commit transaction")?;
    Ok(after_result.context)
}

/// Update a global document within a single transaction: before-hooks → update → join data.
/// When `draft` is true and the global has drafts enabled, creates a version-only save
/// (main table NOT modified). On publish (`draft=false`), the main table is updated.
#[allow(clippy::too_many_arguments)]
pub fn update_global_document(
    pool: &DbPool,
    runner: &HookRunner,
    slug: &str,
    def: &GlobalDefinition,
    data: HashMap<String, String>,
    join_data: &HashMap<String, serde_json::Value>,
    locale_ctx: Option<&LocaleContext>,
    locale: Option<String>,
    user: Option<&Document>,
    draft: bool,
) -> Result<WriteResult> {
    let is_draft = draft && def.has_drafts();

    let mut conn = pool.get().context("DB connection")?;
    let tx = conn.transaction().context("Start transaction")?;

    let global_table = format!("_global_{}", slug);

    let mut hook_data: HashMap<String, serde_json::Value> = data.iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();
    // Merge structured join data (blocks, arrays, has-many) so validation and hooks
    // see them as proper JSON arrays rather than flat bracket-notation keys.
    for (k, v) in join_data {
        hook_data.insert(k.clone(), v.clone());
    }
    let hook_ctx = HookContext {
        collection: slug.to_string(),
        operation: "update".to_string(),
        data: hook_data,
        locale: locale.clone(),
        draft: Some(is_draft),
        context: HashMap::new(),
    };
    let final_ctx = runner.run_before_write(
        &def.hooks, &def.fields, hook_ctx, &tx, &global_table, Some("default"), user, is_draft,
    )?;
    let req_context = final_ctx.context.clone();
    let final_data = lifecycle::hook_ctx_to_string_map(&final_ctx, &def.fields);

    if is_draft && def.has_versions() {
        // Version-only save: do NOT update the main table.
        let existing_doc = query::get_global(&tx, slug, def, locale_ctx)?;

        save_draft_version(
            &tx, &global_table, "default", &def.fields, def.versions.as_ref(),
            &existing_doc, &final_ctx.data,
        )?;

        // After-hooks
        let after_ctx = HookContext {
            collection: slug.to_string(),
            operation: "update".to_string(),
            data: existing_doc.fields.clone(),
            locale: locale.clone(),
            draft: Some(is_draft),
            context: req_context,
        };
        let after_result = runner.run_after_write(
            &def.hooks, &def.fields, HookEvent::AfterChange,
            after_ctx, &tx, user,
        )?;
        let req_context = after_result.context;

        tx.commit().context("Commit transaction")?;
        Ok((existing_doc, req_context))
    } else {
        // Normal update: write to main table
        let doc = query::update_global(&tx, slug, def, &final_data, locale_ctx)?;

        // Use hook-modified data so before_change hooks that alter arrays/blocks/relationships
        // have their changes persisted.
        query::save_join_table_data(&tx, &global_table, &def.fields, "default", &final_ctx.data, locale_ctx)?;

        // Versioning: set status to published and create version
        if def.has_versions() {
            create_version_snapshot(
                &tx, &global_table, "default", &def.fields,
                def.versions.as_ref(), def.has_drafts(), "published", &doc,
            )?;
        }

        // After-hooks: run inside the same transaction, with CRUD access
        let after_ctx = HookContext {
            collection: slug.to_string(),
            operation: "update".to_string(),
            data: doc.fields.clone(),
            locale,
            draft: Some(is_draft),
            context: req_context,
        };
        let after_result = runner.run_after_write(
            &def.hooks, &def.fields, HookEvent::AfterChange,
            after_ctx, &tx, user,
        )?;
        let req_context = after_result.context;

        tx.commit().context("Commit transaction")?;
        Ok((doc, req_context))
    }
}

/// Fire-and-forget: generate a verification token and send the verification email.
/// Spawns its own `spawn_blocking` task internally.
pub fn send_verification_email(
    pool: DbPool,
    email_config: EmailConfig,
    email_renderer: Arc<EmailRenderer>,
    server_config: ServerConfig,
    slug: String,
    user_id: String,
    user_email: String,
) {
    tokio::task::spawn_blocking(move || {
        if !crate::core::email::is_configured(&email_config) {
            tracing::warn!("Email not configured — skipping verification email for {}", user_email);
            return;
        }

        let token = nanoid::nanoid!(32);

        let conn = match pool.get() {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("DB connection for verification token: {}", e);
                return;
            }
        };
        if let Err(e) = query::set_verification_token(&conn, &slug, &user_id, &token) {
            tracing::error!("Failed to set verification token: {}", e);
            return;
        }

        let verify_url = format!(
            "http://{}:{}/admin/verify-email?token={}",
            server_config.host, server_config.admin_port, token
        );
        let data = serde_json::json!({ "verify_url": verify_url });
        let html = match email_renderer.render("verify_email", &data) {
            Ok(h) => h,
            Err(e) => {
                tracing::error!("Failed to render verify email template: {}", e);
                return;
            }
        };

        if let Err(e) = crate::core::email::send_email(
            &email_config, &user_email, "Verify your email", &html, None,
        ) {
            tracing::error!("Failed to send verification email: {}", e);
        }
    });
}
