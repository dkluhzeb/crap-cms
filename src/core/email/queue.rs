//! Queued email delivery via the job system.
//!
//! `queue_email()` inserts a `_system_email` job that the scheduler
//! processes with retries and exponential backoff.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::db::{DbConnection, query};

use super::validation::validate_no_crlf;

/// System job slug for queued emails.
pub const SYSTEM_EMAIL_JOB: &str = "_system_email";

/// Data payload for a queued email job.
#[derive(Debug, Serialize, Deserialize)]
pub struct EmailJobData {
    pub to: String,
    pub subject: String,
    pub html: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// Queue an email for async delivery via the job system.
///
/// The email will be processed by the scheduler with retries on failure.
/// Returns the job run ID.
pub fn queue_email(
    conn: &dyn DbConnection,
    to: &str,
    subject: &str,
    html: &str,
    text: Option<&str>,
    max_attempts: u32,
    queue: &str,
) -> Result<String> {
    validate_no_crlf("to", to)?;
    validate_no_crlf("subject", subject)?;

    let data = EmailJobData {
        to: to.to_string(),
        subject: subject.to_string(),
        html: html.to_string(),
        text: text.map(|s| s.to_string()),
    };

    let data_json = serde_json::to_string(&data)?;

    let job = query::jobs::insert_job(
        conn,
        SYSTEM_EMAIL_JOB,
        &data_json,
        "system",
        max_attempts,
        queue,
    )?;

    tracing::debug!(
        "Queued email to {} (subject: \"{}\") as job {}",
        to,
        subject,
        job.id
    );

    Ok(job.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_job_data_serialization() {
        let data = EmailJobData {
            to: "user@example.com".to_string(),
            subject: "Test".to_string(),
            html: "<p>Hello</p>".to_string(),
            text: Some("Hello".to_string()),
        };
        let json = serde_json::to_string(&data).unwrap();
        assert!(json.contains("user@example.com"));
        assert!(json.contains("Hello"));

        let parsed: EmailJobData = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.to, "user@example.com");
        assert_eq!(parsed.text, Some("Hello".to_string()));
    }

    #[test]
    fn email_job_data_without_text() {
        let data = EmailJobData {
            to: "user@example.com".to_string(),
            subject: "Test".to_string(),
            html: "<p>Hi</p>".to_string(),
            text: None,
        };
        let json = serde_json::to_string(&data).unwrap();
        assert!(!json.contains("text"));
    }
}
