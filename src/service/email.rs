//! Verification- and password-reset email operations.
//!
//! Both the sign-up email and the self-service resend mint their token
//! through [`issue_verification_token`] and render through the same
//! [`VerificationMailer`], so the two paths cannot drift in link shape,
//! template, or lifetime. The password-reset email works the same way through
//! [`ResetMailer`], shared by the admin form and the gRPC endpoint.
//!
//! Minting the token and inserting the `_system_email` job row are one
//! atomic step on the caller's connection — for a sign-up, that connection is
//! the account's own write transaction, so the account and its way to verify
//! commit together. Only the SMTP send itself is deferred, to the job queue,
//! where it has a retry budget. The detached `spawn_blocking` entry points
//! below are the fallback for callers that hold no connection; they open
//! their own transaction to keep the same guarantee.

use std::sync::Arc;

use anyhow::Context as _;
use tracing::{error, warn};

use crate::{
    config::{EmailConfig, LocaleConfig, ServerConfig},
    core::{
        CollectionDefinition,
        email::{
            EmailJobData, EmailRenderer, PasswordResetEmailContext, VerifyEmailContext,
            is_configured, queue_email,
        },
    },
    db::{BoxedConnection, DbConnection, DbPool},
    service::{
        ServiceContext, ServiceError,
        auth::{
            VERIFICATION_TOKEN_EXPIRY, generate_reset_token, generate_verification_token,
            issue_verification_token,
        },
    },
};

/// The account a verification email is for. Built at its single call site and
/// consumed there, so a plain struct literal is enough.
pub(crate) struct VerificationRecipient<'a> {
    pub slug: &'a str,
    pub user_id: &'a str,
    pub email: &'a str,
}

/// Everything needed to render and queue a verification email. Built once
/// per call site and consumed, so a plain struct literal is enough.
pub(crate) struct VerificationMailer {
    pub email_config: EmailConfig,
    pub email_renderer: Arc<EmailRenderer>,
    pub server_config: ServerConfig,
    pub email_max_attempts: u32,
}

impl VerificationMailer {
    /// Whether an email transport is configured at all.
    ///
    /// `context` identifies the flow, never the address: on the resend path
    /// the address is an unauthenticated request field, and `tracing` emits
    /// embedded newlines raw, so interpolating it would let anyone forge log
    /// lines.
    fn is_configured(&self, context: &str) -> bool {
        if is_configured(&self.email_config) {
            return true;
        }

        warn!("Email not configured — skipping the verification email ({context})");

        false
    }

    /// Render the verification email for `token` and insert its `_crap_jobs`
    /// row on `conn`.
    ///
    /// # Errors
    ///
    /// Returns an error if the template fails to render or the job row can't
    /// be inserted.
    fn render_and_queue(
        &self,
        conn: &dyn DbConnection,
        recipient: &str,
        token: &str,
    ) -> Result<(), ServiceError> {
        let base_url = self.server_config.base_url();
        let verify_url = format!("{base_url}/admin/verify-email?token={token}");

        let html = self
            .email_renderer
            .render(
                "verify_email",
                &VerifyEmailContext {
                    verify_url: &verify_url,
                    from_name: &self.email_config.from_name,
                },
            )
            .context("Failed to render verify email template")?;

        queue_email(
            conn,
            &EmailJobData {
                to: recipient.to_string(),
                subject: "Verify your email".to_string(),
                html,
                text: None,
            },
            self.email_max_attempts,
        )
        .context("Failed to queue verification email")?;

        Ok(())
    }

    /// Mint the account's verification token and queue its email job on
    /// `conn` — both on the caller's connection, so when that connection is
    /// the account's own write transaction the three land together.
    ///
    /// This is what keeps a sign-up atomic. Minting the token from a detached
    /// task after the account commits leaves a window where a stop or crash
    /// yields a committed account with no verification token and nothing
    /// queued to retry — the user can neither verify nor sign up again.
    ///
    /// The SMTP send itself stays in the `_system_email` job, where it is
    /// retried on the queue's own budget.
    ///
    /// # Errors
    ///
    /// Returns an error if the token can't be stored, the template fails to
    /// render, or the email job row can't be inserted. Returns `Ok(())`
    /// without doing anything when no email transport is configured.
    pub(crate) fn issue_and_queue(
        &self,
        conn: &dyn DbConnection,
        recipient: &VerificationRecipient<'_>,
    ) -> Result<(), ServiceError> {
        if !self.is_configured("verification") {
            return Ok(());
        }

        let token = issue_verification_token(
            conn,
            recipient.slug,
            recipient.user_id,
            VERIFICATION_TOKEN_EXPIRY,
        )?;

        self.render_and_queue(conn, recipient.email, &token)
    }
}

