//! Config↔docs parity: pins the hand-curated reference tables in
//! `docs/src/configuration/crap-toml.md` against the real config structs.
//!
//! The tables are *curated* — their Description column is documentation in
//! its own right — so they are checked rather than generated (the same
//! trade `tests/wire_parity.rs` makes for the proto file). Three guarantees:
//!
//! 1. every serde key of every section struct has a table row, and no row
//!    names a key the struct does not have (`#[derive(ConfigKeys)]`);
//! 2. the `## Full Reference` example parses through the real config
//!    deserializer (`deny_unknown_fields` on every section), so a phantom
//!    or retyped key in the example is a red test;
//! 3. scalar defaults shown in the tables match the actual `Default` impls.

use std::collections::BTreeSet;

use crap_cms::config::{ConfigKeys, CrapConfig};

const DOC: &str = include_str!("../docs/src/configuration/crap-toml.md");

/// Extract the key names (first table column) of the section under the
/// `[heading]` sub-heading.
fn section_table_keys(heading: &str) -> Option<BTreeSet<String>> {
    let marker = format!("### `[{heading}]`");
    let start = DOC.find(&marker)?;

    // Only the FIRST table after the heading is the section's options
    // table — later tables in the same section (e.g. `[jobs.queues]`'s
    // framework-seeded defaults) describe something else.
    let mut keys = BTreeSet::new();
    let mut in_table = false;
    for line in DOC[start + marker.len()..].lines() {
        if line.starts_with("## ") || line.starts_with("### ") {
            break;
        }

        let is_row = line.starts_with('|');
        if in_table && !is_row {
            break;
        }
        in_table |= is_row;

        let Some(rest) = line.strip_prefix("| `") else {
            continue;
        };
        let Some(end) = rest.find('`') else { continue };
        keys.insert(rest[..end].to_string());
    }

    Some(keys)
}

/// The full row of `key` in the section table under `heading`, split into
/// cells.
fn section_table_row(heading: &str, key: &str) -> Option<Vec<String>> {
    let marker = format!("### `[{heading}]`");
    let start = DOC.find(&marker)?;

    for line in DOC[start + marker.len()..].lines() {
        if line.starts_with("## ") || line.starts_with("### ") {
            break;
        }
        if line.starts_with(&format!("| `{key}` |")) {
            // Split on `|` but honor the `\|` escape markdown uses for a
            // literal pipe inside a cell.
            let sentinel = "\u{0}";
            let cells = line
                .replace("\\|", sentinel)
                .trim_matches('|')
                .split('|')
                .map(|c| c.trim().replace(sentinel, "|"))
                .collect();

            return Some(cells);
        }
    }

    None
}

/// (section heading, expected keys) — one entry per documented section.
fn section_map() -> Vec<(&'static str, Vec<&'static str>)> {
    use crap_cms::config::*;

    vec![
        ("server", ServerConfig::config_keys()),
        ("database", DatabaseConfig::config_keys()),
        ("admin", AdminConfig::config_keys()),
        ("admin.csp", CspConfig::config_keys()),
        ("auth", AuthConfig::config_keys()),
        ("auth.password_policy", PasswordPolicy::config_keys()),
        ("hooks", HooksConfig::config_keys()),
        ("depth", DepthConfig::config_keys()),
        ("upload", UploadConfig::config_keys()),
        ("upload.s3", S3Config::config_keys()),
        ("email", EmailConfig::config_keys()),
        ("live", LiveConfig::config_keys()),
        ("locale", LocaleConfig::config_keys()),
        ("jobs", JobsConfig::config_keys()),
        ("jobs.queues", QueueConfig::config_keys()),
        ("cors", CorsConfig::config_keys()),
        ("routes", RoutesConfig::config_keys()),
        ("access", AccessConfig::config_keys()),
        ("pagination", PaginationConfig::config_keys()),
        ("mcp", McpConfig::config_keys()),
        ("cache", CacheConfig::config_keys()),
        ("logging", LoggingConfig::config_keys()),
        ("update", UpdateConfig::config_keys()),
    ]
}

/// Every struct key has a row; every row names a struct key.
#[test]
fn section_tables_match_config_structs() {
    let mut drift = Vec::new();

    for (heading, expected) in section_map() {
        let expected: BTreeSet<String> = expected.into_iter().map(str::to_string).collect();
        let Some(actual) = section_table_keys(heading) else {
            drift.push(format!(
                "`[{heading}]`: no `### `[{heading}]`` section at all"
            ));
            continue;
        };

        let missing: Vec<_> = expected.difference(&actual).collect();
        let phantom: Vec<_> = actual.difference(&expected).collect();
        if !missing.is_empty() || !phantom.is_empty() {
            drift.push(format!(
                "`[{heading}]`: undocumented keys {missing:?}, rows without a struct field {phantom:?}"
            ));
        }
    }

    assert!(
        drift.is_empty(),
        "crap-toml.md section tables drifted from the config structs:\n{}",
        drift.join("\n")
    );
}

/// Every top-level config section is documented, and the map above names
/// every one — a new section can't be added without deciding its doc story.
#[test]
fn every_top_level_section_is_documented() {
    let mapped: BTreeSet<&str> = section_map().into_iter().map(|(h, _)| h).collect();

    for key in CrapConfig::config_keys() {
        // Not a section: documented under "Top-Level Fields".
        if key == "crap_version" {
            assert!(
                DOC.contains("`crap_version`"),
                "crap_version missing from Top-Level Fields"
            );
            continue;
        }

        assert!(
            mapped.contains(key),
            "config section `[{key}]` is not covered by the parity map (and likely \
             not documented) — add a `### `[{key}]`` section to crap-toml.md and a \
             map entry here"
        );
    }
}

