//! Verification email queued during a transaction for post-commit sending.

use std::{cell::RefCell, rc::Rc};

use crate::service::ServiceContext;

/// A verification email waiting to be sent after transaction commit.
pub struct PendingVerification {
    pub slug: String,
    pub doc_id: String,
    pub email: String,
}

/// Shared queue for verification emails accumulated during a transaction.
pub type VerificationQueue = Rc<RefCell<Vec<PendingVerification>>>;

/// Flush all queued verification emails, sending each via the parent's pool + email context.
pub(crate) fn flush_verification_queue(ctx: &ServiceContext, queue: &VerificationQueue) {
    let Some(pool) = ctx.pool else { return };
    let Some(ref email_ctx) = ctx.email_ctx else {
        return;
    };

    let pending: Vec<PendingVerification> = queue.borrow_mut().drain(..).collect();

    for v in pending {
        email_ctx.send_verification(pool.clone(), v.slug, v.doc_id, v.email);
    }
}
