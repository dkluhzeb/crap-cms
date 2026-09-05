//! Secret-redaction partition guard (ledger class **F17**).
//!
//! Every secret-bearing value in `CrapConfig` must be unreadable
//! through the two secondary channels an operator or Lua hook can
//! reach: `Debug` formatting (logs, error contexts, `dbg!`) and
//! `Serialize` (the `crap.config`-style Lua exposure). The established
//! pattern is a redacting newtype (`JwtSecret`, `S3SecretKey`,
//! `SmtpPassword`, `McpApiKey`); this test is the partition that forces
//! every *new* secret field through the same decision — a bare `String`
//! secret fails here on its first appearance.

use crap_cms::config::CrapConfig;

/// Sentinel secrets, one per secret-bearing config field. Distinctive
/// enough that an accidental echo can't be a coincidence.
const SENTINELS: &[(&str, &str)] = &[
    (
        "auth.secret",
        "SENTINEL-JWT-0123456789abcdef0123456789abcdef",
    ),
    ("upload.s3.secret_key", "SENTINEL-S3-SECRET-KEY-VALUE"),
    ("email.smtp_pass", "SENTINEL-SMTP-PASSWORD-VALUE"),
    ("mcp.api_key", "SENTINEL-MCP-API-KEY-0123456789abcdef00"),
    ("cache.redis_url password", "SENTINEL-REDIS-PW"),
    ("auth.rate_limit_redis_url password", "SENTINEL-RL-REDIS-PW"),
    (
        "email.webhook_headers Authorization",
        "SENTINEL-WEBHOOK-BEARER",
    ),
];

fn sentinel_config() -> CrapConfig {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("crap.toml"),
        r#"
[server]
admin_port = 3000
grpc_port = 50051

[database]
path = "data/crap.db"

[auth]
secret = "SENTINEL-JWT-0123456789abcdef0123456789abcdef"
rate_limit_backend = "redis"
rate_limit_redis_url = "redis://user:SENTINEL-RL-REDIS-PW@localhost:6379"

[upload]
storage = "s3"

[upload.s3]
bucket = "b"
access_key = "AKIASENTINELACCESSKEY"
secret_key = "SENTINEL-S3-SECRET-KEY-VALUE"

[email]
provider = "webhook"
webhook_url = "https://example.com/hook"
smtp_pass = "SENTINEL-SMTP-PASSWORD-VALUE"

[email.webhook_headers]
Authorization = "Bearer SENTINEL-WEBHOOK-BEARER"

[cache]
backend = "redis"
redis_url = "redis://user:SENTINEL-REDIS-PW@localhost:6379"

[mcp]
enabled = true
http = true
api_key = "SENTINEL-MCP-API-KEY-0123456789abcdef00"
"#,
    )
    .unwrap();

    CrapConfig::load(tmp.path()).expect("sentinel config must load")
}

/// No sentinel survives into `Debug` output.
#[test]
fn debug_output_redacts_every_secret() {
    let cfg = sentinel_config();
    let debug = format!("{cfg:?}");

    let leaks: Vec<&str> = SENTINELS
        .iter()
        .filter(|(_, v)| debug.contains(v))
        .map(|(name, _)| *name)
        .collect();

    assert!(
        leaks.is_empty(),
        "secret value(s) readable through CrapConfig's Debug output — \
         wrap the field in a redacting newtype like JwtSecret/S3SecretKey: \
         {leaks:?}"
    );
}

/// No sentinel survives into `Serialize` output (the Lua-facing
/// exposure path).
#[test]
fn serialized_output_redacts_every_secret() {
    let cfg = sentinel_config();
    let json = serde_json::to_string(&cfg).expect("config serializes");

    let leaks: Vec<&str> = SENTINELS
        .iter()
        .filter(|(_, v)| json.contains(v))
        .map(|(name, _)| *name)
        .collect();

    assert!(
        leaks.is_empty(),
        "secret value(s) readable through CrapConfig's Serialize output: \
         {leaks:?}"
    );
}

/// Positive control (ledger class **D4**): the sentinel really is in
/// the loaded config (redaction is doing work, not the loader dropping
/// the value) — prove it by round-tripping one secret through its
/// accessor.
#[test]
fn sentinels_actually_load() {
    let cfg = sentinel_config();
    let secret: &str = cfg.auth.secret.as_ref();
    assert_eq!(secret, "SENTINEL-JWT-0123456789abcdef0123456789abcdef");
    assert!(cfg.cache.redis_url.as_str().contains("SENTINEL-REDIS-PW"));
}
