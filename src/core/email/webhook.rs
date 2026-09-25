//! Webhook email provider — sends emails via HTTP POST.
//! Works with `SendGrid`, Mailgun, Resend, or any HTTP API.

use std::{collections::HashMap, time::Duration};

use anyhow::{Context as _, Result, anyhow, bail};
use reqwest::{Error as ReqwestError, blocking::Client, redirect::Policy};
use serde::Serialize;
use tracing::info;
use url::Url;

use crate::config::EmailConfig;

use super::EmailProvider;

/// JSON body `POSTed` to the configured webhook for each outgoing email.
#[derive(Serialize)]
struct WebhookEmailPayload<'a> {
    from: WebhookFrom<'a>,
    to: &'a str,
    subject: &'a str,
    html: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<&'a str>,
}

#[derive(Serialize)]
struct WebhookFrom<'a> {
    email: &'a str,
    name: &'a str,
}

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

        // Redirects are not followed: a followed 301/302 turns the POST into
        // a GET without the body, so a moved endpoint would answer 2xx and
        // the email be recorded as sent without ever being delivered. A 3xx
        // is a failure (retried, then reported) instead.
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(Policy::none())
            .build()
            .context("Failed to create webhook HTTP client")?;

        Ok(Self {
            url,
            headers: config.webhook_headers.to_map(),
            from_address: config.from_address.clone(),
            from_name: config.from_name.clone(),
            client,
        })
    }
}

impl EmailProvider for WebhookEmailProvider {
    fn send(&self, to: &str, subject: &str, html: &str, text: Option<&str>) -> Result<()> {
        let payload = WebhookEmailPayload {
            from: WebhookFrom {
                email: &self.from_address,
                name: &self.from_name,
            },
            to,
            subject,
            html,
            text,
        };

        let mut req = self.client.post(&self.url).json(&payload);

        for (key, value) in &self.headers {
            req = req.header(key, value);
        }

        // The URL may carry a token (query string, userinfo): the transport
        // error names only its origin, since this text lands in logs and the
        // job's error column.
        let resp = req
            .send()
            .map_err(ReqwestError::without_url)
            .with_context(|| {
                format!("Webhook email request failed: {}", redacted_url(&self.url))
            })?;

        let status = resp.status();

        if status.is_redirection() {
            bail!(
                "Webhook email failed with status {status}: redirects are not followed — \
                 set email.webhook_url to the endpoint's final address"
            );
        }

        if !status.is_success() {
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

/// The origin (`scheme://host:port`) of a webhook URL — what error text may
/// show of it. Path, query and userinfo can carry credentials.
fn redacted_url(url: &str) -> String {
    Url::parse(url).map_or_else(
        |_| "<unparseable webhook URL>".to_string(),
        |u| u.origin().ascii_serialization(),
    )
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read as _, Write as _},
        net::TcpListener,
        sync::mpsc,
        thread,
    };

    use serde_json::json;

    use super::*;

    /// Answer one connection with `response`, reporting each request line.
    fn one_shot_server(response: &'static str) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::channel();

        thread::spawn(move || {
            while let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_string();

                let _ = tx.send(head.lines().next().unwrap_or("").to_string());
                let _ = stream.write_all(response.as_bytes());
            }
        });

        (format!("http://{addr}"), rx)
    }

    fn provider(url: &str) -> WebhookEmailProvider {
        let config = EmailConfig {
            webhook_url: Some(url.to_string()),
            ..EmailConfig::default()
        };

        WebhookEmailProvider::new(&config).unwrap()
    }

    /// Regression: the client followed redirects — a 301/302 turned the POST
    /// into a body-less GET, the new URL answered 2xx and the email was
    /// recorded as sent without being delivered. A 3xx is now a failure and
    /// the redirect target is never requested.
    #[test]
    fn a_redirect_is_a_failure_not_followed() {
        let (base, rx) = one_shot_server(
            "HTTP/1.1 301 Moved Permanently\r\nLocation: /moved\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );

        let err = provider(&format!("{base}/send"))
            .send("u@example.com", "S", "<p>x</p>", None)
            .expect_err("a redirect must fail the send");

        assert!(
            err.to_string().contains("redirects are not followed"),
            "{err}"
        );
        assert!(rx.recv().unwrap().starts_with("POST /send"));
        assert!(rx.try_recv().is_err(), "the redirect target was requested");
    }

    /// Regression: the transport error embedded the full webhook URL — a
    /// token in its query string or userinfo reached logs and the job's
    /// error column.
    #[test]
    fn transport_errors_do_not_leak_url_credentials() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let err = provider(&format!(
            "http://user:pa55@127.0.0.1:{port}/hook/s3gment?token=S3CRET"
        ))
        .send("u@example.com", "S", "<p>x</p>", None)
        .expect_err("nothing listens on the port");
        let text = format!("{err:#}");

        for secret in ["S3CRET", "pa55", "s3gment"] {
            assert!(!text.contains(secret), "{secret} leaked: {text}");
        }
        assert!(text.contains(&format!("http://127.0.0.1:{port}")), "{text}");
    }

    #[test]
    fn redacted_url_keeps_only_the_origin() {
        assert_eq!(
            redacted_url("https://u:p@api.example.com:8443/v1/send?key=k#f"),
            "https://api.example.com:8443"
        );
        assert_eq!(redacted_url("not a url"), "<unparseable webhook URL>");
    }

    #[test]
    fn payload_serializes_nested_from_and_all_fields() {
        let payload = WebhookEmailPayload {
            from: WebhookFrom {
                email: "noreply@example.com",
                name: "Crap CMS",
            },
            to: "user@example.com",
            subject: "Welcome",
            html: "<p>hi</p>",
            text: Some("hi"),
        };
        let v = serde_json::to_value(&payload).expect("serialize");
        assert_eq!(
            v,
            json!({
                "from": { "email": "noreply@example.com", "name": "Crap CMS" },
                "to": "user@example.com",
                "subject": "Welcome",
                "html": "<p>hi</p>",
                "text": "hi",
            })
        );
    }

    #[test]
    fn payload_omits_text_when_none() {
        let payload = WebhookEmailPayload {
            from: WebhookFrom {
                email: "a@b.com",
                name: "N",
            },
            to: "u@b.com",
            subject: "S",
            html: "<p>x</p>",
            text: None,
        };
        let v = serde_json::to_value(&payload).expect("serialize");
        assert!(
            v.get("text").is_none(),
            "text must be omitted entirely when None, got: {v}"
        );
    }
}
