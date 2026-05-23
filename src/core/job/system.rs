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

/// All built-in system-job slugs as a static slice. Useful for
/// validation, admin tooling, and future enumeration over the set.
pub const SYSTEM_JOB_SLUGS: &[&str] = &[SYSTEM_EMAIL_JOB, SYSTEM_IMAGE_CONVERT_JOB];

#[cfg(test)]
mod tests {
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
        let mut seen = std::collections::HashSet::new();
        for slug in SYSTEM_JOB_SLUGS {
            assert!(seen.insert(*slug), "duplicate system job slug: {slug}");
        }
    }
}
