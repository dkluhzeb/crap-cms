//! `crap.email` namespace — outbound email sending via SMTP.

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Table};

use crate::{config::CrapConfig, core::email::send_email};

/// Register `crap.email` — outbound email sending via SMTP.
pub(super) fn register_email(lua: &Lua, crap: &Table, config: &CrapConfig) -> Result<()> {
    let email_table = lua.create_table()?;
    let email_config = config.email.clone();
    let email_send_fn = lua.create_function(move |_, opts: Table| -> mlua::Result<bool> {
        let to: String = opts.get("to")?;
        let subject: String = opts.get("subject")?;
        let html: String = opts.get("html")?;
        let text: Option<String> = opts.get("text")?;

        send_email(&email_config, &to, &subject, &html, text.as_deref())
            .map_err(|e| RuntimeError(format!("email send error: {}", e)))?;

        Ok(true)
    })?;
    email_table.set("send", email_send_fn)?;
    crap.set("email", email_table)?;
    Ok(())
}
