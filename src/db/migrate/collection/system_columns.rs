//! Single source of truth for the internal system/auth column definitions,
//! shared by the CREATE TABLE path (`create::collect_system_columns`) and the
//! ALTER TABLE reconcile path (`alter::add_*_columns`) so the two can't drift on
//! a column's type or default.
//!
//! `_deleted_at` is intentionally absent: its type is backend-dependent
//! (`DbConnection::timestamp_column_type()`), so both paths build it inline from
//! that trait method rather than a static literal.

/// The `_status` draft column (present when the collection has drafts).
pub(super) const DRAFT_STATUS_COLUMN: &str = "_status TEXT NOT NULL DEFAULT 'published'";

/// The incoming-reference counter present on every collection.
pub(super) const REF_COUNT_COLUMN: &str = "_ref_count INTEGER NOT NULL DEFAULT 0";

/// Core auth columns present on every auth collection.
pub(super) const AUTH_COLUMNS: &[&str] = &[
    "_password_hash TEXT",
    "_reset_token TEXT",
    "_reset_token_exp INTEGER",
    "_locked INTEGER DEFAULT 0",
    "_settings TEXT",
    "_session_version INTEGER DEFAULT 0",
];

/// Verify-email columns, present when the auth config requires email verification.
pub(super) const VERIFY_EMAIL_COLUMNS: &[&str] = &[
    "_verified INTEGER DEFAULT 0",
    "_verification_token TEXT",
    "_verification_token_exp INTEGER",
];

/// MFA columns, present when the auth config enables an MFA mode.
pub(super) const MFA_COLUMNS: &[&str] = &["_mfa_code TEXT", "_mfa_code_exp INTEGER"];

/// TOTP columns, present when `mfa = "totp"`: the sealed shared secret, the
/// enrollment-confirmed flag, and the last accepted time step (replay guard).
pub(super) const TOTP_COLUMNS: &[&str] = &[
    "_totp_secret TEXT",
    "_totp_confirmed INTEGER DEFAULT 0",
    "_totp_last_step INTEGER",
];
