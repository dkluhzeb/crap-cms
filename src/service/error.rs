//! Typed service-layer error enum with classification and surface-specific conversion.

use std::fmt;

use anyhow::anyhow;
use tracing::error;

use crate::{
    core::ValidationError,
    db::{
        ConstraintKind::{ForeignKey, Unique},
        constraint_kind, is_transient,
        query::DocumentNotFound,
    },
};

/// Typed service-layer errors that callers can match on for surface-specific handling.
#[derive(Debug)]
pub enum ServiceError {
    /// Collection-level access denied (read, create, update, delete, trash).
    AccessDenied(String),
    /// Document not found.
    NotFound(String),
    /// Ref count protection: document is referenced by others.
    Referenced { id: String, count: i64 },
    /// A bulk operation matched more documents than the configured limit
    /// (`server.bulk_max_documents`). Nothing was changed.
    LimitExceeded(String),
    /// Structured per-field validation errors (required, unique, custom Lua validators).
    Validation(ValidationError),
    /// Hook execution error with a user-facing message.
    HookError(String),
    /// Unique constraint violation with the offending field name.
    UniqueViolation(String),
    /// Foreign-key constraint violation: the write points at a row that does
    /// not exist, or removes a row another table still references. Distinct
    /// from [`Self::UniqueViolation`] — nothing is "already there", so a client
    /// told to pick another value would be told the wrong thing.
    ForeignKeyViolation(String),
    /// Account is locked — authentication or token consumption denied.
    AccountLocked,
    /// Email not verified — login denied.
    EmailNotVerified,
    /// Invalid credentials (email not found or password mismatch).
    InvalidCredentials,
    /// Invalid or expired token (reset or verification).
    InvalidToken {
        kind: &'static str,
        reason: &'static str,
    },
    /// Transient DB error (locked, busy, pool timeout) — caller should retry.
    Transient(anyhow::Error),
    /// Any other internal error.
    Internal(anyhow::Error),
}

impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AccessDenied(msg)
            | Self::NotFound(msg)
            | Self::HookError(msg)
            | Self::LimitExceeded(msg) => {
                write!(f, "{msg}")
            }
            Self::Referenced { id, count } => {
                write!(f, "Cannot delete '{id}': referenced by {count} document(s)")
            }
            Self::Validation(ve) => write!(f, "{ve}"),
            Self::UniqueViolation(field) if field.is_empty() => {
                write!(f, "Unique constraint violated")
            }
            Self::UniqueViolation(field) => {
                write!(f, "Unique constraint violated for field '{field}'")
            }
            Self::ForeignKeyViolation(constraint) if constraint.is_empty() => {
                write!(f, "Foreign key constraint violated")
            }
            Self::ForeignKeyViolation(constraint) => {
                write!(f, "Foreign key constraint violated: '{constraint}'")
            }
            Self::AccountLocked => write!(f, "Account is locked"),
            Self::EmailNotVerified => write!(f, "Email not verified"),
            Self::InvalidCredentials => write!(f, "Invalid credentials"),
            Self::InvalidToken { kind, reason } => {
                write!(f, "Invalid {kind} token: {reason}")
            }
            Self::Transient(e) | Self::Internal(e) => write!(f, "{e:#}"),
        }
    }
}

impl std::error::Error for ServiceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Internal(e) | Self::Transient(e) => Some(e.as_ref()),
            _ => None,
        }
    }
}

impl From<anyhow::Error> for ServiceError {
    fn from(e: anyhow::Error) -> Self {
        // A `ServiceError` raised through a layer that speaks `anyhow` (e.g. the
        // hooks layer's `check_access`) round-trips back to its typed self —
        // so a `HookError` from access-constraint validation maps to
        // `invalid_argument`, not `Internal` (which clients retry on).
        let e = match e.downcast::<ServiceError>() {
            Ok(se) => return se,
            Err(e) => e,
        };

        if let Some(typed) = downcast_typed(&e) {
            return typed;
        }

        Self::Internal(e)
    }
}

/// The variants recoverable from an `anyhow` chain by downcast alone — no
/// message matching, no backend knowledge.
///
/// Both [`From<anyhow::Error>`] and [`ServiceError::classify`] start here, so a
/// surface that classifies directly cannot report a structured validation
/// failure or a missing document as an internal fault. `DocumentNotFound` is
/// raised by `query::update` / `query::update_partial` when the UPDATE matched
/// zero rows; without this, Update / `UpdateMany` on a missing id comes back as
/// `Internal` over gRPC — which production clients retry on, masking the bug.
fn downcast_typed(e: &anyhow::Error) -> Option<ServiceError> {
    if let Some(ve) = e.downcast_ref::<ValidationError>() {
        return Some(ServiceError::Validation(ve.clone()));
    }

    e.downcast_ref::<DocumentNotFound>()
        .map(|dnf| ServiceError::NotFound(dnf.to_string()))
}

