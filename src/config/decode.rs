//! Decoding a parsed `crap.toml` into a [`CrapConfig`] one top-level section
//! at a time, so every problem is reported rather than the first.
//!
//! Each section is decoded on its own (with the same strict,
//! `deny_unknown_fields` types) after its own `${VAR}` substitution, and every
//! unset variable and every unknown or mistyped key in it is recorded by its
//! path (see [`decode_section`]). A section with a problem keeps its default;
//! the rest still decode.

use serde::de::DeserializeOwned;
use toml::{Table, Value};

use crate::config::{
    ConfigKeys, CrapConfig, ErrorReport, env::substitute_in_value, section_decode::decode_section,
};

/// Decode `section` (the value of top-level key `key`) into `slot`, after
/// substituting its environment variables. Every problem is recorded, and
/// `slot` is left alone when there is one.
fn decode_into<T: DeserializeOwned>(
    slot: &mut T,
    key: &str,
    mut section: Value,
    report: &mut ErrorReport,
) {
    let mut problems = ErrorReport::new();

    // What failed to expand is taken out, so the decode below reports the
    // section's other problems and not a second one for it.
    if substitute_in_value(&mut section, key, &mut problems) || section.is_table() {
        let decoded = decode_section(key, section, &mut problems);

        if problems.is_empty()
            && let Some(value) = decoded
        {
            *slot = value;
        }
    }

    report.merge(problems);
}

/// Route each top-level key to the field of the same name; an unknown key is
/// a problem of its own.
macro_rules! decode_sections {
    ($config:ident, $key:ident, $section:ident, $report:ident; $($field:ident),+ $(,)?) => {
        match $key.as_str() {
            $(stringify!($field) => decode_into(&mut $config.$field, &$key, $section, $report),)+
            other => $report.push_message(format!(
                "unknown top-level key `{other}` (expected one of: {})",
                CrapConfig::config_keys().join(", ")
            )),
        }
    };
}

/// Decode a parsed `crap.toml` section by section. Every section that fails —
/// an unknown or mistyped key, an unset environment variable — is recorded in
/// `report` and left at its default; the rest are decoded.
pub(super) fn decode_config(table: Table, report: &mut ErrorReport) -> CrapConfig {
    let mut config = CrapConfig::default();

    for (key, section) in table {
        decode_sections!(config, key, section, report;
            crap_version, server, database, admin, hooks, auth, depth, upload, email,
            live, locale, jobs, cors, routes, access, pagination, query, mcp, cache,
            logging, update,
        );
    }

    config
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(toml: &str) -> (CrapConfig, ErrorReport) {
        let mut report = ErrorReport::new();
        let config = decode_config(toml::from_str(toml).unwrap(), &mut report);

        (config, report)
    }

    /// Every key of the config struct is routed to its section: a document
    /// naming each one decodes without a problem.
    #[test]
    fn every_config_key_is_decoded() {
        let mut table = Table::new();

        for key in CrapConfig::config_keys() {
            let section = if key == "crap_version" {
                Value::String("0.1".into())
            } else {
                Value::Table(Table::new())
            };

            table.insert(key.to_string(), section);
        }

        let mut report = ErrorReport::new();
        let config = decode_config(table, &mut report);

        assert!(report.is_empty(), "{:?}", report.into_result());
        assert_eq!(config.crap_version.as_deref(), Some("0.1"));
    }

    /// Regression: within one section, decoding stopped at the first problem.
    /// Two problems in the same section — a mistyped value and an unknown
    /// key — are both reported.
    #[test]
    fn every_problem_of_one_section_is_reported() {
        let (config, report) = decode("[server]\nadmin_port = \"not a port\"\ngrpc_prot = 50051\n");

        assert_eq!(
            config.server.admin_port,
            CrapConfig::default().server.admin_port
        );

        let text = report.into_result().unwrap_err().to_string();
        assert!(text.starts_with("2 problems:"), "{text}");
        assert!(text.contains("server.admin_port: invalid type"), "{text}");
        assert!(text.contains("server.grpc_prot: unknown field"), "{text}");
    }

    /// Problems in independent sections are all reported, and a section that
    /// decodes keeps its value.
    #[test]
    fn every_failing_section_is_reported() {
        let (config, report) = decode(
            "unknown_section = 1\n\
             [server]\nadmin_port = \"not a port\"\n\
             [database]\nno_such_key = true\n\
             [pagination]\ndefault_limit = 7\n",
        );

        assert_eq!(config.pagination.default_limit, 7);

        let text = report.into_result().unwrap_err().to_string();
        assert!(text.starts_with("3 problems:"), "{text}");
        assert!(
            text.contains("unknown top-level key `unknown_section`"),
            "{text}"
        );
        assert!(text.contains("server.admin_port: invalid type"), "{text}");
        assert!(
            text.contains("database.no_such_key: unknown field"),
            "{text}"
        );
    }
}
