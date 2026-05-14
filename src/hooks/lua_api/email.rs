//! `crap.email` namespace — outbound email sending via configurable provider.
//!
//! - `crap.email.send(opts)` — immediate, blocking send
//! - `crap.email.queue(opts)` — async, queued with retries via job system

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Table};

use crate::{
    config::CrapConfig,
    core::email::{EmailJobData, create_email_provider, queue_email, validate_no_crlf},
};

use crate::hooks::lua_api::crud::get_tx_conn;

/// Validate header-derived email fields from a Lua `opts` table. Rejects any
/// `\r`, `\n`, or `\0` in `to` or `subject` — the two fields currently
/// accepted from Lua that end up in SMTP headers. Body fields (`html`,
/// `text`) are not validated: they are MIME-encoded / JSON-escaped downstream.
fn validate_email_fields(to: &str, subject: &str) -> mlua::Result<()> {
    validate_no_crlf("to", to).map_err(|e| RuntimeError(format!("{e:#}")))?;
    validate_no_crlf("subject", subject).map_err(|e| RuntimeError(format!("{e:#}")))?;

    Ok(())
}

/// Register `crap.email` — outbound email sending via the configured provider.
pub(super) fn register_email(lua: &Lua, crap: &Table, config: &CrapConfig) -> Result<()> {
    let email_table = lua.create_table()?;

    // crap.email.send(opts) — immediate, blocking
    let provider = create_email_provider(&config.email)?;

    let email_send_fn = lua.create_function(move |_, opts: Table| -> mlua::Result<bool> {
        let to: String = opts.get("to")?;
        let subject: String = opts.get("subject")?;
        let html: String = opts.get("html")?;
        let text: Option<String> = opts.get("text")?;

        validate_email_fields(&to, &subject)?;

        provider
            .send(&to, &subject, &html, text.as_deref())
            .map_err(|e| RuntimeError(format!("email send error: {e:#}")))?;

        Ok(true)
    })?;

    // crap.email.queue(opts) — async, queued with retries.
    //
    // We clone the email config into the closure so per-call `opts.retries`
    // overrides flow through `EmailConfig::queue_retries` without changing
    // the `queue_email` signature. `EmailConfig` is plain owned data, so the
    // clone is cheap and per-call mutation can't leak back to the global.
    let email_config = config.email.clone();

    let email_queue_fn = lua.create_function(move |lua, opts: Table| -> mlua::Result<String> {
        let to: String = opts.get("to")?;
        let subject: String = opts.get("subject")?;
        let html: String = opts.get("html")?;
        let text: Option<String> = opts.get("text")?;

        validate_email_fields(&to, &subject)?;

        // Per-call `retries` override; falls back to the captured config.
        let mut config = email_config.clone();
        if let Ok(Some(retries)) = opts.get::<Option<u32>>("retries") {
            config.queue_retries = retries;
        }

        let conn = get_tx_conn(lua)?;

        let job_id = queue_email(
            conn,
            &EmailJobData {
                to,
                subject,
                html,
                text,
            },
            &config,
        )
        .map_err(|e| RuntimeError(format!("email queue error: {e:#}")))?;

        Ok(job_id)
    })?;

    email_table.set("send", email_send_fn)?;
    email_table.set("queue", email_queue_fn)?;
    crap.set("email", email_table)?;

    Ok(())
}
