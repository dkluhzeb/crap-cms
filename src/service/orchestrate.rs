//! The ONE pool-mode write envelope.
//!
//! Every pool-mode write operation used to hand-copy the same orchestration:
//! open a write transaction, create the nested-CRUD event + verification
//! queues, build the runner write hooks, assemble an inner conn-mode context,
//! run the body, commit, clear the cache, publish its events, flush the
//! queues. Ten near-identical copies — and exactly where orchestration bugs
//! clustered (missing verification queues in eight files, restore lacking its
//! event queue entirely). [`run_pool_write`] owns the envelope once; the
//! per-operation code shrinks to its body (what happens inside the
//! transaction) and its post-commit effects (which events to publish).
//!
//! The inner context is a deliberate SUPERSET of what any one body needs
//! (verification queue, email context, and locale config are always
//! attached) — a body that starts needing one of them later cannot find it
//! missing, closing the forgot-a-field class the hand-rolled contexts had.
//!
//! Conn-mode (Lua-in-hook-transaction) writes don't come through here by
//! design: the caller owns their transaction and queue flushing.

use std::{cell::RefCell, rc::Rc};

use anyhow::{Context as _, anyhow};

use crate::{
    hooks::LuaCrudInfra,
    service::{
        Def, DeferredQueue, EffectOutcome, RunnerWriteHooks, ServiceContext, ServiceError,
        flush_deferred_effects, flush_queue, flush_verification_queue,
    },
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Run one pool-mode write inside the shared envelope.
///
/// - `hooks_enabled`: `Some(false)` disables lifecycle hooks on the runner
///   write hooks (the bulk ops' `run_hooks` option); `None` keeps the
///   default (enabled).
/// - `body` runs inside the open transaction against the inner conn-mode
///   context (connection, write hooks with `override_access` applied, both
///   nested-CRUD queues, inherited write infra, email context, locale
///   config). Any error rolls the transaction back.
/// - `post_commit` runs after a successful commit against the OUTER context:
///   publish the operation's mutation events, tear down invalidated user
///   streams, send verification emails. The envelope then flushes the
///   nested-CRUD event queue and the verification queue.
///
/// # Errors
///
/// Propagates the body's error (transaction rolled back), or a backend error
/// from connection/transaction management.
pub(crate) fn run_pool_write<T>(
    ctx: &ServiceContext<'_>,
    hooks_enabled: Option<bool>,
    body: impl for<'i> FnOnce(&ServiceContext<'i>) -> Result<T>,
    post_commit: impl FnOnce(&ServiceContext<'_>, &T),
) -> Result<T> {
    let pool = ctx.pool.context("pool required")?;
    let runner = ctx.runner()?;
    let mut conn = pool.write().context("DB connection")?;
    let tx = conn.transaction_immediate().context("Start transaction")?;

    // Nested-CRUD queues: a hook running inside this transaction that
    // creates/updates documents (or a verify-email auth user) queues its
    // events/emails here; they flush only after a successful commit.
    let queue = Rc::new(RefCell::new(Vec::new()));
    let vqueue = Rc::new(RefCell::new(Vec::new()));

    // Transaction-outcome effects (`crap.tx.on_commit` / `on_rollback`)
    // registered by hooks at any nesting depth inside this transaction.
    let dq: DeferredQueue = Rc::new(RefCell::new(Vec::new()));
    let fq: crate::hooks::lifecycle::FileCleanupQueue = Rc::new(RefCell::new(Vec::new()));

    let mut infra = LuaCrudInfra::from_ctx(ctx, Some(queue.clone()), Some(vqueue.clone()));
    infra.deferred = Some(dq.clone());
    infra.file_cleanup = Some(fq.clone());

    let mut wh = RunnerWriteHooks::new(runner)
        .with_conn(&tx)
        .with_infra(infra);
    if let Some(enabled) = hooks_enabled {
        wh = wh.with_hooks_enabled(enabled);
    }
    if ctx.override_access {
        wh = wh.with_override_access();
    }

    let builder = match &ctx.def {
        Def::Collection(def) => ServiceContext::collection(ctx.slug, def),
        Def::Global(def) => ServiceContext::global(ctx.slug, def),
        Def::None => {
            return Err(ServiceError::Internal(anyhow!(
                "pool write requires a collection or global definition"
            )));
        }
    };

    let inner_ctx = builder
        .conn(&tx)
        .write_hooks(&wh)
        .inherit_write_infra(ctx)
        .ui_locale(ctx.ui_locale.clone())
        .event_queue(queue.clone())
        .file_cleanup(fq.clone())
        .verification_queue(vqueue.clone())
        .email_ctx(ctx.email_ctx.clone())
        .locale_config(ctx.locale_config)
        .build();

    let result = body(&inner_ctx);

    // Release the borrows of `tx` before resolving it.
    drop(inner_ctx);
    drop(wh);

    let result = match result {
        Ok(v) => v,
        Err(e) => {
            // Roll back AND release the pooled connection BEFORE
            // compensations run — their pool-mode CRUD takes a fresh
            // write-pool checkout (ledger class L12: holding `conn`
            // across the flush is the two-connection deadlock shape;
            // with `write_pool_max_size` writers all in their flush
            // phase, every nested acquire would starve).
            drop(tx);
            drop(conn);
            flush_deferred_effects(ctx, &dq, EffectOutcome::Rollback);

            return Err(e);
        }
    };

    let commit_result = tx.commit().context("Commit transaction");
    // Same release-before-effects rule on the commit side: everything
    // from here on (cache clear, post-commit callback, event/email/
    // effect flushes) runs pool-mode CRUD or Lua and must not execute
    // while this write-pool slot is still held.
    drop(conn);

    if let Err(e) = commit_result {
        flush_deferred_effects(ctx, &dq, EffectOutcome::Rollback);

        return Err(e.into());
    }

    ctx.clear_cache();

    // Files after commit: hard deletes performed by hooks inside this
    // transaction queued their upload field-maps; the bytes go only now
    // that the rows are durably gone. (On rollback the queue is simply
    // dropped — orphaned files are the safe direction.)
    if let Some(storage) = &ctx.storage {
        for fields in fq.borrow_mut().drain(..) {
            crate::core::upload::delete_upload_files(storage.as_ref(), &fields);
        }
    }

    post_commit(ctx, &result);

    flush_queue(ctx, &queue);
    flush_verification_queue(ctx, &vqueue);
    flush_deferred_effects(ctx, &dq, EffectOutcome::Commit);

    Ok(result)
}
