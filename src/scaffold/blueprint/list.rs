//! List saved blueprints.

use std::fs;

use anyhow::Result;

use crate::cli;

use super::helpers::{blueprints_dir, count_lua_files};
use super::manifest::read_manifest;

/// List all saved blueprints, printing a table to stdout.
pub fn blueprint_list() -> Result<()> {
    let bp_dir = blueprints_dir()?;
    let names = list_blueprint_names()?;

    if names.is_empty() {
        cli::info("No blueprints saved yet.");
        cli::hint("Save one with: crap-cms blueprint save <dir> <name>");
        return Ok(());
    }

    let mut table = cli::Table::new(vec!["Blueprint", "Collections", "Globals", "Version"]);

    for name in &names {
        let bp_path = bp_dir.join(name);
        let collections = count_lua_files(&bp_path.join("collections"));
        let globals = count_lua_files(&bp_path.join("globals"));
        let version = match read_manifest(&bp_path) {
            Ok(Some(m)) => format!("v{}", m.crap_version),
            _ => "-".to_string(),
        };

        table.row(vec![
            name,
            &collections.to_string(),
            &globals.to_string(),
            &version,
        ]);
    }

    table.print();
    cli::hint("Use with: crap-cms blueprint use <name> [dir]");

    Ok(())
}

/// List blueprint names from the global blueprints directory.
pub fn list_blueprint_names() -> Result<Vec<String>> {
    let bp_dir = blueprints_dir()?;

    if !bp_dir.exists() {
        return Ok(Vec::new());
    }

    let mut names = Vec::new();

    for entry in fs::read_dir(&bp_dir)? {
        let entry = entry?;

        if entry.path().is_dir() {
            names.push(entry.file_name().to_string_lossy().to_string());
        }
    }

    names.sort();
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scaffold::blueprint::helpers::with_temp_config_dir;
    use crate::scaffold::blueprint::manifest::write_manifest;

    #[test]
    fn list_names_empty() {
        with_temp_config_dir(|_| {
            let names = list_blueprint_names().unwrap();
            assert!(names.is_empty());
        });
    }

    #[test]
    fn list_names_with_entries() {
        with_temp_config_dir(|config_home| {
            let bp_dir = config_home.join("crap-cms").join("blueprints");
            fs::create_dir_all(bp_dir.join("alpha")).unwrap();
            fs::create_dir_all(bp_dir.join("beta")).unwrap();
            fs::write(bp_dir.join("not-a-dir.txt"), "ignored").unwrap();

            let names = list_blueprint_names().unwrap();
            assert_eq!(names, vec!["alpha", "beta"]);
        });
    }

    #[test]
    fn list_no_blueprints_dir() {
        with_temp_config_dir(|_| {
            assert!(blueprint_list().is_ok());
        });
    }

    #[test]
    fn list_empty_blueprints_dir() {
        with_temp_config_dir(|config_home| {
            let bp_dir = config_home.join("crap-cms").join("blueprints");
            fs::create_dir_all(&bp_dir).unwrap();
            assert!(blueprint_list().is_ok());
        });
    }

    #[test]
    fn list_with_blueprints() {
        with_temp_config_dir(|config_home| {
            let bp_dir = config_home.join("crap-cms").join("blueprints");

            let bp1 = bp_dir.join("blog");
            fs::create_dir_all(bp1.join("collections")).unwrap();
            fs::create_dir_all(bp1.join("globals")).unwrap();
            fs::write(bp1.join("collections/posts.lua"), "").unwrap();
            fs::write(bp1.join("globals/settings.lua"), "").unwrap();
            write_manifest(&bp1).unwrap();

            let bp2 = bp_dir.join("shop");
            fs::create_dir_all(bp2.join("collections")).unwrap();
            fs::write(bp2.join("collections/products.lua"), "").unwrap();
            fs::write(bp2.join("collections/orders.lua"), "").unwrap();

            assert!(blueprint_list().is_ok());
        });
    }
}
