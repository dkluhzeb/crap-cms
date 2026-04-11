//! Custom Lua-delegated email provider.
//!
//! Delegates email sending to a user-provided Lua function
//! registered via `crap.email.register({ send = function(...) end })`.

use anyhow::Result;
use mlua::{Function, Lua};

use super::EmailProvider;

/// Custom email provider that delegates to a Lua function.
pub struct CustomEmailProvider {
    lua: Lua,
}

impl CustomEmailProvider {
    /// Create a new custom email provider.
    /// The Lua state must have `crap._email_send` registered.
    pub fn new(lua: Lua) -> Self {
        Self { lua }
    }

    fn get_send_fn(&self) -> Result<Function> {
        let crap: mlua::Table = self
            .lua
            .globals()
            .get("crap")
            .map_err(|e| anyhow::anyhow!("crap global not found: {}", e))?;

        let send_fn: Function = crap
            .get("_email_send")
            .map_err(|e| anyhow::anyhow!("crap._email_send not found: {}", e))?;

        Ok(send_fn)
    }
}

impl EmailProvider for CustomEmailProvider {
    fn send(&self, to: &str, subject: &str, html: &str, text: Option<&str>) -> Result<()> {
        let func = self.get_send_fn()?;

        let opts = self.lua.create_table()?;
        opts.set("to", to.to_string())?;
        opts.set("subject", subject.to_string())?;
        opts.set("html", html.to_string())?;
        if let Some(plain) = text {
            opts.set("text", plain.to_string())?;
        }

        func.call::<()>(opts)
            .map_err(|e| anyhow::anyhow!("custom email send error: {:#}", e))
    }

    fn kind(&self) -> &'static str {
        "custom"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_lua() -> Lua {
        let lua = Lua::new();
        lua.load(
            r#"
            crap = {}
            local sent = {}
            crap._email_send = function(opts)
                table.insert(sent, opts)
            end
            crap._sent = sent
            "#,
        )
        .exec()
        .expect("Lua setup failed");
        lua
    }

    #[test]
    fn send_delegates_to_lua() {
        let lua = setup_lua();
        let provider = CustomEmailProvider::new(lua);

        provider
            .send("user@example.com", "Test Subject", "<p>Hello</p>", None)
            .unwrap();
    }

    #[test]
    fn send_with_text_body() {
        let lua = setup_lua();
        let provider = CustomEmailProvider::new(lua);

        provider
            .send(
                "user@example.com",
                "Test",
                "<p>Hello</p>",
                Some("Hello plain"),
            )
            .unwrap();
    }

    #[test]
    fn send_errors_without_function() {
        let lua = Lua::new();
        lua.load("crap = {}").exec().unwrap();
        let provider = CustomEmailProvider::new(lua);

        let result = provider.send("user@example.com", "Test", "<p>Hi</p>", None);
        assert!(result.is_err());
    }

    #[test]
    fn kind_returns_custom() {
        let lua = setup_lua();
        let provider = CustomEmailProvider::new(lua);
        assert_eq!(provider.kind(), "custom");
    }
}
