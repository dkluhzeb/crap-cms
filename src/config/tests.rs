use super::*;

#[test]
fn default_config_values() {
    let config = CrapConfig::default();
    assert_eq!(config.server.admin_port, 3000);
    assert_eq!(config.server.grpc_port, 50051);
    assert_eq!(config.server.host, "0.0.0.0");
    assert_eq!(config.database.path, "data/crap.db");
    assert_eq!(config.database.pool_max_size, 32);
    assert_eq!(config.database.busy_timeout, 30000);
    assert!(!config.admin.dev_mode);
    assert!(config.admin.require_auth);
    assert!(config.admin.access.is_none());
    assert!(!config.access.default_deny);
    assert_eq!(config.pagination.default_limit, 20);
    assert_eq!(config.pagination.max_limit, 1000);
    assert_eq!(config.pagination.mode, PaginationMode::Page);
}

#[test]
fn load_missing_file_returns_defaults() {
    let dir = std::path::PathBuf::from("/tmp/nonexistent-crap-test");
    let config = CrapConfig::load(&dir).unwrap();
    assert_eq!(config.server.admin_port, 3000);
}

#[test]
fn db_path_relative() {
    let config = CrapConfig::default();
    let dir = Path::new("/my/config");
    assert_eq!(config.db_path(dir), Path::new("/my/config/data/crap.db"));
}

#[test]
fn db_path_absolute() {
    let mut config = CrapConfig::default();
    config.database.path = "/absolute/path.db".to_string();
    let dir = Path::new("/my/config");
    assert_eq!(config.db_path(dir), Path::new("/absolute/path.db"));
}

#[test]
fn locale_config_is_enabled() {
    let empty = LocaleConfig::default();
    assert!(!empty.is_enabled(), "empty locales should be disabled");

    let with_locales = LocaleConfig {
        default_locale: "en".to_string(),
        locales: vec!["en".to_string(), "de".to_string()],
        fallback: true,
    };
    assert!(with_locales.is_enabled(), "non-empty locales should be enabled");
}

#[test]
fn load_from_toml() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("crap.toml"),
        r#"
[server]
admin_port = 4000
grpc_port = 60000
host = "127.0.0.1"

[database]
path = "mydata/custom.db"

[admin]
dev_mode = false
"#,
    ).unwrap();

    let config = CrapConfig::load(tmp.path()).unwrap();
    assert_eq!(config.server.admin_port, 4000);
    assert_eq!(config.server.grpc_port, 60000);
    assert_eq!(config.server.host, "127.0.0.1");
    assert_eq!(config.database.path, "mydata/custom.db");
    assert!(!config.admin.dev_mode);
}

#[test]
fn email_config_defaults() {
    let email = EmailConfig::default();
    assert!(email.smtp_host.is_empty(), "smtp_host should be empty by default");
    assert_eq!(email.smtp_port, 587);
    assert!(email.smtp_user.is_empty());
    assert!(email.smtp_pass.is_empty());
    assert_eq!(email.from_address, "noreply@example.com");
    assert_eq!(email.from_name, "Crap CMS");
}

// -- parse_duration_string tests --

#[test]
fn parse_duration_days() {
    assert_eq!(parse_duration_string("7d"), Some(7 * 86400));
    assert_eq!(parse_duration_string("1d"), Some(86400));
    assert_eq!(parse_duration_string("30d"), Some(30 * 86400));
}

#[test]
fn parse_duration_hours() {
    assert_eq!(parse_duration_string("24h"), Some(24 * 3600));
    assert_eq!(parse_duration_string("1h"), Some(3600));
}

#[test]
fn parse_duration_minutes() {
    assert_eq!(parse_duration_string("30m"), Some(30 * 60));
    assert_eq!(parse_duration_string("1m"), Some(60));
}

#[test]
fn parse_duration_seconds() {
    assert_eq!(parse_duration_string("30s"), Some(30));
    assert_eq!(parse_duration_string("1s"), Some(1));
}

#[test]
fn parse_duration_invalid() {
    assert_eq!(parse_duration_string(""), None);
    assert_eq!(parse_duration_string("abc"), None);
    assert_eq!(parse_duration_string("7x"), None);
    assert_eq!(parse_duration_string("d"), None);
}