impl From<ValidationError> for ServiceError {
    fn from(ve: ValidationError) -> Self {
        Self::Validation(ve)
    }
}

impl ServiceError {
    /// Classify an anyhow error into the appropriate `ServiceError` variant.
    ///
    /// The typed driver cause decides first — [`crate::db::constraint_kind`]
    /// and [`crate::db::is_transient`] read the error's SQLSTATE / `SQLite`
    /// result code, which no server locale translates. String matching is the
    /// fallback for causes that arrive with the typed error already erased (a
    /// hook re-wrapping a failure as a message, r2d2's opaque pool error).
    /// `db_kind` selects the backend-specific fallback patterns (`"sqlite"`,
    /// `"postgres"`).
    #[must_use]
    pub fn classify(e: anyhow::Error, db_kind: &str) -> Self {
        // Structured validation errors and a typed not-found — preserve them.
        if let Some(typed) = downcast_typed(&e) {
            return typed;
        }

        // Match against the FULL cause chain (`{:#}`), not just the top
        // message: helpers routinely wrap errors in an anyhow context
        // (`DbPool::get` adds "Failed to get DB connection"), and matching
        // `to_string()` alone made every wrapped transient cause — pool
        // timeout, SQLITE_BUSY — classify as internal (500) instead of
        // transient (unavailable/503) on every surface.
        let msg = format!("{e:#}");

        if is_transient(&e) || transient_message(&msg, db_kind) {
            return Self::Transient(e);
        }

        if let Some(violation) = constraint_violation(&e, &msg, db_kind) {
            return violation;
        }

        // A hook that called `crap.validation_error` encoded its field errors
        // into the message; decode them back so the failure lands on the
        // offending input instead of in a generic hook-error banner.
        if let Some(ve) = ValidationError::from_hook_message(&msg) {
            return Self::Validation(ve);
        }

        // Hook/runtime errors — user-facing messages. A reference to a
        // vanished target is the caller's mistake (a stale or mistyped id),
        // reported as such rather than as a server fault.
        if msg.contains("hook error:")
            || msg.contains("validation error:")
            || msg.contains("Validation failed:")
            || msg.contains("runtime error:")
            || msg.contains("cannot reference ")
        {
            return Self::HookError(strip_lua_traceback(&msg));
        }

        Self::Internal(e)
    }

    /// Re-classify an `Internal` error using backend-specific string patterns.
    ///
    /// Non-Internal variants pass through unchanged. This is used at the surface
    /// boundary (gRPC, admin) where the backend kind is known.
    #[must_use]
    pub fn reclassify(self, db_kind: &str) -> Self {
        match self {
            Self::Internal(e) => Self::classify(e, db_kind),
            other => other,
        }
    }

    /// Convert to an `anyhow::Error`, preserving the original error chain for Internal/Transient.
    #[must_use]
    pub fn into_anyhow(self) -> anyhow::Error {
        match self {
            Self::Internal(inner) | Self::Transient(inner) => inner,
            Self::Validation(ve) => anyhow::Error::new(ve),
            other => anyhow!("{other}"),
        }
    }

    /// Like [`Self::into_anyhow`], but **scrubs** `Internal`/`Transient`
    /// for client-facing surfaces: their inner
    /// chains carry raw backend/pool text (DB identifiers, driver
    /// vocabulary) that gRPC and the REST upload surface already hide —
    /// MCP must match. The full chain is logged server-side first.
    #[must_use]
    pub fn into_anyhow_scrubbed(self) -> anyhow::Error {
        match self {
            Self::Internal(inner) => {
                error!("Internal error (scrubbed from client): {inner:#}");
                anyhow!("Internal error")
            }
            Self::Transient(inner) => {
                error!("Transient error (scrubbed from client): {inner:#}");
                anyhow!("Temporarily unavailable, retry")
            }
            other => other.into_anyhow(),
        }
    }
}