/// Inputs for [`send_verification_email`] — the sign-up path, where the
/// account was just created and its id is already known.
pub(crate) struct VerificationEmailInput {
    pub pool: DbPool,
    pub mailer: VerificationMailer,
    pub slug: String,
    pub user_id: String,
    pub user_email: String,
}

/// Generate a verification token and send the verification email.
/// Spawns its own `spawn_blocking` task internally.
// Excluded from coverage: async tokio task that requires SMTP email transport,
// DB pool, and email renderer — cannot be unit tested without external services.
#[cfg(not(tarpaulin_include))]
pub(crate) fn send_verification_email(input: VerificationEmailInput) {
    tokio::task::spawn_blocking(move || send_verification_email_blocking(&input));
}

/// The blocking body of [`send_verification_email`]. Fire-and-forget — every
/// failure is logged and swallowed here, never surfaced.
#[cfg(not(tarpaulin_include))]
fn send_verification_email_blocking(input: &VerificationEmailInput) {
    if !input.mailer.is_configured("sign-up") {
        return;
    }

    // The write pool: the body below opens `BEGIN IMMEDIATE`, which belongs
    // on a write connection.
    let mut conn = match input.pool.write() {
        Ok(c) => c,
        Err(e) => {
            error!("DB connection for verification token: {e}");

            return;
        }
    };

    issue_in_transaction(&mut conn, input)
        .inspect_err(|e| error!("Verification email not sent: {e}"))
        .ok();
}

/// Mint the token and queue the email in ONE transaction, so this fallback
/// cannot leave an account holding a token whose email was never queued.
#[cfg(not(tarpaulin_include))]
fn issue_in_transaction(
    conn: &mut BoxedConnection,
    input: &VerificationEmailInput,
) -> Result<(), ServiceError> {
    let tx = conn
        .transaction_immediate()
        .context("Failed to open transaction for the verification email")?;

    input.mailer.issue_and_queue(
        &tx,
        &VerificationRecipient {
            slug: &input.slug,
            user_id: &input.user_id,
            email: &input.user_email,
        },
    )?;

    tx.commit()
        .context("Failed to commit the verification email")?;

    Ok(())
}

/// Inputs for [`resend_verification_email`] — the self-service path, which
/// only knows the address the caller typed.
pub(crate) struct ResendVerificationInput {
    pub pool: DbPool,
    pub mailer: VerificationMailer,
    pub locale_config: LocaleConfig,
    pub slug: String,
    pub def: Arc<CollectionDefinition>,
    pub email: String,
}

/// Issue a fresh verification token for the account with this address and
/// queue the email.
///
/// Spawns its own `spawn_blocking` task and reports nothing back: the caller
/// answers the same way whether or not an account was found, so the endpoint
/// never confirms which addresses are registered.
#[cfg(not(tarpaulin_include))]
pub(crate) fn resend_verification_email(input: ResendVerificationInput) {
    tokio::task::spawn_blocking(move || resend_verification_email_blocking(&input));
}

#[cfg(not(tarpaulin_include))]
fn resend_verification_email_blocking(input: &ResendVerificationInput) {
    if !input.mailer.is_configured("resend") {
        return;
    }

    // The write pool: the body below opens `BEGIN IMMEDIATE`.
    let mut conn = match input.pool.write() {
        Ok(c) => c,
        Err(e) => {
            error!("DB connection for verification resend: {e}");

            return;
        }
    };

    resend_in_transaction(&mut conn, input)
        .inspect_err(|e| error!("Verification resend error: {e}"))
        .ok();
}

