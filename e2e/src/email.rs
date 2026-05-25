//! Read queued emails from the test app's DB. The auth flows
//! (forgot-password, email-verify, MFA-by-email) don't call the email
//! provider directly — they queue jobs into `_crap_jobs` for the
//! scheduler to deliver. Since tests don't run the scheduler, the
//! jobs sit pending and we read them out of the queue table.
//!
//! Use [`wait_for_queued_email`] for typical "click submit → assert
//! email" patterns. [`extract_token`] parses `?token=...` from a URL
//! in the email body.

use std::time::{Duration, Instant};

use crap_cms::core::email::SYSTEM_EMAIL_JOB;
use crap_cms::db::{DbConnection, DbPool, DbValue};

use crate::helpers::TestApp;

#[derive(Debug, Clone)]
pub struct CapturedEmail {
    pub to: String,
    pub subject: String,
    pub html: String,
    pub text: Option<String>,
}

/// Read every queued email (pending `_system_email` jobs) from the
/// test app's DB. Order matches insertion order via `created_at`.
#[must_use]
pub fn read_queued_emails(app: &TestApp) -> Vec<CapturedEmail> {
    read_queued_emails_from_pool(&app.pool)
}

/// Pool-based variant of [`read_queued_emails`]. Same query, but
/// usable from harnesses (e.g. `GrpcTestCtx`) that don't carry a
/// `TestApp`.
#[must_use]
pub fn read_queued_emails_from_pool(pool: &DbPool) -> Vec<CapturedEmail> {
    let conn = pool.get().expect("pool");
    let rows = conn
        .query_all(
            "SELECT data FROM _crap_jobs WHERE slug = ? AND status = 'pending' ORDER BY created_at",
            &[DbValue::Text(SYSTEM_EMAIL_JOB.to_string())],
        )
        .expect("query jobs");
    rows.into_iter()
        .filter_map(|row| {
            let DbValue::Text(json) = row.get_value(0).cloned()? else {
                return None;
            };
            let parsed: serde_json::Value = serde_json::from_str(&json).ok()?;
            Some(CapturedEmail {
                to: parsed.get("to")?.as_str()?.to_string(),
                subject: parsed.get("subject")?.as_str()?.to_string(),
                html: parsed.get("html")?.as_str()?.to_string(),
                text: parsed
                    .get("text")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            })
        })
        .collect()
}

/// Latest queued email addressed to `to`, if any.
#[must_use]
pub fn find_queued_email(app: &TestApp, to: &str) -> Option<CapturedEmail> {
    find_queued_email_in_pool(&app.pool, to)
}

/// Pool-based variant of [`find_queued_email`].
#[must_use]
pub fn find_queued_email_in_pool(pool: &DbPool, to: &str) -> Option<CapturedEmail> {
    read_queued_emails_from_pool(pool)
        .into_iter()
        .rev()
        .find(|e| e.to == to)
}

/// Poll the queue until an email to `to` appears, or `timeout`
/// elapses. The handler may queue the email from a `spawn_blocking`
/// task that completes after the HTTP response returns, so a brief
/// wait is normal.
#[must_use]
pub fn wait_for_queued_email(app: &TestApp, to: &str, timeout: Duration) -> Option<CapturedEmail> {
    wait_for_queued_email_in_pool(&app.pool, to, timeout)
}

/// Pool-based variant of [`wait_for_queued_email`].
#[must_use]
pub fn wait_for_queued_email_in_pool(
    pool: &DbPool,
    to: &str,
    timeout: Duration,
) -> Option<CapturedEmail> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(email) = find_queued_email_in_pool(pool, to) {
            return Some(email);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Delete all pending email jobs. Useful for tests that want to
/// assert "exactly one email was queued after action X" by clearing
/// before the action.
pub fn clear_queued_emails(app: &TestApp) {
    let conn = app.pool.get().expect("pool");
    conn.execute(
        "DELETE FROM _crap_jobs WHERE slug = ?",
        &[DbValue::Text(SYSTEM_EMAIL_JOB.to_string())],
    )
    .expect("delete jobs");
}

/// Parse a `token=...` query value from the first URL in the email
/// body matching `path_prefix` (e.g. `/admin/reset-password`). Looks
/// in `html` first, then falls back to `text`. Decodes `&#x3D;` /
/// `&amp;` since Handlebars HTML-escapes `=` and `&` when rendering
/// URLs into href attributes.
#[must_use]
pub fn extract_token(email: &CapturedEmail, path_prefix: &str) -> Option<String> {
    let html_decoded = decode_basic_entities(&email.html);
    let text = email.text.as_deref().unwrap_or("");
    for body in [html_decoded.as_str(), text] {
        let Some(start) = body.find(path_prefix) else {
            continue;
        };
        let chunk = &body[start..];
        let Some(token_idx) = chunk.find("token=") else {
            continue;
        };
        let after = &chunk[token_idx + "token=".len()..];
        let end = after
            .find(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_' || c == '.'))
            .unwrap_or(after.len());
        if end > 0 {
            return Some(after[..end].to_string());
        }
    }
    None
}

fn decode_basic_entities(s: &str) -> String {
    s.replace("&#x3D;", "=")
        .replace("&#61;", "=")
        .replace("&amp;", "&")
}

/// Extract a 6-digit MFA code from a verification email. Looks for
/// `<span class="code">123456</span>` (the shape rendered by
/// `templates/email/mfa_code.hbs`); falls back to the first 6-digit
/// run in the text body if HTML doesn't contain the marker.
#[must_use]
pub fn extract_mfa_code(email: &CapturedEmail) -> Option<String> {
    let html_decoded = decode_basic_entities(&email.html);
    let marker = r#"class="code">"#;
    if let Some(idx) = html_decoded.find(marker) {
        let after = &html_decoded[idx + marker.len()..];
        let code: String = after
            .chars()
            .skip_while(|c| c.is_whitespace())
            .take_while(char::is_ascii_digit)
            .collect();
        if code.len() == 6 {
            return Some(code);
        }
    }
    None
}