/// The transient conditions recognizable only from text, for causes that reach
/// us with the typed driver error erased.
///
/// Both timeout spellings are needed: r2d2's pool timeout is lowercase ("timed
/// out waiting for connection") and exposes no variant to match on, while the
/// capitalized form covers other wait-timeout sources.
fn transient_message(msg: &str, db_kind: &str) -> bool {
    if msg.contains("Timed out waiting")
        || msg.contains("timed out waiting for connection")
        || msg.contains("connection pool")
    {
        return true;
    }

    match db_kind {
        "sqlite" => {
            msg.contains("database is locked")
                || msg.contains("database is busy")
                || msg.contains("SQLITE_BUSY")
                || msg.contains("SQLITE_LOCKED")
        }
        "postgres" => {
            msg.contains("connection refused")
                || msg.contains("connection closed")
                || msg.contains("too many clients")
                || msg.contains("remaining connection slots are reserved")
        }
        _ => false,
    }
}

/// The constraint violation `e` reports, if any.
///
/// The kind comes from the driver's code; only the *field name* is read from
/// the message, and only for `SQLite`, whose "UNIQUE constraint failed:
/// table.column" wording is fixed English emitted by the library itself.
/// Postgres names the index rather than the field, so its payload stays empty
/// and the surfaces render the constraint without one.
fn constraint_violation(e: &anyhow::Error, msg: &str, db_kind: &str) -> Option<ServiceError> {
    const UNIQUE_PREFIX: &str = "UNIQUE constraint failed: ";

    // `find` (not `strip_prefix`): the SQLite message sits behind context
    // layers in the `{:#}` chain rather than at the start.
    let unique_field = || {
        msg.find(UNIQUE_PREFIX).map_or_else(String::new, |pos| {
            let rest = &msg[pos + UNIQUE_PREFIX.len()..];

            rest.find('.')
                .map_or_else(|| rest.to_string(), |dot| rest[dot + 1..].to_string())
        })
    };

    match constraint_kind(e) {
        Some(Unique) => return Some(ServiceError::UniqueViolation(unique_field())),
        Some(ForeignKey) => return Some(ServiceError::ForeignKeyViolation(String::new())),
        None => {}
    }

    // Message fallback, for a cause that lost its typed error on the way here.
    if msg.contains(UNIQUE_PREFIX) || msg.contains("duplicate key value violates unique constraint")
    {
        return Some(ServiceError::UniqueViolation(unique_field()));
    }

    if msg.contains("FOREIGN KEY constraint failed")
        || (db_kind == "postgres" && msg.contains("violates foreign key constraint"))
    {
        return Some(ServiceError::ForeignKeyViolation(String::new()));
    }

    None
}

/// Drop the Lua stack traceback mlua appends to every Lua-originated error.
///
/// The message before it is the user-facing text (`error("…")` in a hook);
/// the traceback names internal hook modules, line numbers, and functions,
/// which belongs in the server log, not in a client response or admin toast.
fn strip_lua_traceback(msg: &str) -> String {
    msg.split("\nstack traceback:")
        .next()
        .unwrap_or(msg)
        .trim_end()
        .to_string()
}

#[cfg(test)]
mod tests {
    use anyhow::{Context as _, anyhow};

    use super::*;
    use crate::core::{FieldError, ValidationError};

    /// A hook error carries only its message to the client: the Lua
    /// traceback mlua appends (module paths, line numbers) is stripped.
    #[test]
    fn hook_error_drops_the_lua_traceback() {
        let raw = anyhow!(
            "runtime error: hook error: title is taken\nstack traceback:\n\t[C]: in function 'error'\n\thooks/posts.lua:12: in function <hooks/posts.lua:10>"
        );
        let ServiceError::HookError(msg) = ServiceError::classify(raw, "sqlite") else {
            panic!("expected HookError");
        };
        assert_eq!(msg, "runtime error: hook error: title is taken");
    }

    /// A reference to a vanished target is a caller error (400), not an
    /// internal fault.
    #[test]
    fn dangling_reference_write_is_a_hook_error_not_internal() {
        let raw = anyhow!("cannot reference authors/ghost: target no longer exists");
        assert!(matches!(
            ServiceError::classify(raw, "sqlite"),
            ServiceError::HookError(m) if m.contains("authors/ghost")
        ));
    }

    /// `into_anyhow_scrubbed` must hide the raw Internal/Transient chain
    /// (backend/pool text — DB identifiers, driver vocabulary) from
    /// client-facing surfaces, while validation errors stay verbatim (they
    /// are user-facing by design). This is the invariant the MCP job tools
    /// and gRPC surfaces rely on.
    #[test]
    fn into_anyhow_scrubbed_hides_internal_and_transient_text() {
        let internal = ServiceError::Internal(anyhow!("relation \"posts\" secret column x"));
        assert_eq!(
            internal.into_anyhow_scrubbed().to_string(),
            "Internal error",
            "raw backend text must never reach the client"
        );

        let transient = ServiceError::Transient(anyhow!("pool timed out at 10.0.0.5:5432"));
        let msg = transient.into_anyhow_scrubbed().to_string();
        assert!(
            !msg.contains("10.0.0.5"),
            "transient text must be scrubbed, got: {msg}"
        );

        // A validation error is user-facing and must survive scrubbing.
        let ve = ServiceError::Validation(ValidationError::new(vec![FieldError::new(
            "title",
            "is required",
        )]));
        assert!(ve.into_anyhow_scrubbed().to_string().contains("required"));
    }

