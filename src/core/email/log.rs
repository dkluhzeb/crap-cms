//! Log email provider — logs emails instead of sending them.
//! Useful for development and testing.

use anyhow::Result;
use tracing::info;

use super::EmailProvider;

/// Email provider that logs emails to tracing instead of sending.
pub struct LogEmailProvider;

impl EmailProvider for LogEmailProvider {
    fn send(&self, to: &str, subject: &str, _html: &str, _text: Option<&str>) -> Result<()> {
        info!("[email:log] Would send to={}, subject=\"{}\"", to, subject);

        Ok(())
    }

    fn kind(&self) -> &'static str {
        "log"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_provider_send_succeeds() {
        let provider = LogEmailProvider;
        let result = provider.send("user@example.com", "Test", "<p>Hello</p>", None);
        assert!(result.is_ok());
    }

    #[test]
    fn log_provider_kind() {
        let provider = LogEmailProvider;
        assert_eq!(provider.kind(), "log");
    }
}
