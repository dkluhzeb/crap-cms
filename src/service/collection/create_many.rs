//! Bulk create -- create multiple documents in a single operation.
//!
//! **Pool mode** (`ctx.pool` set): chunks documents into batched transactions
//! (500 per batch). Events and cache are handled after each commit.
//!
//! **Conn mode** (`ctx.conn` set, Lua path): creates all documents on the
//! existing connection. Events are queued for later flush by the caller.

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use anyhow::Context as _;
use serde_json::Value;

use crate::{
    core::event::EventOperation,
    hooks::LuaCrudInfra,
    service::{
        RunnerWriteHooks, ServiceContext, ServiceError, WriteInput, create_document_core,
        flush_queue, flush_verification_queue,
    },
};

const BATCH_SIZE: usize = 500;

type Result<T> = std::result::Result<T, ServiceError>;

/// Input for a single document in a bulk create.
pub struct CreateManyItem {
    pub data: HashMap<String, String>,
    pub join_data: HashMap<String, Value>,
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
pub struct CreateManyResult {
    pub created: i64,
    pub documents: Vec<crate::core::Document>,
}

/// Create multiple documents from the given inputs.
///
/// Each document goes through the full lifecycle (before-hooks, validation,
/// persist, after-hooks). Referenced targets are validated per-document.
#[cfg(not(tarpaulin_include))]
pub fn create_many(
    ctx: &ServiceContext,
    items: Vec<CreateManyItem>,
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
    items: Vec<CreateManyItem>,
    opts: &CreateManyOptions,
) -> Result<CreateManyResult> {
    let runner = ctx.runner()?;
    let def = ctx.collection_def();

    let mut created = 0i64;
    let mut documents = Vec::with_capacity(items.len());

    for chunk in items.chunks(BATCH_SIZE) {
        let mut conn = pool.get().context("DB connection")?;
        let tx = conn.transaction_immediate().context("Start transaction")?;

        let queue = Rc::new(RefCell::new(Vec::new()));
        let vqueue = Rc::new(RefCell::new(Vec::new()));

        let infra = LuaCrudInfra {
            event_transport: ctx.event_transport.clone(),
            cache: ctx.cache.clone(),
            event_queue: Some(queue.clone()),
            verification_queue: Some(vqueue.clone()),
        };

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
            let input = WriteInput::builder(item.data.clone(), &item.join_data)
                .password(item.password.as_deref())
                .draft(opts.draft)
                .build();

            let (doc, _after_ctx) = create_document_core(&inner_ctx, input)?;
            documents.push(doc);
            created += 1;
        }

        drop(inner_ctx);
        tx.commit().context("Commit transaction")?;

        ctx.clear_cache();
        for doc in documents.iter().skip(documents.len() - chunk.len()) {
            ctx.publish_mutation_event(EventOperation::Create, &doc.id, doc.fields.clone());
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
    items: Vec<CreateManyItem>,
    opts: &CreateManyOptions,
) -> Result<CreateManyResult> {
    let mut created = 0i64;
    let mut documents = Vec::with_capacity(items.len());

    for item in &items {
        let input = WriteInput::builder(item.data.clone(), &item.join_data)
            .password(item.password.as_deref())
            .draft(opts.draft)
            .build();

        let (doc, _after_ctx) = create_document_core(ctx, input)?;

        ctx.publish_mutation_event(EventOperation::Create, &doc.id, doc.fields.clone());
        ctx.maybe_send_verification(&doc);
        documents.push(doc);
        created += 1;
    }

    Ok(CreateManyResult { created, documents })
}