    // ── classify ────────────────────────────────────────────────────

    /// Regression: a transient cause hidden behind an anyhow context layer
    /// (the shape `DbPool::get` produces — "Failed to get DB connection:
    /// Timed out waiting…") must still classify as `Transient`. `classify`
    /// used to match only `to_string()` (the outermost context), so every
    /// wrapped pool timeout / `SQLITE_BUSY` landed as `Internal` (500) instead
    /// of `Transient` (unavailable/503) on every surface.
    #[test]
    fn classify_matches_transient_cause_behind_context() {
        // r2d2's actual (lowercase) pool-timeout wording.
        let e = Err::<(), _>(anyhow!("timed out waiting for connection"))
            .context("Failed to get DB connection")
            .unwrap_err();
        assert!(matches!(
            ServiceError::classify(e, "sqlite"),
            ServiceError::Transient(_)
        ));

        let e = Err::<(), _>(anyhow!("database is locked"))
            .context("Failed to update document x in 'posts'")
            .unwrap_err();
        assert!(matches!(
            ServiceError::classify(e, "sqlite"),
            ServiceError::Transient(_)
        ));
    }

    /// The unique-violation field extraction still works when the `SQLite`
    /// message sits behind a context layer in the `{:#}` chain.
    #[test]
    fn classify_unique_violation_behind_context() {
        let e = Err::<(), _>(anyhow!("UNIQUE constraint failed: users.email"))
            .context("Failed to create document in 'users'")
            .unwrap_err();
        let ServiceError::UniqueViolation(field) = ServiceError::classify(e, "sqlite") else {
            panic!("expected UniqueViolation");
        };
        assert_eq!(field, "email");
    }

    #[test]
    fn classify_validation_error_preserved() {
        let ve = ValidationError::new(vec![FieldError::new("title", "required")]);
        let e = anyhow::Error::new(ve);
        let se = ServiceError::classify(e, "sqlite");
        assert!(matches!(se, ServiceError::Validation(_)));
    }

    #[test]
    fn classify_transient_sqlite_locked() {
        let e = anyhow!("database is locked");
        let se = ServiceError::classify(e, "sqlite");
        assert!(matches!(se, ServiceError::Transient(_)));
    }

    #[test]
    fn classify_transient_sqlite_busy() {
        let e = anyhow!("SQLITE_BUSY error");
        let se = ServiceError::classify(e, "sqlite");
        assert!(matches!(se, ServiceError::Transient(_)));
    }

    #[test]
    fn classify_transient_pool_timeout() {
        let e = anyhow!("Timed out waiting for connection pool");
        let se = ServiceError::classify(e, "sqlite");
        assert!(matches!(se, ServiceError::Transient(_)));
    }

    #[test]
    fn classify_transient_postgres_connection_refused() {
        let e = anyhow!("connection refused");
        let se = ServiceError::classify(e, "postgres");
        assert!(matches!(se, ServiceError::Transient(_)));
    }

    #[test]
    fn classify_unique_violation_sqlite() {
        let e = anyhow!("UNIQUE constraint failed: users.email");
        let se = ServiceError::classify(e, "sqlite");
        assert!(matches!(se, ServiceError::UniqueViolation(ref f) if f == "email"));
    }

    #[test]
    fn classify_unique_violation_postgres() {
        let e = anyhow!("duplicate key value violates unique constraint");
        let se = ServiceError::classify(e, "postgres");
        assert!(matches!(se, ServiceError::UniqueViolation(_)));
    }

    /// A foreign-key violation is its own variant on both backends.
    ///
    /// Postgres' wording was mapped to `UniqueViolation`, so a write pointing
    /// at a row that does not exist came back as `ALREADY_EXISTS` / 409-conflict
    /// — telling the client to pick another value for something *missing*.
    /// `SQLite`'s wording matched nothing at all and landed as `Internal` (500,
    /// and retried by clients).
    #[test]
    fn classify_foreign_key_violation_on_both_backends() {
        assert!(matches!(
            ServiceError::classify(anyhow!("FOREIGN KEY constraint failed"), "sqlite"),
            ServiceError::ForeignKeyViolation(_)
        ));

        assert!(matches!(
            ServiceError::classify(
                anyhow!("insert violates foreign key constraint \"posts_author_fkey\""),
                "postgres"
            ),
            ServiceError::ForeignKeyViolation(_)
        ));
    }

