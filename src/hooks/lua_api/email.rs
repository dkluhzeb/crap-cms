//! `crap.email` namespace — outbound email sending via configurable provider.
//!
//! - `crap.email.send(opts)` — immediate, blocking send
//! - `crap.email.queue(opts)` — async, queued with retries via job system

use anyhow::Result;
use mlua::{Error::RuntimeError, FromLua, Lua, LuaSerdeExt, Result as LuaResult, Value};
use serde::Deserialize;

use crate::config::CrapConfig;
use crate::core::email::{
    EmailJobData, SharedEmailProvider, create_email_provider, queue_email, validate_no_crlf,
};
use crate::hooks::lua_api::crud::get_tx_conn;
use crate::typegen::lua::{LuaAnnotation, LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table};

/// Options table for `crap.email.send` / `crap.email.queue`.
#[derive(Deserialize, LuaAnnotation)]
#[lua(class = "crap.EmailOptions")]
pub(crate) struct EmailOptions {
    /// Recipient email address.
    pub(crate) to: String,
    /// Email subject line.
    pub(crate) subject: String,
    /// HTML email body.
    pub(crate) html: String,
    /// Plain-text fallback body.
    pub(crate) text: Option<String>,
    /// Per-call override of the queue retry count (only honoured by
    /// `crap.email.queue`).
    pub(crate) retries: Option<u32>,
}

impl FromLua for EmailOptions {
    fn from_lua(value: Value, lua: &Lua) -> LuaResult<Self> {
        lua.from_value(value)
    }
}

/// State threaded into `crap.email.send` / `crap.email.queue` — the
/// shared provider (sender) and the queue's default `max_attempts`
/// (resolved at register time from `[jobs.queues.email] retries`).
pub(super) struct EmailState {
    provider: SharedEmailProvider,
    queue_max_attempts: u32,
}

/// Validate header-derived email fields. Rejects any `\r`, `\n`, or
/// `\0` in `to` or `subject` — the two fields currently accepted from
/// Lua that end up in SMTP headers. Body fields (`html`, `text`) are
/// not validated: they are MIME-encoded / JSON-escaped downstream.
fn validate_email_fields(to: &str, subject: &str) -> LuaResult<()> {
    validate_no_crlf("to", to).map_err(|e| RuntimeError(format!("{e:#}")))?;
    validate_no_crlf("subject", subject).map_err(|e| RuntimeError(format!("{e:#}")))?;
    Ok(())
}

/// Send an email via SMTP. Blocking — safe to call from hooks.
/// Returns true on success. If email is not configured (`smtp_host` empty), logs a warning and returns true (no-op).
#[lua_fn(
    path = "crap.email.send",
    returns_doc = "True on success (also true when SMTP is disabled — call is a no-op)."
)]
fn email_send(
    state: &EmailState,
    _: &Lua,
    #[lua(ty = "crap.EmailOptions", doc = "Email options.")] opts: EmailOptions,
) -> LuaResult<bool> {
    validate_email_fields(&opts.to, &opts.subject)?;

    state
        .provider
        .send(&opts.to, &opts.subject, &opts.html, opts.text.as_deref())
        .map_err(|e| RuntimeError(format!("email send error: {e:#}")))?;

    Ok(true)
}

/// Queue an email for async delivery via the job system. Returns the job
/// run ID. Per-call `retries` override on `opts` takes precedence over
/// the queue default from `[jobs.queues.email] retries`.
#[lua_fn(path = "crap.email.queue", returns_doc = "Queued job ID.", auto_tx)]
fn email_queue(
    state: &EmailState,
    lua: &Lua,
    #[lua(
        ty = "crap.EmailOptions",
        doc = "Email options (with optional `retries` override)."
    )]
    opts: EmailOptions,
) -> LuaResult<String> {
    validate_email_fields(&opts.to, &opts.subject)?;

    let max_attempts = opts
        .retries
        .map_or(state.queue_max_attempts, |r| r.saturating_add(1));

    let conn = get_tx_conn(lua)?;

    let job_id = queue_email(
        conn,
        &EmailJobData {
            to: opts.to,
            subject: opts.subject,
            html: opts.html,
            text: opts.text,
        },
        max_attempts,
    )
    .map_err(|e| RuntimeError(format!("email queue error: {e:#}")))?;

    Ok(job_id)
}

lua_table! {
    name: crap_email,
    path: "crap.email",
    state: EmailState,
    header: "Email sending (requires SMTP configuration in crap.toml).",
    fns: [email_send, email_queue],
}

/// Register `crap.email` — outbound email sending via the configured
/// provider. The parent `crap` table must already be in globals.
pub(super) fn register_email(lua: &Lua, config: &CrapConfig) -> Result<()> {
    let provider = create_email_provider(&config.email)?;
    let state = EmailState {
        provider,
        queue_max_attempts: config.jobs.system_email_max_attempts(),
    };
    register_crap_email(lua, state)?;
    Ok(())
}
