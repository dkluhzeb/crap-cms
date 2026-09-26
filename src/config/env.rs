//! Environment variable substitution in TOML config values.

use std::env;
use std::sync::LazyLock;

use anyhow::{Context as _, Result};
use regex::Regex;

use crate::config::ErrorReport;

/// A placeholder to expand (`inner`), or one escaped with a second `$` that
/// stays in the value literally, minus the escaping `$` (`escaped`).
static ENV_VAR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\$(?P<escaped>\$\{[^}]*\})|\$\{(?P<inner>[^}]+)\}").expect("env var regex")
});

/// Recursively walk a TOML `Value` tree and substitute `${VAR}` / `${VAR:-default}`
/// in all `String` nodes. Tables and arrays are descended into; other types are untouched.
///
/// Every placeholder that can't be expanded is recorded in `report` under the
/// value's dotted `path`, and the walk goes on, so every unset variable is
/// reported at once. A value that can't be expanded is taken out — a table
/// key, or the whole array holding it — so decoding what is left reports no
/// second problem for it. Returns whether `value` itself expanded.
pub(crate) fn substitute_in_value(
    value: &mut toml::Value,
    path: &str,
    report: &mut ErrorReport,
) -> bool {
    match value {
        toml::Value::String(s) => match substitute_env_vars(s) {
            Ok(expanded) => {
                *s = expanded;
                true
            }
            Err(e) => {
                report.push(&e.context(path.to_string()));
                false
            }
        },
        toml::Value::Array(arr) => arr.iter_mut().enumerate().fold(true, |all, (index, item)| {
            substitute_in_value(item, &format!("{path}[{index}]"), report) && all
        }),
        toml::Value::Table(tbl) => {
            let mut all = true;

            tbl.retain(|key, val| {
                let expanded = substitute_in_value(val, &format!("{path}.{key}"), report);
                all &= expanded;
                expanded
            });

            all
        }
        _ => true, // Integer, Float, Boolean, Datetime — no substitution
    }
}

/// Replace `${VAR}` and `${VAR:-default}` placeholders with environment variable values.
///
/// - `${VAR}` — replaced with the value of `VAR`. Returns an error if `VAR` is unset.
/// - `${VAR:-fallback}` — replaced with `VAR` if set and non-empty, otherwise `fallback`.
/// - `$${...}` — a literal `${...}`, not substituted (for values that contain
///   the placeholder syntax themselves, such as a password with `${` in it).
pub(super) fn substitute_env_vars(input: &str) -> Result<String> {
    let mut result = String::with_capacity(input.len());
    let mut last_end = 0;

    for cap in ENV_VAR_RE.captures_iter(input) {
        let full_match = cap.get(0).expect("regex group 0 always exists");

        result.push_str(&input[last_end..full_match.start()]);

        if let Some(escaped) = cap.name("escaped") {
            result.push_str(escaped.as_str());
        } else {
            result.push_str(&expand_placeholder(&cap["inner"])?);
        }

        last_end = full_match.end();
    }

    result.push_str(&input[last_end..]);

    Ok(result)
}

