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

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use anyhow::{Context as _, anyhow};

use crate::{
    core::upload::delete_storage_keys,
    hooks::LuaCrudInfra,
    service::{
        Def, DeferredQueue, EffectOutcome, RunnerWriteHooks, ServiceContext, ServiceError,
        flush_deferred_effects, flush_queue, flush_verification_queue, warn_orphaned_files,
    },
};

type Result<T> = std::result::Result<T, ServiceError>;

thread_local! {
    /// The commit watches open on this thread, innermost last. A pool write
    /// marks the innermost one the moment its transaction is durable.
    static COMMIT_WATCHES: RefCell<Vec<Rc<Cell<bool>>>> = const { RefCell::new(Vec::new()) };
}

/// Learns whether a pool write issued while it is open reached a durable
/// commit — including when the envelope's post-commit work then panics and
/// unwinds through the caller without returning.
///
/// The commit is the moment the file half of an upload write settles: the
/// stored bytes stay once the row naming them is durable, whatever happens to
/// the cache clear, event publishing or effect flushes that run before the
/// envelope returns. The caller sits above an operation it does not control
/// (`create_document`, `update_document`), so the envelope reports the commit
/// through this thread-local watch rather than through its return value.
///
/// Watches nest: the innermost open one is what a commit marks, so a write
/// issued from post-commit work under a watch of its own is never mistaken
/// for the outer one. Dropping a watch closes it.
pub(crate) struct CommitWatch {
    flag: Rc<Cell<bool>>,
}

impl CommitWatch {
    /// Open a watch for the pool writes issued on this thread from now on.
    pub(crate) fn open() -> Self {
        let flag = Rc::new(Cell::new(false));

        COMMIT_WATCHES.with(|watches| watches.borrow_mut().push(flag.clone()));

        Self { flag }
    }

    /// Whether a pool write under this watch committed.
    pub(crate) fn committed(&self) -> bool {
        self.flag.get()
    }
}

impl Drop for CommitWatch {
    fn drop(&mut self) {
        COMMIT_WATCHES.with(|watches| {
            watches
                .borrow_mut()
                .retain(|watch| !Rc::ptr_eq(watch, &self.flag));
        });
    }
}

/// A pool write just committed: tell the innermost open watch, if any.
fn mark_commit() {
    COMMIT_WATCHES.with(|watches| {
        if let Some(watch) = watches.borrow().last() {
            watch.set(true);
        }
    });
}

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
            // write-pool checkout.
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

    // Durable from here on. Reported before any post-commit work, so a
    // caller holding stored bytes for this write keeps them even if that
    // work panics and this function never returns.
    mark_commit();

    ctx.clear_cache();

    // Files after commit: hard deletes performed by hooks inside this
    // transaction queued their upload field-maps; the bytes go only now
    // that the rows are durably gone. (On rollback the queue is simply
    // dropped — orphaned files are the safe direction.)
    let keys: Vec<String> = fq.borrow_mut().drain(..).collect();
    if let Some(storage) = &ctx.storage {
        delete_storage_keys(storage.as_ref(), &keys);
    } else {
        // Same visibility as a write that found no cleanup queue: the keys were
        // collected, nothing can delete them, and only a log line says so.
        warn_orphaned_files(ctx.slug, "storage backend", keys.len());
    }

    post_commit(ctx, &result);

    flush_queue(ctx, &queue);
    flush_verification_queue(ctx, &vqueue);
    flush_deferred_effects(ctx, &dq, EffectOutcome::Commit);

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Without a pool write, a watch reports nothing.
    #[test]
    fn a_fresh_watch_has_seen_no_commit() {
        let watch = CommitWatch::open();

        assert!(!watch.committed());
    }

    /// A commit marks the innermost open watch only: a write issued from
    /// post-commit work under its own watch must not count for the outer.
    #[test]
    fn a_commit_marks_the_innermost_watch() {
        let outer = CommitWatch::open();

        {
            let inner = CommitWatch::open();

            mark_commit();

            assert!(inner.committed());
            assert!(!outer.committed(), "the outer write has not committed");
        }

        mark_commit();

        assert!(outer.committed(), "the inner watch is closed again");
    }

    /// A commit with no watch open is nobody's business — and must not
    /// leak into a watch opened afterwards.
    #[test]
    fn a_commit_with_no_watch_open_is_forgotten() {
        mark_commit();

        let watch = CommitWatch::open();

        assert!(!watch.committed());
    }

    #[cfg(feature = "sqlite")]
    mod envelope {
        use std::panic::{AssertUnwindSafe, catch_unwind};

        use super::*;
        use crate::{
            admin::test_support::test_infra_with_events,
            core::{CollectionDefinition, FieldDefinition, FieldType},
        };

        fn things() -> CollectionDefinition {
            let mut def = CollectionDefinition::new("things");
            def.fields = vec![FieldDefinition::builder("name", FieldType::Text).build()];

            def
        }

        /// The commit is reported before the post-commit callback runs, so a
        /// panic there still leaves the watch marked — the row IS durable.
        #[test]
        fn a_post_commit_panic_still_reports_the_commit() {
            let def = things();
            let (_tmp, infra, _rx) = test_infra_with_events(def.clone());
            let ctx = ServiceContext::collection("things", &def)
                .infra(&infra)
                .build();

            let watch = CommitWatch::open();

            let outcome = catch_unwind(AssertUnwindSafe(|| {
                run_pool_write(
                    &ctx,
                    None,
                    |_| Ok(()),
                    |_, (): &()| panic!("post-commit work failed"),
                )
            }));

            assert!(outcome.is_err(), "the post-commit panic propagates");
            assert!(
                watch.committed(),
                "the transaction committed before the panic"
            );
        }

        /// A body error rolls the write back: the watch stays unmarked.
        #[test]
        fn a_rolled_back_write_reports_no_commit() {
            let def = things();
            let (_tmp, infra, _rx) = test_infra_with_events(def.clone());
            let ctx = ServiceContext::collection("things", &def)
                .infra(&infra)
                .build();

            let watch = CommitWatch::open();

            let result: Result<()> = run_pool_write(
                &ctx,
                None,
                |_| Err(ServiceError::Internal(anyhow!("body failed"))),
                |_, (): &()| {},
            );

            assert!(result.is_err());
            assert!(!watch.committed());
        }
    }
}
