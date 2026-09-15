//! The one-time startup conversions.
//!
//! Each rewrites data stored before its stored form changed, once per
//! installation, gated by a versioned `_crap_meta` value. They only carry existing
//! databases forward, so each can be removed after the release it names — once
//! every installation has started a release that ran it.
//!
//! | Module | Converts | Removable after |
//! |--------|----------|-----------------|
//! | [`super::checkbox_columns`] | `Postgres` checkbox columns from `BIGINT` to `SMALLINT` | 0.1.0 |
//! | [`super::legacy_timestamps`] | `SQLite` timestamps stored with a space | 0.1.0 |
//! | [`super::canonical_text`] | email and text values to their canonical form | 0.1.0 |
//! | [`super::nested_values`] | values inside JSON-stored rows to their typed form | 0.1.0 |
//!
//! Every module listed carries the marker line
//! `**One-time conversion — removable after 0.1.0**`, and a test keeps the table
//! and the markers in step.

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    const MARKER: &str = "**One-time conversion — removable after 0.1.0**";

    /// Every migration module, as the module path the table would name it by
    /// (`canonical_text`, `collection::sync`) and the file that defines it. The
    /// walk is recursive, so a conversion parked in a submodule directory is
    /// held to the same marker/table rule as a top-level one.
    fn migration_modules() -> Vec<(String, PathBuf)> {
        let mut found = Vec::new();
        collect_modules(
            Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src/db/migrate")),
            "",
            &mut found,
        );

        found
    }

    /// Walk `dir`, naming each `.rs` file it holds `{prefix}{stem}` and
    /// descending into each subdirectory with the directory appended to
    /// `prefix`. A `mod.rs` names the directory itself.
    fn collect_modules(dir: &Path, prefix: &str, found: &mut Vec<(String, PathBuf)>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();

            if path.is_dir() {
                let name = path.file_name().unwrap().to_str().unwrap();
                collect_modules(&path, &format!("{prefix}{name}::"), found);
                continue;
            }

            let (Some(stem), Some("rs")) = (
                path.file_stem().and_then(|s| s.to_str()),
                path.extension().and_then(|e| e.to_str()),
            ) else {
                continue;
            };

            let module = if stem == "mod" {
                prefix.trim_end_matches("::").to_string()
            } else {
                format!("{prefix}{stem}")
            };

            if module.is_empty() || module == "one_time" {
                continue;
            }

            found.push((module, path));
        }
    }

    /// Every module the table lists carries the marker, and every migration
    /// module that carries it is listed.
    #[test]
    fn one_time_conversions_are_listed_and_marked() {
        let table = include_str!("one_time.rs");

        for (module, path) in migration_modules() {
            let marked = fs::read_to_string(&path).unwrap().contains(MARKER);
            let listed = table.contains(&format!("[`super::{module}`]"));

            assert_eq!(marked, listed, "{module}: marked {marked}, listed {listed}");
        }
    }

    /// Every module the table lists exists — a row naming a removed module
    /// would otherwise pass unnoticed.
    #[test]
    fn listed_one_time_conversions_exist() {
        let table = include_str!("one_time.rs");

        let listed: Vec<&str> = table
            .lines()
            .filter_map(|line| line.strip_prefix("//! | [`super::"))
            .filter_map(|rest| rest.split_once("`]").map(|(name, _)| name))
            .collect();

        assert!(!listed.is_empty(), "the table lists no module");

        let modules: Vec<String> = migration_modules()
            .into_iter()
            .map(|(module, _)| module)
            .collect();

        for name in listed {
            assert!(
                modules.iter().any(|module| module == name),
                "{name} is listed but no such migration module exists"
            );
        }
    }
}