    /// A typed `DocumentNotFound` is recovered by `classify`, not only by
    /// `From<anyhow::Error>`. The surfaces that classify directly (the gRPC
    /// content service, the REST upload path, the account and bulk-queue
    /// handlers) reported an update of a missing id as `Internal` — a 500 that
    /// production clients retry — instead of not-found.
    #[test]
    fn classify_recovers_a_typed_document_not_found() {
        let e = Err::<(), _>(anyhow::Error::new(DocumentNotFound {
            slug: "posts".into(),
            id: "missing".into(),
        }))
        .context("Failed to update document")
        .unwrap_err();

        let ServiceError::NotFound(msg) = ServiceError::classify(e, "sqlite") else {
            panic!("expected NotFound");
        };
        assert!(msg.contains("missing"), "{msg}");
    }

    #[test]
    fn classify_hook_error() {
        let e = anyhow!("hook error: title is required");
        let se = ServiceError::classify(e, "sqlite");
        assert!(matches!(se, ServiceError::HookError(_)));
    }

    #[test]
    fn classify_validation_string() {
        let e = anyhow!("Validation failed: email invalid");
        let se = ServiceError::classify(e, "sqlite");
        assert!(matches!(se, ServiceError::HookError(_)));
    }

    #[test]
    fn classify_unknown_falls_to_internal() {
        let e = anyhow!("something unexpected");
        let se = ServiceError::classify(e, "sqlite");
        assert!(matches!(se, ServiceError::Internal(_)));
    }

    // ── reclassify ──────────────────────────────────────────────────

    #[test]
    fn reclassify_internal_to_transient() {
        let se = ServiceError::Internal(anyhow!("database is locked"));
        let re = se.reclassify("sqlite");
        assert!(matches!(re, ServiceError::Transient(_)));
    }

    #[test]
    fn reclassify_non_internal_passes_through() {
        let se = ServiceError::AccessDenied("denied".into());
        let re = se.reclassify("sqlite");
        assert!(matches!(re, ServiceError::AccessDenied(_)));
    }

    // ── From<anyhow::Error> ─────────────────────────────────────────

    #[test]
    fn from_anyhow_validation_extracted() {
        let ve = ValidationError::new(vec![FieldError::new("x", "bad")]);
        let e = anyhow::Error::new(ve);
        let se: ServiceError = e.into();
        assert!(matches!(se, ServiceError::Validation(_)));
    }

    #[test]
    fn from_anyhow_generic_becomes_internal() {
        let e = anyhow!("generic error");
        let se: ServiceError = e.into();
        assert!(matches!(se, ServiceError::Internal(_)));
    }

    /// A `ServiceError` raised through the hooks layer (which speaks `anyhow`)
    /// round-trips back to its typed self, so a `HookError` from access-constraint
    /// validation maps to `invalid_argument`, not `Internal`. Regression for the
    /// access-constraint chokepoint surfacing the right gRPC status.
    #[test]
    fn from_anyhow_recovers_typed_service_error() {
        let e = anyhow::Error::new(ServiceError::HookError("bad access constraint".into()));
        let se: ServiceError = e.into();
        match se {
            ServiceError::HookError(msg) => assert_eq!(msg, "bad access constraint"),
            other => panic!("expected HookError, got {other:?}"),
        }
    }

    // ── into_anyhow ─────────────────────────────────────────────────

    #[test]
    fn into_anyhow_preserves_internal() {
        let se = ServiceError::Internal(anyhow!("inner error"));
        let e = se.into_anyhow();
        assert!(e.to_string().contains("inner error"));
    }

    #[test]
    fn into_anyhow_validation_roundtrips() {
        let ve = ValidationError::new(vec![FieldError::new("a", "b")]);
        let se = ServiceError::Validation(ve);
        let e = se.into_anyhow();
        assert!(e.downcast_ref::<ValidationError>().is_some());
    }

    // ── Display ─────────────────────────────────────────────────────

    #[test]
    fn display_referenced() {
        let se = ServiceError::Referenced {
            id: "doc-1".into(),
            count: 3,
        };
        assert_eq!(
            se.to_string(),
            "Cannot delete 'doc-1': referenced by 3 document(s)"
        );
    }

    #[test]
    fn display_unique_violation() {
        let se = ServiceError::UniqueViolation("email".into());
        assert_eq!(
            se.to_string(),
            "Unique constraint violated for field 'email'"
        );
    }
}