#[test]
fn parse_duration_whitespace() {
    assert_eq!(parse_duration_string("  7d  "), Some(7 * 86400));
}

// -- parse_filesize_string tests --

#[test]
fn parse_filesize_bytes() {
    assert_eq!(parse_filesize_string("500B"), Some(500));
    assert_eq!(parse_filesize_string("0B"), Some(0));
    assert_eq!(parse_filesize_string("1B"), Some(1));
}

#[test]
fn parse_filesize_kilobytes() {
    assert_eq!(parse_filesize_string("100KB"), Some(100 * 1024));
    assert_eq!(parse_filesize_string("1KB"), Some(1024));
}

#[test]
fn parse_filesize_megabytes() {
    assert_eq!(parse_filesize_string("50MB"), Some(50 * 1024 * 1024));
    assert_eq!(parse_filesize_string("1MB"), Some(1024 * 1024));
    assert_eq!(parse_filesize_string("100MB"), Some(100 * 1024 * 1024));
}

#[test]
fn parse_filesize_gigabytes() {
    assert_eq!(parse_filesize_string("1GB"), Some(1024 * 1024 * 1024));
    assert_eq!(parse_filesize_string("2GB"), Some(2 * 1024 * 1024 * 1024));
}

#[test]
fn parse_filesize_case_insensitive() {
    assert_eq!(parse_filesize_string("50mb"), Some(50 * 1024 * 1024));
    assert_eq!(parse_filesize_string("50Mb"), Some(50 * 1024 * 1024));
    assert_eq!(parse_filesize_string("1gb"), Some(1024 * 1024 * 1024));
    assert_eq!(parse_filesize_string("100kb"), Some(100 * 1024));
}

#[test]
fn parse_filesize_whitespace() {
    assert_eq!(parse_filesize_string("  50MB  "), Some(50 * 1024 * 1024));
}

#[test]
fn parse_filesize_invalid() {
    assert_eq!(parse_filesize_string(""), None);
    assert_eq!(parse_filesize_string("abc"), None);
    assert_eq!(parse_filesize_string("50"), None);
    assert_eq!(parse_filesize_string("MB"), None);
    assert_eq!(parse_filesize_string("50TB"), None);
}

// -- serde_filesize deserialization tests --

#[test]
fn serde_filesize_integer() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("crap.toml"),
        "[upload]\nmax_file_size = 52428800\n",
    ).unwrap();
    let config = CrapConfig::load(tmp.path()).unwrap();
    assert_eq!(config.upload.max_file_size, 52_428_800);
}

#[test]
fn serde_filesize_string_megabytes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("crap.toml"),
        "[upload]\nmax_file_size = \"50MB\"\n",
    ).unwrap();
    let config = CrapConfig::load(tmp.path()).unwrap();
    assert_eq!(config.upload.max_file_size, 50 * 1024 * 1024);
}

#[test]
fn serde_filesize_string_gigabytes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("crap.toml"),
        "[upload]\nmax_file_size = \"1GB\"\n",
    ).unwrap();
    let config = CrapConfig::load(tmp.path()).unwrap();
    assert_eq!(config.upload.max_file_size, 1024 * 1024 * 1024);
}

// -- serde_duration deserialization tests --

#[test]
fn serde_duration_integer() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("crap.toml"),
        "[auth]\ntoken_expiry = 7200\n",
    ).unwrap();
    let config = CrapConfig::load(tmp.path()).unwrap();
    assert_eq!(config.auth.token_expiry, 7200);
}

#[test]
fn serde_duration_string_hours() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("crap.toml"),
        "[auth]\ntoken_expiry = \"2h\"\n",
    ).unwrap();
    let config = CrapConfig::load(tmp.path()).unwrap();
    assert_eq!(config.auth.token_expiry, 7200);
}

#[test]
fn serde_duration_string_minutes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("crap.toml"),
        "[auth]\nlogin_lockout_seconds = \"5m\"\n",
    ).unwrap();
    let config = CrapConfig::load(tmp.path()).unwrap();
    assert_eq!(config.auth.login_lockout_seconds, 300);
}