/// The `## Full Reference` example must parse through the real config
/// deserializer — `deny_unknown_fields` turns any phantom key into a
/// failure here.
#[test]
fn full_reference_example_parses_as_a_real_config() {
    let start = DOC
        .find("## Full Reference")
        .expect("crap-toml.md has a Full Reference section");
    let block_start = DOC[start..]
        .find("```toml\n")
        .map(|i| start + i + "```toml\n".len())
        .expect("Full Reference has a toml block");
    let block_end = block_start + DOC[block_start..].find("\n```").expect("toml block closed");

    let toml_src = &DOC[block_start..block_end];

    let parsed: Result<CrapConfig, _> = toml::from_str(toml_src);
    assert!(
        parsed.is_ok(),
        "the Full Reference example in crap-toml.md does not parse as a real \
         config: {}",
        parsed.err().map(|e| e.to_string()).unwrap_or_default()
    );
}

/// Scalar defaults in the tables match the code's `Default` impls: the
/// serialized default value must appear in the row's Default cell.
///
/// Host-dependent defaults (CPU-count pool sizes) and non-scalar values are
/// exempt; the cell text may add human formatting (`` `60` (`"1m"`) ``) as
/// long as the raw value is present.
#[test]
fn table_defaults_match_code_defaults() {
    let defaults = toml::Value::try_from(CrapConfig::default()).expect("config serializes");
    let mut drift = Vec::new();

    // key → host-dependent or intentionally prose-described defaults.
    let exempt: &[(&str, &str)] = &[("hooks", "vm_pool_size"), ("hooks", "max_vm_pool_size")];

    for (heading, keys) in section_map() {
        // Sub-sections live inside their parent's serialized table.
        let mut node = &defaults;
        for part in heading.split('.') {
            match node.get(part) {
                Some(v) => node = v,
                None => return, // absent from serialized defaults — nothing to pin
            }
        }
        let Some(section) = node.as_table() else {
            continue;
        };

        for key in keys {
            if exempt.contains(&(heading, key)) {
                continue;
            }
            let Some(value) = section.get(key) else {
                continue; // e.g. Option::None — no serialized default
            };

            let rendered = match value {
                // Secret types serialize redacted — the doc shows the real
                // default, the serialization deliberately does not.
                toml::Value::String(s) if s.contains("REDACTED") => continue,
                toml::Value::String(s) if s.is_empty() => "\"\"".to_string(),
                toml::Value::String(s) => format!("\"{s}\""),
                toml::Value::Integer(n) => n.to_string(),
                toml::Value::Float(f) => f.to_string(),
                toml::Value::Boolean(b) => b.to_string(),
                toml::Value::Array(a) if a.is_empty() => "[]".to_string(),
                // Non-empty arrays / tables / datetimes: formatting varies
                // too much for a substring pin.
                _ => continue,
            };

            let Some(row) = section_table_row(heading, key) else {
                continue; // the key-parity test reports missing rows
            };
            let default_cell = row.get(2).cloned().unwrap_or_default();

            // "(required)" is a deliberate doc idiom for keys whose empty
            // default is never usable — the cell documents the obligation,
            // not the placeholder value.
            if default_cell.contains("required") {
                continue;
            }

            if !default_cell.contains(&rendered) {
                drift.push(format!(
                    "`[{heading}]` row `{key}`: Default cell {default_cell:?} does not \
                     contain the code default `{rendered}`"
                ));
            }
        }
    }

    assert!(
        drift.is_empty(),
        "crap-toml.md Default cells drifted from the code's Default impls:\n{}",
        drift.join("\n")
    );
}

/// The `crap-cms init` scaffold template mentions every config key.
///
/// The template (`src/scaffold/init/templates/crap.toml.hbs`) is the
/// operator's first contact with the config surface — most keys appear
/// as commented examples. This pins it to the same `ConfigKeys`
/// inventory the doc tables are pinned to, so a new config key can't
/// ship without the scaffold learning about it. A key counts as present
/// when it appears as `key =` (active, commented, or inline-table form)
/// or — for table-typed keys like `csp`/`s3`/`password_policy` — as a
/// `[section.key]` heading.
#[test]
fn init_template_mentions_every_config_key() {
    const TEMPLATE: &str = include_str!("../src/scaffold/init/templates/crap.toml.hbs");

    let mut missing = Vec::new();

    for (heading, keys) in section_map() {
        // Section heading present, active or commented.
        let heading_active = format!("[{heading}]");
        if !TEMPLATE.contains(&heading_active) {
            missing.push(format!("section heading `[{heading}]`"));
        }

        for key in keys {
            let assignment = format!("{key} =");
            let sub_table = format!("[{heading}.{key}]");
            if !TEMPLATE.contains(&assignment) && !TEMPLATE.contains(&sub_table) {
                missing.push(format!("`[{heading}]` key `{key}`"));
            }
        }
    }

    assert!(
        missing.is_empty(),
        "crap.toml.hbs scaffold template is missing config keys — add them \
         (commented is fine) so `crap-cms init` output stays current:\n{}",
        missing.join("\n")
    );
}
