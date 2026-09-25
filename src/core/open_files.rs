//! The process's open-file (`RLIMIT_NOFILE`) limit: read, and raised at
//! startup.
//!
//! Every client connection, database connection and open upload holds a file
//! descriptor. Many systems start processes with a small soft limit (1024 on
//! most Linux distributions, 256 on macOS) under a much larger hard limit, so
//! a server left at the soft limit runs out of descriptors — and refuses
//! connections — long before the machine does. `serve` and `work` raise the
//! soft limit to the hard limit before anything sizes itself from it.

#[cfg(unix)]
use std::io::{Error as IoError, ErrorKind, Result as IoResult};

#[cfg(unix)]
use tracing::{debug, info, warn};

/// The soft limit asked for when the hard limit is unlimited. An unlimited
/// soft limit is refused on some platforms (macOS), and Linux caps a process's
/// descriptors at `fs.nr_open` (1 048 576 by default) anyway.
#[cfg(any(unix, test))]
const UNLIMITED_TARGET: u64 = 1_048_576;

/// The fallback asked for when the first raise is refused: macOS's
/// per-process `OPEN_MAX`, which its kernel accepts where a larger value is
/// refused.
#[cfg(any(unix, test))]
const FALLBACK_TARGET: u64 = 10_240;

/// The process's open-file limits as `(soft, hard)`; an unlimited value reads
/// as `u64::MAX`. `None` when they cannot be read.
#[cfg(unix)]
#[must_use]
pub fn open_file_limits() -> Option<(u64, u64)> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };

    // SAFETY: getrlimit(2) only writes the struct it is handed, which lives
    // for the whole call.
    let status = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) };

    if status != 0 {
        return None;
    }

    Some((widen(limit.rlim_cur)?, widen(limit.rlim_max)?))
}

/// Platforms without `getrlimit` report no descriptor limit.
#[cfg(not(unix))]
#[must_use]
pub fn open_file_limits() -> Option<(u64, u64)> {
    None
}

/// A platform `rlim_t` (`u32` or `u64`, depending on the target) as `u64`,
/// unlimited as `u64::MAX`.
#[cfg(unix)]
fn widen(value: libc::rlim_t) -> Option<u64> {
    if value == libc::RLIM_INFINITY {
        return Some(u64::MAX);
    }

    convert(value)
}

/// A limit as a platform `rlim_t`, `u64::MAX` as unlimited.
#[cfg(unix)]
fn narrow(value: u64) -> IoResult<libc::rlim_t> {
    if value == u64::MAX {
        return Ok(libc::RLIM_INFINITY);
    }

    convert(value).ok_or_else(|| IoError::from(ErrorKind::InvalidInput))
}

/// An integer conversion that is lossless on some targets and fallible on
/// others.
#[cfg(unix)]
fn convert<T: TryInto<U>, U>(value: T) -> Option<U> {
    value.try_into().ok()
}

/// The soft limits to try, in order, to raise `soft` towards `hard`: the hard
/// limit itself (or [`UNLIMITED_TARGET`] for an unlimited one), then
/// [`FALLBACK_TARGET`] — each only when it is a raise.
#[cfg(any(unix, test))]
fn raise_targets(soft: u64, hard: u64) -> Vec<u64> {
    let first = if hard == u64::MAX {
        UNLIMITED_TARGET
    } else {
        hard
    };

    let mut targets: Vec<u64> = [first, FALLBACK_TARGET.min(hard)]
        .into_iter()
        .filter(|&target| target > soft)
        .collect();
    targets.dedup();

    targets
}

/// Set the soft limit to `soft`, keeping the hard limit `hard`.
#[cfg(unix)]
fn set_soft_limit(soft: u64, hard: u64) -> IoResult<()> {
    let limit = libc::rlimit {
        rlim_cur: narrow(soft)?,
        rlim_max: narrow(hard)?,
    };

    // SAFETY: setrlimit(2) only reads the struct it is handed, which lives for
    // the whole call.
    let status = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const limit) };

    if status != 0 {
        return Err(IoError::last_os_error());
    }

    Ok(())
}

/// Raise the process's open-file soft limit to its hard limit, logging the old
/// and new values. Never fails the startup: a limit that cannot be read or
/// raised is logged and left as it is.
#[cfg(unix)]
pub fn raise_open_file_limit() {
    let Some((soft, hard)) = open_file_limits() else {
        warn!("Could not read the open-file limit; leaving it as it is");
        return;
    };

    let targets = raise_targets(soft, hard);

    if targets.is_empty() {
        debug!(
            open_file_limit = soft,
            "Open-file limit already at its maximum"
        );
        return;
    }

    for target in targets {
        match set_soft_limit(target, hard) {
            Ok(()) => {
                info!("Raised the open-file limit from {soft} to {target}");
                return;
            }
            Err(e) => debug!("Raising the open-file limit to {target} was refused: {e}"),
        }
    }

    warn!("Could not raise the open-file limit above {soft} (hard limit {hard})");
}

/// Platforms without `setrlimit` have no limit to raise.
#[cfg(not(unix))]
pub fn raise_open_file_limit() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_soft_limit_is_raised_to_a_finite_hard_limit() {
        assert_eq!(raise_targets(1024, 524_288), vec![524_288, FALLBACK_TARGET]);
    }

    #[test]
    fn an_unlimited_hard_limit_is_approached_with_a_finite_target() {
        assert_eq!(
            raise_targets(256, u64::MAX),
            vec![UNLIMITED_TARGET, FALLBACK_TARGET]
        );
    }

    #[test]
    fn a_limit_already_at_the_hard_limit_is_left_alone() {
        assert!(raise_targets(4096, 4096).is_empty());
        assert!(raise_targets(u64::MAX, u64::MAX).is_empty());
    }

    /// The fallback never exceeds the hard limit, and is only a raise.
    #[test]
    fn the_fallback_respects_the_hard_limit_and_the_current_value() {
        assert_eq!(raise_targets(1024, 4096), vec![4096]);
        assert_eq!(raise_targets(20_000, 30_000), vec![30_000]);
    }

    #[cfg(unix)]
    #[test]
    fn the_limits_are_readable_and_ordered() {
        let (soft, hard) = open_file_limits().expect("readable");

        assert!(soft > 0 && soft <= hard);
    }

    /// Raising is idempotent and leaves the limit readable: after a raise the
    /// soft limit is no lower than before.
    #[cfg(unix)]
    #[test]
    fn raising_never_lowers_the_limit() {
        let (before, _) = open_file_limits().expect("readable");

        raise_open_file_limit();

        let (after, hard) = open_file_limits().expect("readable");
        assert!(after >= before && after <= hard);
    }
}