#[test]
fn serde_duration_ms_human_string() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("crap.toml"),
        "[database]\nbusy_timeout = \"30s\"\n",
    ).unwrap();
    let config = CrapConfig::load(tmp.path()).unwrap();
    assert_eq!(config.database.busy_timeout, 30000);
}

#[test]
fn serde_duration_ms_integer_backward_compat() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("crap.toml"),
        "[database]\nbusy_timeout = 15000\n",
    ).unwrap();
    let config = CrapConfig::load(tmp.path()).unwrap();
    assert_eq!(config.database.busy_timeout, 15000);
}

#[test]
fn connection_timeout_human_string() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("crap.toml"),
        "[database]\nconnection_timeout = \"10s\"\n",
    ).unwrap();
    let config = CrapConfig::load(tmp.path()).unwrap();
    assert_eq!(config.database.connection_timeout, 10);
}

#[test]
fn serde_duration_option_string() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("crap.toml"),
        "[jobs]\nauto_purge = \"7d\"\n",
    ).unwrap();
    let config = CrapConfig::load(tmp.path()).unwrap();
    assert_eq!(config.jobs.auto_purge, Some(7 * 86400));
}

#[test]
fn serde_duration_option_integer() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("crap.toml"),
        "[jobs]\nauto_purge = 86400\n",
    ).unwrap();
    let config = CrapConfig::load(tmp.path()).unwrap();
    assert_eq!(config.jobs.auto_purge, Some(86400));
}

#[test]
fn serde_duration_option_absent_uses_default() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("crap.toml"),
        "[jobs]\nmax_concurrent = 5\n",
    ).unwrap();
    let config = CrapConfig::load(tmp.path()).unwrap();
    assert_eq!(config.jobs.auto_purge, Some(7 * 86400)); // default
}

#[test]
fn auto_purge_default_config() {
    let cfg = JobsConfig::default();
    assert_eq!(cfg.auto_purge, Some(7 * 86400));
}

#[test]
fn jobs_config_defaults() {
    let cfg = JobsConfig::default();
    assert_eq!(cfg.max_concurrent, 10);
    assert_eq!(cfg.poll_interval, 1);
    assert_eq!(cfg.cron_interval, 60);
    assert_eq!(cfg.heartbeat_interval, 10);
    assert_eq!(cfg.auto_purge, Some(7 * 86400));
    assert_eq!(cfg.image_queue_batch_size, 10);
}

#[test]
fn depth_config_defaults() {
    let depth = DepthConfig::default();
    assert_eq!(depth.default_depth, 1);
    assert_eq!(depth.max_depth, 10);
}

#[test]
fn auth_config_defaults() {
    let auth = AuthConfig::default();
    assert!(auth.secret.is_empty());
    assert_eq!(auth.token_expiry, 7200);
    assert_eq!(auth.reset_token_expiry, 3600);
}

#[test]
fn hooks_config_defaults() {
    let hooks = HooksConfig::default();
    assert!(hooks.on_init.is_empty());
    assert_eq!(hooks.max_depth, 3);
    assert!(hooks.vm_pool_size >= 4 && hooks.vm_pool_size <= 32);
}

#[test]
fn upload_config_defaults() {
    let upload = UploadConfig::default();
    assert_eq!(upload.max_file_size, 52_428_800);
}

#[test]
fn live_config_defaults() {
    let live = LiveConfig::default();
    assert!(live.enabled);
    assert_eq!(live.channel_capacity, 1024);
}

#[test]
fn cors_config_defaults() {
    let cors = CorsConfig::default();
    assert!(cors.allowed_origins.is_empty());
    assert_eq!(cors.allowed_methods, vec!["GET", "POST", "PUT", "DELETE", "PATCH", "OPTIONS"]);
    assert_eq!(cors.allowed_headers, vec!["Content-Type", "Authorization"]);
    assert!(cors.exposed_headers.is_empty());
    assert_eq!(cors.max_age_seconds, 3600);
    assert!(!cors.allow_credentials);
}