/// Rotate the account's verification token and queue its email in ONE
/// transaction.
///
/// Issuing the new token invalidates the link the previous email carried, so
/// a failure to queue the replacement between the two writes would leave the
/// account with a dead link and no mail on the way. Either both land or
/// neither does, and the caller can simply ask again.
#[cfg(not(tarpaulin_include))]
fn resend_in_transaction(
    conn: &mut BoxedConnection,
    input: &ResendVerificationInput,
) -> Result<(), ServiceError> {
    let tx = conn
        .transaction_immediate()
        .context("Failed to open transaction for verification resend")?;

    let ctx = ServiceContext::collection(&input.slug, &input.def)
        .conn(&tx)
        .locale_config(Some(&input.locale_config))
        .build();

    // No account, already verified, or locked — nothing to send.
    let issued = generate_verification_token(&ctx, &input.email, VERIFICATION_TOKEN_EXPIRY)?;

    // Release the borrow of `tx` before resolving it.
    drop(ctx);

    let Some(issued) = issued else {
        return Ok(());
    };

    input
        .mailer
        .render_and_queue(&tx, &issued.email, &issued.token)?;

    tx.commit()
        .context("Failed to commit verification resend")?;

    Ok(())
}

/// Everything needed to render and queue a password-reset email. Built at its
/// single call site (`EmailContext::reset_mailer`) and consumed here, so a
/// plain struct literal is enough.
pub(crate) struct ResetMailer {
    pub email_config: EmailConfig,
    pub email_renderer: Arc<EmailRenderer>,
    pub server_config: ServerConfig,
    pub email_max_attempts: u32,
    /// How long the emitted reset link stays valid, in seconds. Also the
    /// figure the email itself quotes, so the two cannot disagree.
    pub reset_expiry: u64,
}

impl ResetMailer {
    /// Render the reset email for `token` and insert its `_crap_jobs` row on
    /// `conn`.
    ///
    /// # Errors
    ///
    /// Returns an error if the template fails to render or the job row can't
    /// be inserted.
    fn render_and_queue(
        &self,
        conn: &dyn DbConnection,
        recipient: &str,
        token: &str,
    ) -> Result<(), ServiceError> {
        // The shared `base_url()` trims a configured `public_url`'s trailing
        // slash; a hand-rolled join produced `…com//admin/…`.
        let base_url = self.server_config.base_url();
        let reset_url = format!("{base_url}/admin/reset-password?token={token}");

        let html = self
            .email_renderer
            .render(
                "password_reset",
                &PasswordResetEmailContext {
                    reset_url: &reset_url,
                    expiry_minutes: self.reset_expiry / 60,
                    from_name: &self.email_config.from_name,
                },
            )
            .context("Failed to render reset email template")?;

        queue_email(
            conn,
            &EmailJobData {
                to: recipient.to_string(),
                subject: "Reset your password".to_string(),
                html,
                text: None,
            },
            self.email_max_attempts,
        )
        .context("Failed to queue reset email")?;

        Ok(())
    }

    /// Mint the account's reset token and queue its email on the context's
    /// connection — both writes on one connection, so a caller that hands in
    /// a transaction gets the two atomically.
    ///
    /// `Ok(())` with nothing written when no account matches: the caller
    /// answers the same either way, so the endpoint never confirms which
    /// addresses are registered.
    ///
    /// # Errors
    ///
    /// Returns an error if the context carries no connection, the token can't
    /// be stored, the template fails to render, or the email job row can't be
    /// inserted. The caller must roll its transaction back on an error — a
    /// committed token whose email was never queued is a reset link nobody
    /// will ever receive.
    pub(crate) fn issue_and_queue(
        &self,
        ctx: &ServiceContext,
        email: &str,
    ) -> Result<(), ServiceError> {
        // Both writes have to land on ONE connection for the caller's
        // transaction to cover them together. A pool-backed context would
        // resolve a fresh connection per write and quietly reopen the split
        // this exists to close, so require the attached one.
        let conn = ctx
            .conn
            .context("a password reset needs a connection to mint and queue on")?;

        let Some(issued) = generate_reset_token(ctx, email, self.reset_expiry)? else {
            return Ok(());
        };

        // The mail goes to the address the caller typed, matching the account
        // the lookup resolved it to.
        self.render_and_queue(conn, email, &issued.token)
    }
}

/// Inputs for [`send_reset_email`] — the forgot-password path, which only
/// knows the address the caller typed.
pub(crate) struct ResetEmailInput {
    pub pool: DbPool,
    pub mailer: ResetMailer,
    pub locale_config: LocaleConfig,
    pub slug: String,
    pub def: Arc<CollectionDefinition>,
    pub email: String,
}

