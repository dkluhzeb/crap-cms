//! Queued email delivery via the job system.
//!
//! `queue_email()` inserts a `_system_email` job that the scheduler
//! processes with retries and exponential backoff.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::{
    config::EmailConfig,
    db::{DbConnection, query},
};

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
/// `config` supplies the retry budget (`queue_retries + 1` total attempts)
/// and the queue name. Returns the job run ID.
///
/// # Errors
///
/// Returns an error if `to` or `subject` contain CRLF (header injection),
/// or if serializing/inserting the job fails.
pub fn queue_email(
    conn: &dyn DbConnection,
    data: &EmailJobData,
    config: &EmailConfig,
) -> Result<String> {
    validate_no_crlf("to", &data.to)?;
    validate_no_crlf("subject", &data.subject)?;

    let data_json = serde_json::to_string(data)?;

    let job = query::jobs::insert_job(
        conn,
        SYSTEM_EMAIL_JOB,
        &data_json,
        "system",
        config.queue_retries + 1,
        &config.queue_name,
        0,
    )?;

    tracing::debug!(
        "Queued email to {} (subject: \"{}\") as job {}",
        data.to,
        data.subject,
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