#[test]
fn cors_build_layer_disabled_when_no_origins() {
    let cors = CorsConfig::default();
    assert!(cors.build_layer().is_none());
}

#[test]
fn cors_build_layer_wildcard() {
    let cors = CorsConfig {
        allowed_origins: vec!["*".to_string()],
        ..Default::default()
    };
    assert!(cors.build_layer().is_some());
}

#[test]
fn cors_build_layer_specific_origins() {
    let cors = CorsConfig {
        allowed_origins: vec![
            "http://localhost:3000".to_string(),
            "https://example.com".to_string(),
        ],
        ..Default::default()
    };
    assert!(cors.build_layer().is_some());
}

#[test]
fn cors_build_layer_with_credentials() {
    let cors = CorsConfig {
        allowed_origins: vec!["https://example.com".to_string()],
        allow_credentials: true,
        ..Default::default()
    };
    assert!(cors.build_layer().is_some());
}

#[test]
fn cors_build_layer_wildcard_with_credentials_ignores_credentials() {
    // Wildcard + credentials is invalid per CORS spec — credentials should be ignored
    let cors = CorsConfig {
        allowed_origins: vec!["*".to_string()],
        allow_credentials: true,
        ..Default::default()
    };
    // Should still build (doesn't panic), just logs a warning
    assert!(cors.build_layer().is_some());
}

#[test]
fn cors_build_layer_with_exposed_headers() {
    let cors = CorsConfig {
        allowed_origins: vec!["https://example.com".to_string()],
        exposed_headers: vec!["X-Custom-Header".to_string()],
        ..Default::default()
    };
    assert!(cors.build_layer().is_some());
}

#[test]
fn cors_config_from_toml() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("crap.toml"),
        r#"
[cors]
allowed_origins = ["https://example.com", "https://app.example.com"]
allowed_methods = ["GET", "POST"]
allowed_headers = ["Content-Type", "Authorization", "X-Custom"]
exposed_headers = ["X-Request-Id"]
max_age_seconds = 7200
allow_credentials = true
"#,
    ).unwrap();
    let config = CrapConfig::load(tmp.path()).unwrap();
    assert_eq!(config.cors.allowed_origins, vec!["https://example.com", "https://app.example.com"]);
    assert_eq!(config.cors.allowed_methods, vec!["GET", "POST"]);
    assert_eq!(config.cors.allowed_headers, vec!["Content-Type", "Authorization", "X-Custom"]);
    assert_eq!(config.cors.exposed_headers, vec!["X-Request-Id"]);
    assert_eq!(config.cors.max_age_seconds, 7200);
    assert!(config.cors.allow_credentials);
}

#[test]
fn load_invalid_toml_returns_error() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("crap.toml"), "invalid { toml").unwrap();
    let result = CrapConfig::load(tmp.path());
    assert!(result.is_err());
}

#[test]
fn load_partial_toml_uses_defaults_for_missing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("crap.toml"),
        "[server]\nadmin_port = 5000\n",
    ).unwrap();
    let config = CrapConfig::load(tmp.path()).unwrap();
    assert_eq!(config.server.admin_port, 5000);
    // Other fields should be defaults
    assert_eq!(config.server.grpc_port, 50051);
    assert_eq!(config.database.path, "data/crap.db");
}

#[test]
fn admin_config_defaults() {
    let admin = AdminConfig::default();
    assert!(!admin.dev_mode);
    assert!(admin.require_auth);
    assert!(admin.access.is_none());
}

#[test]
fn admin_config_from_toml() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("crap.toml"),
        "[admin]\ndev_mode = true\nrequire_auth = false\naccess = \"access.admin_panel\"\n",
    ).unwrap();
    let config = CrapConfig::load(tmp.path()).unwrap();
    assert!(config.admin.dev_mode);
    assert!(!config.admin.require_auth);
    assert_eq!(config.admin.access, Some("access.admin_panel".to_string()));
}

#[test]
fn admin_config_partial_toml_uses_defaults() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("crap.toml"),
        "[admin]\ndev_mode = true\n",
    ).unwrap();
    let config = CrapConfig::load(tmp.path()).unwrap();
    assert!(config.admin.dev_mode);
    assert!(config.admin.require_auth); // default
    assert!(config.admin.access.is_none()); // default
}

