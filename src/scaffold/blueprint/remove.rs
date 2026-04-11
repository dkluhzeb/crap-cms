//! Remove a saved blueprint.

use std::fs;

use anyhow::{Context as _, Result, bail};

use crate::cli;

use super::helpers::{blueprints_dir, validate_blueprint_name};

/// Remove a saved blueprint by name.
pub fn blueprint_remove(name: &str) -> Result<()> {
    validate_blueprint_name(name)?;

    let target = blueprints_dir()?.join(name);

    if !target.exists() {
        bail!("Blueprint '{}' not found", name);
    }

    fs::remove_dir_all(&target)
        .with_context(|| format!("Failed to remove blueprint '{}'", name))?;

    cli::success(&format!("Removed blueprint '{}'", name));

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scaffold::blueprint::helpers::with_temp_config_dir;

    #[test]
    fn remove_not_found() {
        let result = blueprint_remove("nonexistent_test_bp_12345");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn remove_success() {
        with_temp_config_dir(|config_home| {
            let bp_dir = config_home.join("crap-cms").join("blueprints");
            let bp_target = bp_dir.join("remove-me");
            fs::create_dir_all(&bp_target).unwrap();
            fs::write(bp_target.join("crap.toml"), "").unwrap();

            let result = blueprint_remove("remove-me");
            assert!(result.is_ok(), "blueprint_remove failed: {:?}", result);
            assert!(!bp_target.exists());
        });
    }
}
