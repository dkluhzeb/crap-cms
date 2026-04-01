//! Webhook email provider — sends emails via HTTP POST.
//! Works with SendGrid, Mailgun, Resend, or any HTTP API.

use std::collections::HashMap;

use anyhow::{Context as _, Result, anyhow, bail};
use reqwest::blocking::Client;
use serde_json::json;
use tracing::info;

use crate::config::EmailConfig;

use super::EmailProvider;

/// Webhook email provider that POSTs email data as JSON to a URL.
pub struct WebhookEmailProvider {
    url: String,
    headers: HashMap<String, String>,
    from_address: String,
    from_name: String,
    client: Client,
}

impl WebhookEmailProvider {
    pub fn new(config: &EmailConfig) -> Result<Self> {
        let url = config
            .webhook_url
            .as_deref()
            .ok_or_else(|| anyhow!("email.webhook_url is required for webhook provider"))?
            .to_string();

        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .context("Failed to create webhook HTTP client")?;

        Ok(Self {
            url,
            headers: config.webhook_headers.clone(),
            from_address: config.from_address.clone(),
            from_name: config.from_name.clone(),
            client,
        })
    }
}

impl EmailProvider for WebhookEmailProvider {
    fn send(&self, to: &str, subject: &str, html: &str, text: Option<&str>) -> Result<()> {
        let mut payload = json!({
            "from": {
                "email": self.from_address,
                "name": self.from_name,
            },
            "to": to,
            "subject": subject,
            "html": html,
        });

        if let Some(plain) = text {
            payload["text"] = json!(plain);
        }

        let mut req = self.client.post(&self.url).json(&payload);

        for (key, value) in &self.headers {
            req = req.header(key, value);
        }

        let resp = req
            .send()
            .with_context(|| format!("Webhook email request failed: {}", self.url))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            bail!(
                "Webhook email failed with status {}: {}",
                status,
                body.chars().take(200).collect::<String>()
            );
        }

        info!("Email sent via webhook to {} (subject: {})", to, subject);

        Ok(())
    }

    fn kind(&self) -> &'static str {
        "webhook"
    }
}