#[test]
fn access_config_default_deny_false_by_default() {
    let config = CrapConfig::default();
    assert!(!config.access.default_deny);
}

#[test]
fn access_config_default_deny_from_toml() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("crap.toml"),
        "[access]\ndefault_deny = true\n",
    ).unwrap();
    let config = CrapConfig::load(tmp.path()).unwrap();
    assert!(config.access.default_deny);
}

#[test]
fn database_config_from_toml() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("crap.toml"),
        "[database]\npool_max_size = 32\nbusy_timeout = 60000\n",
    ).unwrap();
    let config = CrapConfig::load(tmp.path()).unwrap();
    assert_eq!(config.database.pool_max_size, 32);
    assert_eq!(config.database.busy_timeout, 60000);
}

#[test]
fn auth_reset_token_expiry_from_toml() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("crap.toml"),
        "[auth]\nreset_token_expiry = 1800\n",
    ).unwrap();
    let config = CrapConfig::load(tmp.path()).unwrap();
    assert_eq!(config.auth.reset_token_expiry, 1800);
}

#[test]
fn jobs_image_queue_batch_size_from_toml() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("crap.toml"),
        "[jobs]\nimage_queue_batch_size = 50\n",
    ).unwrap();
    let config = CrapConfig::load(tmp.path()).unwrap();
    assert_eq!(config.jobs.image_queue_batch_size, 50);
}

// -- crap_version / check_version tests --

#[test]
fn check_version_none_returns_ok() {
    assert!(CrapConfig::check_version_against(None, "0.1.0").is_none());
}

#[test]
fn check_version_empty_returns_ok() {
    assert!(CrapConfig::check_version_against(Some(""), "0.1.0").is_none());
}

#[test]
fn check_version_exact_match() {
    assert!(CrapConfig::check_version_against(Some("0.1.0"), "0.1.0").is_none());
}

#[test]
fn check_version_prefix_match() {
    assert!(CrapConfig::check_version_against(Some("0.1"), "0.1.0").is_none());
    assert!(CrapConfig::check_version_against(Some("0.1"), "0.1.3").is_none());
    assert!(CrapConfig::check_version_against(Some("0"), "0.1.0").is_none());
}

#[test]
fn check_version_prefix_no_false_positive() {
    // "0.1" should NOT match "0.10.0" — the next char must be '.'
    let result = CrapConfig::check_version_against(Some("0.1"), "0.10.0");
    assert!(result.is_some());
}

#[test]
fn check_version_mismatch() {
    let result = CrapConfig::check_version_against(Some("0.2.0"), "0.1.0");
    assert!(result.is_some());
    let msg = result.unwrap();
    assert!(msg.contains("0.2.0"), "should mention required version");
    assert!(msg.contains("0.1.0"), "should mention running version");
}

#[test]
fn check_version_prefix_mismatch() {
    let result = CrapConfig::check_version_against(Some("1.0"), "0.1.0");
    assert!(result.is_some());
}

#[test]
fn check_version_from_struct() {
    // Test via the public check_version() method on a default config (crap_version = None)
    let config = CrapConfig::default();
    assert!(config.crap_version.is_none());
    assert!(config.check_version().is_none());
}

#[test]
fn check_version_from_struct_with_value() {
    let mut config = CrapConfig::default();
    config.crap_version = Some(env!("CARGO_PKG_VERSION").to_string());
    assert!(config.check_version().is_none());
}

#[test]
fn crap_version_from_toml() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("crap.toml"),
        "crap_version = \"0.1.0\"\n",
    ).unwrap();
    let config = CrapConfig::load(tmp.path()).unwrap();
    assert_eq!(config.crap_version, Some("0.1.0".to_string()));
}

#[test]
fn crap_version_absent_in_toml_is_none() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("crap.toml"),
        "[server]\nadmin_port = 3000\n",
    ).unwrap();
    let config = CrapConfig::load(tmp.path()).unwrap();
    assert!(config.crap_version.is_none());
}

