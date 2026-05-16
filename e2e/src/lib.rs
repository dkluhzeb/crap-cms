// Test-infrastructure crate. The shared `browser`, `helpers`, and `html`
// modules expose many fixtures; pedantic lints that target library APIs
// (must_use_candidate, wildcard_imports, pub_underscore_fields) don't add
// value here. `dead_code` is allowed because individual tests use only a
// subset of the fixtures.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::items_after_statements,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::needless_pass_by_value,
    clippy::pub_underscore_fields,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::unreadable_literal,
    clippy::used_underscore_binding,
    clippy::wildcard_imports,
    dead_code
)]

pub mod browser;
pub mod email;
pub mod grpc;
pub mod helpers;
pub mod html;

pub use browser::{BrowserTestCtx, setup_browser_test, setup_browser_test_with_config};
pub use email::{
    CapturedEmail, clear_queued_emails, extract_mfa_code, extract_token, find_queued_email,
    find_queued_email_in_pool, read_queued_emails, read_queued_emails_from_pool,
    wait_for_queued_email, wait_for_queued_email_in_pool,
};
pub use grpc::{
    GrpcTestCtx, spawn_grpc_server, spawn_grpc_server_with_jobs, spawn_grpc_server_with_lua,
    spawn_grpc_server_with_rate_limit,
};
pub use helpers::HtmlTestCtx;
