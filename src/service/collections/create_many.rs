//! Bulk create -- create multiple documents in a single operation.
//!
//! **Pool mode** (`ctx.pool` set): all documents are created in a single
//! transaction, so the operation is atomic — a failure on any document rolls
//! the whole batch back. Events and cache are handled after the commit.
//!
//! **Conn mode** (`ctx.conn` set, Lua path): creates all documents on the
//! existing connection (atomic with the caller's transaction). Events are
//! queued for later flush by the caller.

use std::{cell::RefCell, rc::Rc};

use anyhow::Context as _;

use crate::{
    core::{DocumentFields, event::EventOperation},
    hooks::LuaCrudInfra,
    service::{
        RunnerWriteHooks, ServiceContext, ServiceError, WriteInput, create_document_in_conn,
        flush_queue, flush_verification_queue,
    },
    typegen::lua::LuaAnnotation,
};

use super::update_many::enforce_bulk_limit;

type Result<T> = std::result::Result<T, ServiceError>;

/// Input for a single document in a bulk create.
pub struct CreateManyItem {
    pub data: DocumentFields,
    pub password: Option<String>,
}

/// Options controlling bulk create behavior.
pub struct CreateManyOptions {
    /// Whether to run lifecycle hooks per document.
    pub run_hooks: bool,
    /// Whether documents are created as drafts.
    pub draft: bool,
    /// Maximum number of documents the operation may create before it is
    /// rejected (from `server.bulk_max_documents`). `0` = no limit.
    pub max_documents: i64,
}

impl Default for CreateManyOptions {
    fn default() -> Self {
        Self {
            run_hooks: true,
            draft: false,
            max_documents: 0,
        }
    }
}

/// Result of a bulk create operation.
#[derive(Debug, crate::typegen::lua::LuaAnnotation)]
#[lua(class = "crap.CreateManyResult")]
pub struct CreateManyResult {
    /// Number of documents created.
    pub created: i64,
    /// The created documents in order.
    #[lua(ty = "crap.Document[]")]
    pub documents: Vec<crate::core::Document>,
}

/// Create multiple documents from the given inputs.
///
/// Each document goes through the full lifecycle (before-hooks, validation,
/// persist, after-hooks). Referenced targets are validated per-document.
///
/// # Errors
///
/// Returns service-layer errors per-document or a backend error if the
/// transaction fails.
#[cfg(not(tarpaulin_include))]
pub fn create_many(
    ctx: &ServiceContext,
    items: &[CreateManyItem],
    opts: &CreateManyOptions,
) -> Result<CreateManyResult> {
    if let Some(pool) = ctx.pool {
        create_many_pooled(ctx, pool, items, opts)
    } else {
        create_many_on_conn(ctx, items, opts)
    }
}

/// Pool mode: create every document in a SINGLE transaction so the operation
/// is atomic — a failure on any document rolls the whole batch back.
fn create_many_pooled(
    ctx: &ServiceContext,
    pool: &crate::db::DbPool,
    items: &[CreateManyItem],
    opts: &CreateManyOptions,
) -> Result<CreateManyResult> {
    let runner = ctx.runner()?;
    let def = ctx.collection_def()?;

    enforce_bulk_limit("create_many", items.len(), opts.max_documents)?;

    let mut conn = pool.write().context("DB connection")?;
    let tx = conn
        .transaction_immediate()
        .context("Start bulk create transaction")?;

    let queue = Rc::new(RefCell::new(Vec::new()));
    let vqueue = Rc::new(RefCell::new(Vec::new()));

    let infra = LuaCrudInfra::from_ctx(ctx, Some(queue.clone()), Some(vqueue.clone()));

    let mut wh = RunnerWriteHooks::new(runner)
        .with_conn(&tx)
        .with_infra(infra);
    if !opts.run_hooks {
        wh = wh.with_hooks_enabled(false);
    }

    // Mirror the update_many/delete_many siblings: a trusted caller (MCP's
    // Principal::Override) must bypass the access hook and field-level
    // stripping here too — this was the one bulk op missing the block, so MCP
    // bulk creates were access-gated and silently field-stripped.
    if ctx.override_access {
        wh = wh.with_override_access();
    }

    let inner_ctx = ServiceContext::collection(ctx.slug, def)
        .conn(&tx)
        .write_hooks(&wh)
        .inherit_write_infra(ctx)
        .event_queue(queue.clone())
        .verification_queue(vqueue.clone())
        .email_ctx(ctx.email_ctx.clone())
        .build();

    let mut documents = Vec::with_capacity(items.len());
    let mut created = 0i64;

    for item in items {
        let input = WriteInput::builder(item.data.clone())
            .password(item.password.as_deref())
            .draft(opts.draft)
            .ui_locale(ctx.ui_locale.clone())
            .build();

        // A failure here returns via `?`; `tx` drops without commit, rolling
        // back every document created so far.
        let (doc, _after_ctx) = create_document_in_conn(&inner_ctx, input)?;
        documents.push(doc);
        created += 1;
    }

    // Release the borrows of `tx` before committing it.
    drop(inner_ctx);
    drop(wh);
    tx.commit().context("Commit bulk create transaction")?;

    ctx.clear_cache();

    // Per-doc events are gated by `ctx.emit_events` (bulk defaults to off).
    // Verification emails and nested-hook events always flush.
    for doc in &documents {
        ctx.publish_mutation_event(EventOperation::Create, &doc.id, &doc.fields);
        ctx.maybe_send_verification(doc);
    }
    flush_queue(ctx, &queue);
    flush_verification_queue(ctx, &vqueue);

    Ok(CreateManyResult { created, documents })
}

/// Conn mode (Lua): create on existing connection without transaction management.
fn create_many_on_conn(
    ctx: &ServiceContext,
    items: &[CreateManyItem],
    opts: &CreateManyOptions,
) -> Result<CreateManyResult> {
    enforce_bulk_limit("create_many", items.len(), opts.max_documents)?;

    let mut created = 0i64;
    let mut documents = Vec::with_capacity(items.len());

    for item in items {
        let input = WriteInput::builder(item.data.clone())
            .password(item.password.as_deref())
            .draft(opts.draft)
            .ui_locale(ctx.ui_locale.clone())
            .build();

        let (doc, _after_ctx) = create_document_in_conn(ctx, input)?;

        // Gated by `ctx.emit_events`; verification emails always send.
        ctx.publish_mutation_event(EventOperation::Create, &doc.id, &doc.fields);
        ctx.maybe_send_verification(&doc);
        documents.push(doc);
        created += 1;
    }

    Ok(CreateManyResult { created, documents })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SAFE-DEFAULT GUARD: bulk create must run lifecycle hooks (validation,
    /// hooks, ref-counting) per document by default, and not create drafts.
    /// Flipping `run_hooks` to `false` here would silently let bulk create
    /// bypass validation that single create always applies.
    #[test]
    fn create_many_options_default_runs_hooks_and_is_not_draft() {
        let opts = CreateManyOptions::default();
        assert!(opts.run_hooks, "bulk create must run hooks by default");
        assert!(!opts.draft, "bulk create is not a draft by default");
    }
}