// -- substitute_env_vars tests --

#[test]
fn env_subst_simple() {
    std::env::set_var("CRAP_TEST_HOST", "127.0.0.1");
    let result = substitute_env_vars("host = \"${CRAP_TEST_HOST}\"").unwrap();
    assert_eq!(result, "host = \"127.0.0.1\"");
    std::env::remove_var("CRAP_TEST_HOST");
}

#[test]
fn env_subst_with_default() {
    std::env::remove_var("CRAP_TEST_MISSING");
    let result = substitute_env_vars("port = ${CRAP_TEST_MISSING:-3000}").unwrap();
    assert_eq!(result, "port = 3000");
}

#[test]
fn env_subst_default_not_used_when_set() {
    std::env::set_var("CRAP_TEST_PORT", "8080");
    let result = substitute_env_vars("port = ${CRAP_TEST_PORT:-3000}").unwrap();
    assert_eq!(result, "port = 8080");
    std::env::remove_var("CRAP_TEST_PORT");
}

#[test]
fn env_subst_empty_uses_default() {
    std::env::set_var("CRAP_TEST_EMPTY", "");
    let result = substitute_env_vars("val = \"${CRAP_TEST_EMPTY:-fallback}\"").unwrap();
    assert_eq!(result, "val = \"fallback\"");
    std::env::remove_var("CRAP_TEST_EMPTY");
}

#[test]
fn env_subst_missing_no_default_errors() {
    std::env::remove_var("CRAP_TEST_NOEXIST_XYZ");
    let result = substitute_env_vars("secret = \"${CRAP_TEST_NOEXIST_XYZ}\"");
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("CRAP_TEST_NOEXIST_XYZ"));
}

#[test]
fn env_subst_multiple() {
    std::env::set_var("CRAP_TEST_A", "hello");
    std::env::set_var("CRAP_TEST_B", "world");
    let result = substitute_env_vars("${CRAP_TEST_A} ${CRAP_TEST_B}").unwrap();
    assert_eq!(result, "hello world");
    std::env::remove_var("CRAP_TEST_A");
    std::env::remove_var("CRAP_TEST_B");
}

#[test]
fn env_subst_no_vars_passthrough() {
    let input = "admin_port = 3000\nhost = \"0.0.0.0\"";
    let result = substitute_env_vars(input).unwrap();
    assert_eq!(result, input);
}

#[test]
fn env_subst_in_toml_load() {
    std::env::set_var("CRAP_TEST_ADMIN_PORT", "9999");
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("crap.toml"),
        "[server]\nadmin_port = 9999\nhost = \"${CRAP_TEST_HOST2:-0.0.0.0}\"\n",
    ).unwrap();
    let config = CrapConfig::load(tmp.path()).unwrap();
    assert_eq!(config.server.admin_port, 9999);
    assert_eq!(config.server.host, "0.0.0.0");
    std::env::remove_var("CRAP_TEST_ADMIN_PORT");
}

#[test]
fn env_subst_ignores_comments() {
    // ${UNSET_VAR_IN_COMMENT} in a TOML comment should not cause an error
    std::env::remove_var("CRAP_TEST_UNSET_COMMENT_VAR");
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("crap.toml"),
        "# Set ${CRAP_TEST_UNSET_COMMENT_VAR} for production\n\
         [server]\nadmin_port = 3000\n",
    ).unwrap();
    let config = CrapConfig::load(tmp.path()).unwrap();
    assert_eq!(config.server.admin_port, 3000);
}

#[test]
fn env_subst_in_string_values_via_load() {
    std::env::set_var("CRAP_TEST_SMTP_HOST", "mail.example.com");
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("crap.toml"),
        "[email]\nsmtp_host = \"${CRAP_TEST_SMTP_HOST}\"\n",
    ).unwrap();
    let config = CrapConfig::load(tmp.path()).unwrap();
    assert_eq!(config.email.smtp_host, "mail.example.com");
    std::env::remove_var("CRAP_TEST_SMTP_HOST");
}

// -- substitute_in_value tests --