/// Mint a password-reset token for the account with this address and queue
/// the reset email.
///
/// Spawns its own `spawn_blocking` task and reports nothing back: the caller
/// answers the same way whether or not an account was found, so the endpoint
/// never confirms which addresses are registered.
// Excluded from coverage: async tokio task that requires a DB pool and an
// email renderer — the transactional body below is what the tests exercise.
#[cfg(not(tarpaulin_include))]
pub(crate) fn send_reset_email(input: ResetEmailInput) {
    tokio::task::spawn_blocking(move || send_reset_email_blocking(&input));
}

/// The blocking body of [`send_reset_email`]. Fire-and-forget — every failure
/// is logged and swallowed here, never surfaced.
#[cfg(not(tarpaulin_include))]
fn send_reset_email_blocking(input: &ResetEmailInput) {
    // The write pool: the body below opens `BEGIN IMMEDIATE`.
    let mut conn = match input.pool.write() {
        Ok(c) => c,
        Err(e) => {
            error!("DB connection for forgot password: {e}");

            return;
        }
    };

    issue_reset_in_transaction(&mut conn, input)
        .inspect_err(|e| error!("Password reset email not sent: {e}"))
        .ok();
}

/// Mint the token and queue the email in ONE transaction.
///
/// Issuing the token invalidates any link an earlier email carried, and the
/// new link only exists in the mail that is queued alongside it. Splitting
/// the two lets a crash in between leave an account whose only live reset
/// token is one nobody will ever be sent — the user is locked out of the
/// flow until the token expires. Either both land or neither does, and the
/// caller can simply ask again.
fn issue_reset_in_transaction(
    conn: &mut BoxedConnection,
    input: &ResetEmailInput,
) -> Result<(), ServiceError> {
    let tx = conn
        .transaction_immediate()
        .context("Failed to open transaction for the password reset email")?;

    let ctx = ServiceContext::collection(&input.slug, &input.def)
        .conn(&tx)
        .locale_config(Some(&input.locale_config))
        .build();

    let issued = input.mailer.issue_and_queue(&ctx, &input.email);

    // Release the borrow of `tx` before resolving it.
    drop(ctx);

    issued?;

    tx.commit()
        .context("Failed to commit the password reset email")?;

    Ok(())
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use r2d2::Pool;
    use r2d2_sqlite::SqliteConnectionManager;

    use crate::{
        config::{CrapConfig, EmailProvider},
        core::{FieldDefinition, FieldType, collection::Auth, email::SYSTEM_EMAIL_JOB},
        db::{InMemoryConn, migrate, query::jobs as job_query},
    };

    use super::*;

    /// A mailer whose renderer is the compiled-in template set. The connection
    /// handed to it in these tests has no tables at all, so any write attempt
    /// shows up as an error rather than passing silently.
    fn mailer(provider: EmailProvider) -> VerificationMailer {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config = CrapConfig::default();

        VerificationMailer {
            email_config: EmailConfig {
                provider,
                ..config.email
            },
            email_renderer: Arc::new(EmailRenderer::new(tmp.path()).expect("renderer")),
            server_config: config.server,
            email_max_attempts: 1,
        }
    }

    fn recipient() -> VerificationRecipient<'static> {
        VerificationRecipient {
            slug: "users",
            user_id: "u1",
            email: "someone@example.com",
        }
    }

    /// With no transport there is nothing to verify against, so the account
    /// write must not be burdened with a token or a job row — and must not
    /// fail either.
    #[test]
    fn an_unconfigured_transport_writes_nothing_and_does_not_fail() {
        let conn = InMemoryConn::open();

        mailer(EmailProvider::Log)
            .issue_and_queue(&conn, &recipient())
            .expect("no transport configured is a no-op, not an error");
    }

    /// The failure must reach the caller: it runs on the account's own write
    /// transaction, and rolling that back is the whole point — committing the
    /// account while swallowing this leaves a user who can never verify and
    /// no queued mail to retry.
    #[test]
    fn a_failed_token_write_propagates_instead_of_being_swallowed() {
        let conn = InMemoryConn::open();

        mailer(EmailProvider::Webhook)
            .issue_and_queue(&conn, &recipient())
            .expect_err("a token that could not be stored must fail the write");
    }

    /// The `users` collection the reset tests resolve their address against.
    fn users_def() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("users");
        def.auth = Some(Auth::enabled());
        def.fields = vec![
            FieldDefinition::builder("email", FieldType::Email)
                .unique(true)
                .build(),
        ];

        def
    }

    /// An in-memory database with the auth table and one account, plus the
    /// jobs table only when `with_jobs_table` — omitting it is how these
    /// tests make the email job insert fail.
    ///
    /// Every statement runs on the returned connection, so what the
    /// transaction leaves behind is read back from the same database.
    fn reset_fixture(with_jobs_table: bool) -> (DbPool, BoxedConnection) {
        let pool = DbPool::from_pool(
            Pool::builder()
                .max_size(1)
                .build(SqliteConnectionManager::memory())
                .expect("test pool"),
        );

        let conn = pool.get().expect("connection");

        conn.execute_batch(
            "CREATE TABLE users (
                id TEXT PRIMARY KEY,
                email TEXT UNIQUE,
                _password_hash TEXT,
                _locked INTEGER DEFAULT 0,
                _verified INTEGER DEFAULT 1,
                _session_version INTEGER DEFAULT 0,
                _reset_token TEXT,
                _reset_token_exp INTEGER,
                _verification_token TEXT,
                _verification_token_exp INTEGER,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO users (id, email) VALUES ('u1', 'user@example.com');",
        )
        .expect("users table");

        if with_jobs_table {
            migrate::create_jobs_table(
                &conn,
                "TEXT DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
                "TEXT",
            )
            .expect("jobs table");
        }

        (pool, conn)
    }

    fn reset_input(pool: &DbPool) -> ResetEmailInput {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config = CrapConfig::default();

        ResetEmailInput {
            pool: pool.clone(),
            mailer: ResetMailer {
                email_config: config.email,
                email_renderer: Arc::new(EmailRenderer::new(tmp.path()).expect("renderer")),
                server_config: config.server,
                email_max_attempts: 1,
                reset_expiry: 3600,
            },
            locale_config: config.locale,
            slug: "users".to_string(),
            def: Arc::new(users_def()),
            email: "user@example.com".to_string(),
        }
    }

    /// The reset token standing on the account, if any.
    fn stored_token(conn: &BoxedConnection) -> Option<String> {
        conn.query_one("SELECT _reset_token FROM users WHERE id = 'u1'", &[])
            .expect("read the account's reset token")
            .and_then(|row| row.opt_text_at(0))
    }

    /// Queued `_system_email` job rows.
    fn queued_emails(conn: &BoxedConnection) -> usize {
        job_query::list_job_runs(conn, Some(SYSTEM_EMAIL_JOB), None, 100, 0)
            .expect("list queued email jobs")
            .len()
    }

    /// Regression: the token used to be minted and committed in one step and
    /// the email queued in another. A failure in between left a live reset
    /// token on the account whose only link was never queued for delivery —
    /// the account is stuck in the flow until that token expires, and a
    /// retry just replaces one undeliverable token with another. Both writes
    /// share one transaction now, so a failed queue takes the token with it.
    #[test]
    fn a_failed_email_queue_leaves_no_reset_token() {
        let (pool, mut conn) = reset_fixture(false);
        let input = reset_input(&pool);

        issue_reset_in_transaction(&mut conn, &input)
            .expect_err("no jobs table — queueing the email must fail");

        assert_eq!(
            stored_token(&conn),
            None,
            "the token must roll back with the email job that could not be queued"
        );
    }

    /// The other half of the guarantee: on success both writes are there.
    #[test]
    fn a_successful_reset_commits_the_token_and_the_email_job() {
        let (pool, mut conn) = reset_fixture(true);
        let input = reset_input(&pool);

        issue_reset_in_transaction(&mut conn, &input).expect("reset email queued");

        assert!(
            stored_token(&conn).is_some(),
            "the account must carry the token the emailed link uses"
        );
        assert_eq!(queued_emails(&conn), 1, "exactly one email must be queued");
    }

    /// An address with no account writes nothing and reports nothing — the
    /// endpoint answers the same either way, so it cannot be used to test
    /// which addresses are registered.
    #[test]
    fn an_unknown_address_is_a_silent_no_op() {
        let (pool, mut conn) = reset_fixture(true);
        let mut input = reset_input(&pool);
        input.email = "nobody@example.com".to_string();

        issue_reset_in_transaction(&mut conn, &input).expect("an unknown address is not an error");

        assert_eq!(stored_token(&conn), None);
        assert_eq!(queued_emails(&conn), 0);
    }
}
