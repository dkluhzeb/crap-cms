//! Pagination limit, population depth and optional-limit clamping.

/// Clamp a requested limit to the configured default/max.
///
/// - `None` → `default_limit`
/// - `Some(v)` → clamped to `[1, max_limit]`
#[must_use]
pub fn apply_pagination_limits(requested: Option<i64>, default_limit: i64, max_limit: i64) -> i64 {
    match requested {
        None => default_limit,
        Some(v) => v.max(1).min(max_limit),
    }
}

/// Resolve a requested relationship-population depth into `[0, max_depth]`.
///
/// `None` (no explicit depth) uses the configured `default_depth`; a negative
/// value floors to 0; everything is capped at `max_depth`. One helper so every
/// read surface (Lua / gRPC / MCP / admin) resolves depth identically — the
/// surfaces previously diverged (some defaulted to `default_depth`, some to 0,
/// MCP never floored a negative), so the `[depth] default_depth` knob was
/// honored inconsistently.
#[must_use]
pub fn clamp_depth(requested: Option<i32>, default_depth: i32, max_depth: i32) -> i32 {
    requested.unwrap_or(default_depth).max(0).min(max_depth)
}

/// Floor an optional `limit`/`offset` at 0, preserving `None`.
///
/// Used where `None` is an intended, separately-bounded "no explicit limit"
/// contract (e.g. version history, capped by `max_versions`): we must not turn
/// `None` into a cap, but a negative `Some(-1)` must never become an unbounded
/// `LIMIT -1` read (which `SQLite` treats as *no limit* — a fail-open bypass).
/// The floor at 0 is fail-closed (`LIMIT 0` / `OFFSET 0`). Lives here in
/// `db::query` so every read surface (Lua / gRPC / MCP) and the service layer
/// share one floor without a layering inversion.
#[must_use]
pub fn floor_optional_limit(limit: Option<i64>) -> Option<i64> {
    limit.map(|l| l.max(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── apply_pagination_limits tests ──────────────────────────────────

    #[test]
    fn pagination_limits_default_when_none() {
        assert_eq!(apply_pagination_limits(None, 100, 1000), 100);
    }

    #[test]
    fn pagination_limits_clamp_max() {
        assert_eq!(apply_pagination_limits(Some(5000), 100, 1000), 1000);
    }

    #[test]
    fn pagination_limits_minimum_one() {
        assert_eq!(apply_pagination_limits(Some(0), 100, 1000), 1);
        assert_eq!(apply_pagination_limits(Some(-5), 100, 1000), 1);
    }

    #[test]
    fn pagination_limits_passthrough() {
        assert_eq!(apply_pagination_limits(Some(50), 100, 1000), 50);
    }
}