#[test]
fn substitute_in_value_string() {
    std::env::set_var("CRAP_TEST_SIV", "replaced");
    let mut val = toml::Value::String("${CRAP_TEST_SIV}".to_string());
    substitute_in_value(&mut val).unwrap();
    assert_eq!(val.as_str().unwrap(), "replaced");
    std::env::remove_var("CRAP_TEST_SIV");
}

#[test]
fn substitute_in_value_table() {
    std::env::set_var("CRAP_TEST_SIV2", "value2");
    let mut tbl = toml::map::Map::new();
    tbl.insert("key".to_string(), toml::Value::String("${CRAP_TEST_SIV2}".to_string()));
    tbl.insert("num".to_string(), toml::Value::Integer(42));
    let mut val = toml::Value::Table(tbl);
    substitute_in_value(&mut val).unwrap();
    assert_eq!(val.get("key").unwrap().as_str().unwrap(), "value2");
    assert_eq!(val.get("num").unwrap().as_integer().unwrap(), 42);
    std::env::remove_var("CRAP_TEST_SIV2");
}

#[test]
fn substitute_in_value_array() {
    std::env::set_var("CRAP_TEST_SIV3", "item");
    let mut val = toml::Value::Array(vec![
        toml::Value::String("${CRAP_TEST_SIV3}".to_string()),
        toml::Value::Boolean(true),
    ]);
    substitute_in_value(&mut val).unwrap();
    assert_eq!(val.as_array().unwrap()[0].as_str().unwrap(), "item");
    assert!(val.as_array().unwrap()[1].as_bool().unwrap());
    std::env::remove_var("CRAP_TEST_SIV3");
}

#[test]
fn substitute_in_value_non_string_untouched() {
    let mut val = toml::Value::Integer(99);
    substitute_in_value(&mut val).unwrap();
    assert_eq!(val.as_integer().unwrap(), 99);

    let mut val = toml::Value::Float(3.14);
    substitute_in_value(&mut val).unwrap();
    assert!((val.as_float().unwrap() - 3.14).abs() < f64::EPSILON);

    let mut val = toml::Value::Boolean(true);
    substitute_in_value(&mut val).unwrap();
    assert!(val.as_bool().unwrap());
}

#[test]
fn locale_validation_valid_codes() {
    let config = LocaleConfig {
        default_locale: "en".to_string(),
        locales: vec!["en".to_string(), "de".to_string(), "pt-BR".to_string(), "zh_CN".to_string()],
        fallback: true,
    };
    assert!(config.validate().is_ok());
}

#[test]
fn locale_validation_rejects_sql_injection() {
    let config = LocaleConfig {
        default_locale: "en'; DROP TABLE posts; --".to_string(),
        locales: vec![],
        fallback: true,
    };
    assert!(config.validate().is_err());
}

#[test]
fn locale_validation_rejects_empty() {
    let config = LocaleConfig {
        default_locale: "".to_string(),
        locales: vec![],
        fallback: true,
    };
    assert!(config.validate().is_err());
}

#[test]
fn locale_validation_rejects_bad_locale_in_list() {
    let config = LocaleConfig {
        default_locale: "en".to_string(),
        locales: vec!["en".to_string(), "de/../etc".to_string()],
        fallback: true,
    };
    assert!(config.validate().is_err());
}

// ── Password policy tests ──────────────────────────────────────────────

#[test]
fn password_policy_defaults() {
    let policy = PasswordPolicy::default();
    assert_eq!(policy.min_length, 8);
    assert_eq!(policy.max_length, 128);
    assert!(!policy.require_uppercase);
    assert!(!policy.require_lowercase);
    assert!(!policy.require_digit);
    assert!(!policy.require_special);
}

#[test]
fn password_policy_accepts_valid() {
    let policy = PasswordPolicy::default();
    assert!(policy.validate("abcdefgh").is_ok());
    assert!(policy.validate("12345678").is_ok());
}

#[test]
fn password_policy_rejects_too_short() {
    let policy = PasswordPolicy { min_length: 8, ..Default::default() };
    assert!(policy.validate("short").is_err());
    assert!(policy.validate("1234567").is_err());
    assert!(policy.validate("12345678").is_ok());
}

