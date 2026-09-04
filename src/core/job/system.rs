//! System-job slug constants — names for the framework's
//! built-in background jobs (image conversion, email delivery, …).
//!
//! These are re-exports of constants defined alongside the subsystem
//! that owns each job, gathered here so callers can discover the full
//! list in one place and so dispatch code in `scheduler/runner.rs`
//! can pattern-match on a single import.
//!
//! Each constant's source of truth lives next to the queueing /
//! handler code for that subsystem — see the linked modules below.

/// `_system_email` — email delivery. Defined in [`crate::core::email::queue`].
pub use crate::core::email::SYSTEM_EMAIL_JOB;

/// `_system_image_convert` — AVIF / WebP image format conversion.
/// Defined in [`crate::core::upload::queue`].
pub use crate::core::upload::SYSTEM_IMAGE_CONVERT_JOB;

/// `_system_bulk` — queued bulk create/update/delete (`queue = true` on the
/// gRPC/MCP bulk operations). Defined HERE (no core subsystem owns it):
/// queueing lives in `service::jobs::bulk_queue`, execution in
/// `scheduler::bulk`.
pub const SYSTEM_BULK_JOB: &str = "_system_bulk";

/// Queue name for `_system_bulk` runs. Concurrency defaults to 1 — bulk
/// writes hold the write transaction for their whole batch, so running them
/// serially is the sane default.
pub const SYSTEM_BULK_QUEUE: &str = "bulk";

/// All built-in system-job slugs as a static slice. Useful for
/// validation, admin tooling, and future enumeration over the set.
///
/// **Adding a new system job requires updating three places:**
/// 1. Define the slug constant in the subsystem's `queue.rs`.
/// 2. Re-export it here and append to `SYSTEM_JOB_SLUGS`.
/// 3. Add a seeding block to
///    [`crate::config::JobsConfig::apply_queue_defaults`] so a fresh
///    `crap.toml` resolves sane `timeout` / `retries` /
///    `concurrency` for the new queue — the regression test
///    `system_queues_have_all_defaults_seeded` guards this.
pub const SYSTEM_JOB_SLUGS: &[&str] =
    &[SYSTEM_EMAIL_JOB, SYSTEM_IMAGE_CONVERT_JOB, SYSTEM_BULK_JOB];

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn system_slugs_are_underscore_prefixed() {
        for slug in SYSTEM_JOB_SLUGS {
            assert!(
                slug.starts_with("_system_"),
                "system job slug '{slug}' must start with '_system_' to avoid clashing with user-defined slugs"
            );
        }
    }

    #[test]
    fn system_slugs_are_unique() {
        let mut seen = HashSet::new();
        for slug in SYSTEM_JOB_SLUGS {
            assert!(seen.insert(*slug), "duplicate system job slug: {slug}");
        }
    }
}
