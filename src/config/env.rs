//! Environment variable substitution in TOML config values.

use anyhow::{Context as _, Result};
use regex::Regex;

/// Recursively walk a TOML `Value` tree and substitute `${VAR}` / `${VAR:-default}`
/// in all `String` nodes. Tables and arrays are descended into; other types are untouched.
pub(super) fn substitute_in_value(value: &mut toml::Value) -> Result<()> {
    match value {
        toml::Value::String(s) => {
            *s = substitute_env_vars(s)?;
        }
        toml::Value::Array(arr) => {
            for item in arr.iter_mut() {
                substitute_in_value(item)?;
            }
        }
        toml::Value::Table(tbl) => {
            for (_key, val) in tbl.iter_mut() {
                substitute_in_value(val)?;
            }
        }
        _ => {} // Integer, Float, Boolean, Datetime — no substitution
    }
    Ok(())
}

/// Replace `${VAR}` and `${VAR:-default}` placeholders with environment variable values.
///
/// - `${VAR}` — replaced with the value of `VAR`. Returns an error if `VAR` is unset.
/// - `${VAR:-fallback}` — replaced with `VAR` if set and non-empty, otherwise `fallback`.
pub(super) fn substitute_env_vars(input: &str) -> Result<String> {
    let re = Regex::new(r"\$\{([^}]+)\}").expect("env var regex");
    let mut result = String::with_capacity(input.len());
    let mut last_end = 0;

    for cap in re.captures_iter(input) {
        let full_match = cap.get(0).expect("regex group 0 always exists");
        result.push_str(&input[last_end..full_match.start()]);

        let inner = &cap[1];
        if let Some((var_name, default_val)) = inner.split_once(":-") {
            match std::env::var(var_name) {
                Ok(val) if !val.is_empty() => result.push_str(&val),
                _ => result.push_str(default_val),
            }
        } else {
            let val = std::env::var(inner).with_context(|| {
                format!(
                    "Environment variable '{}' referenced in crap.toml is not set \
                     (use ${{{}:-default}} for a fallback)",
                    inner, inner
                )
            })?;
            result.push_str(&val);
        }

        last_end = full_match.end();
    }

    result.push_str(&input[last_end..]);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        )
        .unwrap();
        let config = crate::config::CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.server.admin_port, 9999);
        assert_eq!(config.server.host, "0.0.0.0");
        std::env::remove_var("CRAP_TEST_ADMIN_PORT");
    }

    #[test]
    fn env_subst_ignores_comments() {
        std::env::remove_var("CRAP_TEST_UNSET_COMMENT_VAR");
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "# Set ${CRAP_TEST_UNSET_COMMENT_VAR} for production\n\
             [server]\nadmin_port = 3000\n",
        )
        .unwrap();
        let config = crate::config::CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.server.admin_port, 3000);
    }

    #[test]
    fn env_subst_in_string_values_via_load() {
        std::env::set_var("CRAP_TEST_SMTP_HOST", "mail.example.com");
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[email]\nsmtp_host = \"${CRAP_TEST_SMTP_HOST}\"\n",
        )
        .unwrap();
        let config = crate::config::CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.email.smtp_host, "mail.example.com");
        std::env::remove_var("CRAP_TEST_SMTP_HOST");
    }

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
        tbl.insert(
            "key".to_string(),
            toml::Value::String("${CRAP_TEST_SIV2}".to_string()),
        );
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
}
