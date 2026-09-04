//! Bulk create -- create multiple documents in a single operation.
//!
//! **Pool mode** (`ctx.pool` set): all documents are created in a single
//! transaction, so the operation is atomic — a failure on any document rolls
//! the whole batch back. Events and cache are handled after the commit.
//!
//! **Conn mode** (`ctx.conn` set, Lua path): creates all documents on the
//! existing connection (atomic with the caller's transaction). Events are
//! queued for later flush by the caller.

use crate::{
    core::{DocumentFields, event::EventOperation},
    db::LocaleContext,
    service::{ServiceContext, ServiceError, WriteInput, create_document_in_conn, run_pool_write},
    typegen::lua::LuaAnnotation,
};

use super::update_many::enforce_bulk_limit;
use crate::service::OpDeadline;

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
    /// Locale for localized field writes — every item writes this locale's
    /// columns, exactly like single create. `None` = default locale.
    pub locale_ctx: Option<LocaleContext>,
    /// Cooperative abort deadline, checked between documents. Set by the
    /// queued-bulk runner so a job timeout actually stops (and rolls back)
    /// the batch instead of only recording a failure.
    pub deadline: OpDeadline,
}

impl Default for CreateManyOptions {
    fn default() -> Self {
        Self {
            run_hooks: true,
            draft: false,
            max_documents: 0,
            locale_ctx: None,
            deadline: OpDeadline::none(),
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
    if ctx.pool.is_some() {
        create_many_pooled(ctx, items, opts)
    } else {
        create_many_on_conn(ctx, items, opts)
    }
}

/// Pool mode: create every document in a SINGLE transaction so the operation
/// is atomic — a failure on any document rolls the whole batch back.
fn create_many_pooled(
    ctx: &ServiceContext,
    items: &[CreateManyItem],
    opts: &CreateManyOptions,
) -> Result<CreateManyResult> {
    enforce_bulk_limit("create_many", items.len(), opts.max_documents)?;

    run_pool_write(
        ctx,
        (!opts.run_hooks).then_some(false),
        |inner| {
            let mut documents = Vec::with_capacity(items.len());
            let mut created = 0i64;

            for item in items {
                opts.deadline.check(created)?;

                let input = WriteInput::builder(item.data.clone())
                    .password(item.password.as_deref())
                    .locale_ctx(opts.locale_ctx.as_ref())
                    .draft(opts.draft)
                    .ui_locale(ctx.ui_locale.clone())
                    .build();

                // A failure here returns via `?`; the envelope rolls back
                // every document created so far.
                let (doc, _after_ctx) = create_document_in_conn(inner, input)?;
                documents.push(doc);
                created += 1;
            }

            // One last check: everything after this returns into the
            // envelope's `tx.commit()`, so an expiry here still rolls back.
            opts.deadline.check(created)?;

            Ok(CreateManyResult { created, documents })
        },
        |ctx, result| {
            // Per-doc events are gated by `ctx.emit_events` (bulk defaults to
            // off). Verification emails always send.
            for doc in &result.documents {
                ctx.publish_mutation_event(EventOperation::Create, &doc.id, &doc.fields);
                ctx.maybe_send_verification(doc);
            }
        },
    )
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
        opts.deadline.check(created)?;

        let input = WriteInput::builder(item.data.clone())
            .password(item.password.as_deref())
            .locale_ctx(opts.locale_ctx.as_ref())
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

    // Parity with the single-document conn path (and the pool path's
    // orchestrator): every write clears the populate cache.
    ctx.clear_cache();

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