/// The value of one `VAR` / `VAR:-default` placeholder body.
fn expand_placeholder(inner: &str) -> Result<String> {
    if let Some((var_name, default_val)) = inner.split_once(":-") {
        return Ok(match env::var(var_name) {
            Ok(val) if !val.is_empty() => val,
            _ => default_val.to_string(),
        });
    }

    env::var(inner).with_context(|| {
        format!(
            "Environment variable '{inner}' referenced in crap.toml is not set \
             (use ${{{inner}:-default}} for a fallback, or $${{{inner}}} for a literal)"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::CrapConfig, test_support::env_lock};

    /// # Safety
    ///
    /// The caller must hold the crate-wide environment lock ([`env_lock`]):
    /// the environment is process-wide state and the test harness runs tests
    /// on many threads at once.
    unsafe fn set_env(key: &str, val: &str) {
        unsafe { env::set_var(key, val) };
    }

    /// # Safety
    ///
    /// Same contract as [`set_env`] — the caller holds the environment lock.
    unsafe fn remove_env(key: &str) {
        unsafe { env::remove_var(key) };
    }

    #[test]
    fn env_subst_simple() {
        let _guard = env_lock();

        unsafe { set_env("CRAP_TEST_HOST", "127.0.0.1") };
        let result = substitute_env_vars("host = \"${CRAP_TEST_HOST}\"").unwrap();
        assert_eq!(result, "host = \"127.0.0.1\"");
        unsafe { remove_env("CRAP_TEST_HOST") };
    }

    #[test]
    fn env_subst_with_default() {
        let _guard = env_lock();

        unsafe { remove_env("CRAP_TEST_MISSING") };
        let result = substitute_env_vars("port = ${CRAP_TEST_MISSING:-3000}").unwrap();
        assert_eq!(result, "port = 3000");
    }

    #[test]
    fn env_subst_default_not_used_when_set() {
        let _guard = env_lock();

        unsafe { set_env("CRAP_TEST_PORT", "8080") };
        let result = substitute_env_vars("port = ${CRAP_TEST_PORT:-3000}").unwrap();
        assert_eq!(result, "port = 8080");
        unsafe { remove_env("CRAP_TEST_PORT") };
    }

    #[test]
    fn env_subst_empty_uses_default() {
        let _guard = env_lock();

        unsafe { set_env("CRAP_TEST_EMPTY", "") };
        let result = substitute_env_vars("val = \"${CRAP_TEST_EMPTY:-fallback}\"").unwrap();
        assert_eq!(result, "val = \"fallback\"");
        unsafe { remove_env("CRAP_TEST_EMPTY") };
    }

    #[test]
    fn env_subst_missing_no_default_errors() {
        let _guard = env_lock();

        unsafe { remove_env("CRAP_TEST_NOEXIST_XYZ") };
        let result = substitute_env_vars("secret = \"${CRAP_TEST_NOEXIST_XYZ}\"");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("CRAP_TEST_NOEXIST_XYZ"));
    }

    #[test]
    fn env_subst_multiple() {
        let _guard = env_lock();

        unsafe { set_env("CRAP_TEST_A", "hello") };
        unsafe { set_env("CRAP_TEST_B", "world") };
        let result = substitute_env_vars("${CRAP_TEST_A} ${CRAP_TEST_B}").unwrap();
        assert_eq!(result, "hello world");
        unsafe { remove_env("CRAP_TEST_A") };
        unsafe { remove_env("CRAP_TEST_B") };
    }

    /// Regression: there was no way to keep a literal `${...}` in a value —
    /// `pa${ss}word` failed with "Environment variable 'ss' is not set".
    /// A doubled `$` escapes the placeholder.
    #[test]
    fn env_subst_escaped_placeholder_stays_literal() {
        let _guard = env_lock();

        unsafe { remove_env("CRAP_TEST_ESC") };
        let result = substitute_env_vars("pass = \"pa$${CRAP_TEST_ESC}word\"").unwrap();
        assert_eq!(result, "pass = \"pa${CRAP_TEST_ESC}word\"");

        let result = substitute_env_vars("$${CRAP_TEST_ESC:-x} $${}").unwrap();
        assert_eq!(result, "${CRAP_TEST_ESC:-x} ${}");
    }

    /// An escaped placeholder next to a real one leaves the real one expanded.
    #[test]
    fn env_subst_escape_does_not_disable_neighbouring_placeholders() {
        let _guard = env_lock();

        unsafe { set_env("CRAP_TEST_ESC_REAL", "value") };
        let result =
            substitute_env_vars("$${CRAP_TEST_ESC_REAL} ${CRAP_TEST_ESC_REAL} $${x}").unwrap();
        assert_eq!(result, "${CRAP_TEST_ESC_REAL} value ${x}");
        unsafe { remove_env("CRAP_TEST_ESC_REAL") };
    }

    /// The escape works through the real config load, where it matters.
    #[test]
    fn env_subst_escape_in_toml_load() {
        let _guard = env_lock();

        unsafe { remove_env("CRAP_TEST_ESC_LOAD") };
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[email]\nsmtp_host = \"pa$${CRAP_TEST_ESC_LOAD}word\"\n",
        )
        .unwrap();

        let config = CrapConfig::load(tmp.path()).unwrap();

        assert_eq!(config.email.smtp_host, "pa${CRAP_TEST_ESC_LOAD}word");
    }

    #[test]
    fn env_subst_no_vars_passthrough() {
        let input = "admin_port = 3000\nhost = \"0.0.0.0\"";
        let result = substitute_env_vars(input).unwrap();
        assert_eq!(result, input);
    }

    #[test]
    fn env_subst_in_toml_load() {
        let _guard = env_lock();

        unsafe { set_env("CRAP_TEST_ADMIN_PORT", "9999") };
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[server]\nadmin_port = 9999\nhost = \"${CRAP_TEST_HOST2:-0.0.0.0}\"\n",
        )
        .unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.server.admin_port, 9999);
        assert_eq!(config.server.host, "0.0.0.0");
        unsafe { remove_env("CRAP_TEST_ADMIN_PORT") };
    }

    #[test]
    fn env_subst_ignores_comments() {
        let _guard = env_lock();

        unsafe { remove_env("CRAP_TEST_UNSET_COMMENT_VAR") };
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "# Set ${CRAP_TEST_UNSET_COMMENT_VAR} for production\n\
             [server]\nadmin_port = 3000\n",
        )
        .unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.server.admin_port, 3000);
    }

    #[test]
    fn env_subst_in_string_values_via_load() {
        let _guard = env_lock();

        unsafe { set_env("CRAP_TEST_SMTP_HOST", "mail.example.com") };
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[email]\nsmtp_host = \"${CRAP_TEST_SMTP_HOST}\"\n",
        )
        .unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.email.smtp_host, "mail.example.com");
        unsafe { remove_env("CRAP_TEST_SMTP_HOST") };
    }

    #[test]
    fn substitute_in_value_string() {
        let _guard = env_lock();

        unsafe { set_env("CRAP_TEST_SIV", "replaced") };
        let mut val = toml::Value::String("${CRAP_TEST_SIV}".to_string());
        let mut report = ErrorReport::new();
        substitute_in_value(&mut val, "server.host", &mut report);
        assert!(report.is_empty());
        assert_eq!(val.as_str().unwrap(), "replaced");
        unsafe { remove_env("CRAP_TEST_SIV") };
    }

    #[test]
    fn substitute_in_value_table() {
        let _guard = env_lock();

        unsafe { set_env("CRAP_TEST_SIV2", "value2") };
        let mut tbl = toml::map::Map::new();
        tbl.insert(
            "key".to_string(),
            toml::Value::String("${CRAP_TEST_SIV2}".to_string()),
        );
        tbl.insert("num".to_string(), toml::Value::Integer(42));
        let mut val = toml::Value::Table(tbl);
        let mut report = ErrorReport::new();
        substitute_in_value(&mut val, "server", &mut report);
        assert!(report.is_empty());
        assert_eq!(val.get("key").unwrap().as_str().unwrap(), "value2");
        assert_eq!(val.get("num").unwrap().as_integer().unwrap(), 42);
        unsafe { remove_env("CRAP_TEST_SIV2") };
    }

    #[test]
    fn substitute_in_value_array() {
        let _guard = env_lock();

        unsafe { set_env("CRAP_TEST_SIV3", "item") };
        let mut val = toml::Value::Array(vec![
            toml::Value::String("${CRAP_TEST_SIV3}".to_string()),
            toml::Value::Boolean(true),
        ]);
        let mut report = ErrorReport::new();
        substitute_in_value(&mut val, "cors.allowed_origins", &mut report);
        assert!(report.is_empty());
        assert_eq!(val.as_array().unwrap()[0].as_str().unwrap(), "item");
        assert!(val.as_array().unwrap()[1].as_bool().unwrap());
        unsafe { remove_env("CRAP_TEST_SIV3") };
    }

    #[test]
    fn substitute_in_value_non_string_untouched() {
        let mut report = ErrorReport::new();
        let mut val = toml::Value::Integer(99);
        substitute_in_value(&mut val, "a", &mut report);
        assert_eq!(val.as_integer().unwrap(), 99);

        let mut val = toml::Value::Float(2.5);
        substitute_in_value(&mut val, "a", &mut report);
        assert!((val.as_float().unwrap() - 2.5).abs() < f64::EPSILON);

        let mut val = toml::Value::Boolean(true);
        substitute_in_value(&mut val, "a", &mut report);
        assert!(val.as_bool().unwrap());
        assert!(report.is_empty());
    }

    /// Regression: substitution stopped at the first unset variable, so a
    /// second one surfaced only after the first was set. Every one is
    /// reported, by the path of the value holding it.
    #[test]
    fn every_unset_variable_is_reported_by_path() {
        let _guard = env_lock();

        unsafe { remove_env("CRAP_TEST_UNSET_A") };
        unsafe { remove_env("CRAP_TEST_UNSET_B") };

        let mut val: toml::Value = toml::from_str(
            "url = \"${CRAP_TEST_UNSET_A}\"\nhosts = [\"ok\", \"${CRAP_TEST_UNSET_B}\"]\n",
        )
        .unwrap();
        let mut report = ErrorReport::new();
        assert!(!substitute_in_value(&mut val, "database", &mut report));
        assert!(
            val.get("url").is_none() && val.get("hosts").is_none(),
            "{val:?}"
        );

        let text = report.into_result().unwrap_err().to_string();
        assert!(text.starts_with("2 problems:"), "{text}");
        assert!(
            text.contains("database.url: Environment variable 'CRAP_TEST_UNSET_A'"),
            "{text}"
        );
        assert!(
            text.contains("database.hosts[1]: Environment variable 'CRAP_TEST_UNSET_B'"),
            "{text}"
        );
    }
}
