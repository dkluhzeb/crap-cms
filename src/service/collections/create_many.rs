//! Bulk create -- create multiple documents in a single operation.
//!
//! **Pool mode** (`ctx.pool` set): chunks documents into batched transactions
//! (500 per batch). Events and cache are handled after each commit.
//!
//! **Conn mode** (`ctx.conn` set, Lua path): creates all documents on the
//! existing connection. Events are queued for later flush by the caller.

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

const BATCH_SIZE: usize = 500;

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
}

impl Default for CreateManyOptions {
    fn default() -> Self {
        Self {
            run_hooks: true,
            draft: false,
        }
    }
}

/// Result of a bulk create operation.
#[derive(crate::typegen::lua::LuaAnnotation)]
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

/// Pool mode: chunk into transactions.
fn create_many_pooled(
    ctx: &ServiceContext,
    pool: &crate::db::DbPool,
    items: &[CreateManyItem],
    opts: &CreateManyOptions,
) -> Result<CreateManyResult> {
    let runner = ctx.runner()?;
    let def = ctx.collection_def()?;

    let mut created = 0i64;
    let mut documents = Vec::with_capacity(items.len());

    for chunk in items.chunks(BATCH_SIZE) {
        let mut conn = pool.get().context("DB connection")?;
        let tx = conn.transaction_immediate().context("Start transaction")?;

        let queue = Rc::new(RefCell::new(Vec::new()));
        let vqueue = Rc::new(RefCell::new(Vec::new()));

        let infra = LuaCrudInfra::from_ctx(ctx, Some(queue.clone()), Some(vqueue.clone()));

        let mut wh = RunnerWriteHooks::new(runner)
            .with_conn(&tx)
            .with_infra(infra);
        if !opts.run_hooks {
            wh = wh.with_hooks_enabled(false);
        }

        let inner_ctx = ServiceContext::collection(ctx.slug, def)
            .conn(&tx)
            .write_hooks(&wh)
            .user(ctx.user)
            .override_access(ctx.override_access)
            .event_transport(ctx.event_transport.clone())
            .event_queue(queue.clone())
            .verification_queue(vqueue.clone())
            .cache(ctx.cache.clone())
            .email_ctx(ctx.email_ctx.clone())
            .build();

        for item in chunk {
            let input = WriteInput::builder(item.data.clone())
                .password(item.password.as_deref())
                .draft(opts.draft)
                .build();

            let (doc, _after_ctx) = create_document_in_conn(&inner_ctx, input)?;
            documents.push(doc);
            created += 1;
        }

        drop(inner_ctx);
        tx.commit().context("Commit transaction")?;

        ctx.clear_cache();
        for doc in documents.iter().skip(documents.len() - chunk.len()) {
            ctx.publish_mutation_event(EventOperation::Create, &doc.id, &doc.fields);
            ctx.maybe_send_verification(doc);
        }
        flush_queue(ctx, &queue);
        flush_verification_queue(ctx, &vqueue);
    }

    Ok(CreateManyResult { created, documents })
}

/// Conn mode (Lua): create on existing connection without transaction management.
fn create_many_on_conn(
    ctx: &ServiceContext,
    items: &[CreateManyItem],
    opts: &CreateManyOptions,
) -> Result<CreateManyResult> {
    let mut created = 0i64;
    let mut documents = Vec::with_capacity(items.len());

    for item in items {
        let input = WriteInput::builder(item.data.clone())
            .password(item.password.as_deref())
            .draft(opts.draft)
            .build();

        let (doc, _after_ctx) = create_document_in_conn(ctx, input)?;

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