#[test]
fn password_policy_rejects_too_long() {
    let policy = PasswordPolicy { max_length: 10, ..Default::default() };
    assert!(policy.validate("12345678").is_ok());
    assert!(policy.validate("12345678901").is_err());
}

#[test]
fn password_policy_require_uppercase() {
    let policy = PasswordPolicy { require_uppercase: true, ..Default::default() };
    assert!(policy.validate("alllower").is_err());
    assert!(policy.validate("hasUpper1").is_ok());
}

#[test]
fn password_policy_require_lowercase() {
    let policy = PasswordPolicy { require_lowercase: true, ..Default::default() };
    assert!(policy.validate("ALLUPPER").is_err());
    assert!(policy.validate("HASLOWERa").is_ok());
}

#[test]
fn password_policy_require_digit() {
    let policy = PasswordPolicy { require_digit: true, ..Default::default() };
    assert!(policy.validate("nodigits").is_err());
    assert!(policy.validate("hasdigit1").is_ok());
}

#[test]
fn password_policy_require_special() {
    let policy = PasswordPolicy { require_special: true, ..Default::default() };
    assert!(policy.validate("nospecial1").is_err());
    assert!(policy.validate("special!1").is_ok());
}

#[test]
fn password_policy_all_requirements() {
    let policy = PasswordPolicy {
        min_length: 8,
        max_length: 128,
        require_uppercase: true,
        require_lowercase: true,
        require_digit: true,
        require_special: true,
    };
    assert!(policy.validate("Abc1234!").is_ok());
    assert!(policy.validate("abc1234!").is_err(), "missing uppercase");
    assert!(policy.validate("ABC1234!").is_err(), "missing lowercase");
    assert!(policy.validate("Abcdefg!").is_err(), "missing digit");
    assert!(policy.validate("Abc12345").is_err(), "missing special");
    assert!(policy.validate("Ac1!").is_err(), "too short");
}

#[test]
fn password_policy_from_toml() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("crap.toml"), r#"
[auth.password_policy]
min_length = 12
require_uppercase = true
require_digit = true
"#).unwrap();
    let config = CrapConfig::load(tmp.path()).unwrap();
    assert_eq!(config.auth.password_policy.min_length, 12);
    assert!(config.auth.password_policy.require_uppercase);
    assert!(config.auth.password_policy.require_digit);
    assert!(!config.auth.password_policy.require_lowercase);
    assert!(!config.auth.password_policy.require_special);
}

// ── validate() ───────────────────────────────────────────────────────

#[test]
fn validate_default_config_passes() {
    let config = CrapConfig::default();
    assert!(config.validate().is_ok());
}

#[test]
fn validate_pool_max_size_zero_errors() {
    let mut config = CrapConfig::default();
    config.database.pool_max_size = 0;
    let err = config.validate().unwrap_err();
    assert!(err.to_string().contains("pool_max_size"));
}

#[test]
fn validate_connection_timeout_zero_errors() {
    let mut config = CrapConfig::default();
    config.database.connection_timeout = 0;
    let err = config.validate().unwrap_err();
    assert!(err.to_string().contains("connection_timeout"));
}

#[test]
fn validate_vm_pool_size_zero_errors() {
    let mut config = CrapConfig::default();
    config.hooks.vm_pool_size = 0;
    let err = config.validate().unwrap_err();
    assert!(err.to_string().contains("vm_pool_size"));
}

#[test]
fn validate_max_concurrent_zero_warns_but_passes() {
    let mut config = CrapConfig::default();
    config.jobs.max_concurrent = 0;
    // Should pass (warning only, not fatal)
    assert!(config.validate().is_ok());
}

#[test]
fn validate_short_auth_secret_warns_but_passes() {
    let mut config = CrapConfig::default();
    config.auth.secret = "short".to_string();
    // Should pass (warning only)
    assert!(config.validate().is_ok());
}

#[test]
fn validate_max_depth_zero_warns_but_passes() {
    let mut config = CrapConfig::default();
    config.depth.max_depth = 0;
    // Should pass (warning only)
    assert!(config.validate().is_ok());
}
