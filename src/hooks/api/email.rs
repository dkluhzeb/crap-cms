//! `crap.email` namespace — outbound email sending via configurable provider.
//!
//! - `crap.email.send(opts)` — immediate, blocking send
//! - `crap.email.queue(opts)` — async, queued with retries via job system

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Table};

use crate::{
    config::CrapConfig,
    core::email::{create_email_provider, queue_email},
};

use super::super::lifecycle::crud::get_tx_conn;

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

        provider
            .send(&to, &subject, &html, text.as_deref())
            .map_err(|e| RuntimeError(format!("email send error: {:#}", e)))?;

        Ok(true)
    })?;

    // crap.email.queue(opts) — async, queued with retries
    let default_retries = config.email.queue_retries;
    let default_queue = config.email.queue_name.clone();

    let email_queue_fn = lua.create_function(move |lua, opts: Table| -> mlua::Result<String> {
        let to: String = opts.get("to")?;
        let subject: String = opts.get("subject")?;
        let html: String = opts.get("html")?;
        let text: Option<String> = opts.get("text")?;

        let retries: u32 = opts
            .get::<Option<u32>>("retries")
            .ok()
            .flatten()
            .unwrap_or(default_retries);

        let conn_ptr = get_tx_conn(lua)?;
        let conn = unsafe { &*conn_ptr };

        let job_id = queue_email(
            conn,
            &to,
            &subject,
            &html,
            text.as_deref(),
            retries + 1, // max_attempts = retries + 1
            &default_queue,
        )
        .map_err(|e| RuntimeError(format!("email queue error: {:#}", e)))?;

        Ok(job_id)
    })?;

    email_table.set("send", email_send_fn)?;
    email_table.set("queue", email_queue_fn)?;
    crap.set("email", email_table)?;

    Ok(())
}
