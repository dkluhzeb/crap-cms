//! Shared helpers for blueprint operations -- filesystem, validation, paths.

#[cfg(test)]
use std::env;
use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, anyhow, bail};

use crate::cli;

#[cfg(test)]
use crate::test_support::env_lock;

/// How to save a blueprint, for the messages shown when none exists yet.
/// `blueprint save` takes the name only — the project is the resolved config
/// directory.
pub const SAVE_BLUEPRINT_HINT: &str = "Save one with: crap-cms blueprint save <name>";

/// Resolve the global blueprints directory.
///
/// - Linux: `~/.config/crap-cms/blueprints/`
/// - macOS: `~/Library/Application Support/crap-cms/blueprints/`
/// - Windows: `C:\Users\<user>\AppData\Roaming\crap-cms\blueprints\`
pub(super) fn blueprints_dir() -> Result<PathBuf> {
    let base = dirs::config_dir()
        .ok_or_else(|| anyhow!("Could not determine config directory for your platform"))?;

    Ok(base.join("crap-cms").join("blueprints"))
}

/// Recursively copy `src` into `dst`, leaving out every entry `exclude`
/// matches (it receives the entry's source path). Symlinks are neither
/// followed nor copied — a blueprint is a self-contained copy, and following a
/// directory link could recurse forever — and are returned so the caller can
/// report them.
pub(super) fn copy_dir_recursive(
    src: &Path,
    dst: &Path,
    exclude: &dyn Fn(&Path) -> bool,
) -> Result<Vec<PathBuf>> {
    let mut skipped_links = Vec::new();

    for entry in fs::read_dir(src)
        .with_context(|| format!("Failed to read directory '{}'", src.display()))?
    {
        let entry = entry?;
        let src_path = entry.path();

        if exclude(&src_path) {
            continue;
        }

        let file_type = entry.file_type()?;
        let dst_path = dst.join(entry.file_name());

        if file_type.is_symlink() {
            skipped_links.push(src_path);
        } else if file_type.is_dir() {
            fs::create_dir_all(&dst_path)?;
            skipped_links.extend(copy_dir_recursive(&src_path, &dst_path, exclude)?);
        } else {
            fs::copy(&src_path, &dst_path)
                .with_context(|| format!("Failed to copy '{}'", src_path.display()))?;
        }
    }

    Ok(skipped_links)
}

/// Tell the operator which symlinks [`copy_dir_recursive`] left out.
pub(super) fn report_skipped_links(links: &[PathBuf]) {
    for link in links {
        cli::warning(&format!(
            "Skipped symlink '{}' -- blueprints hold regular files only",
            link.display()
        ));
    }
}

/// An `exclude` for [`copy_dir_recursive`] that keeps everything.
pub(super) fn keep_all(_: &Path) -> bool {
    false
}

/// Count `.lua` files in a directory (0 if directory doesn't exist).
pub(super) fn count_lua_files(dir: &Path) -> usize {
    fs::read_dir(dir).map_or(0, |entries| {
        entries
            .filter_map(std::result::Result::ok)
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "lua"))
            .count()
    })
}

/// Validate a blueprint name: alphanumeric, hyphens, underscores.
pub(super) fn validate_blueprint_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("Blueprint name cannot be empty");
    }

    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        bail!(
            "Invalid blueprint name '{name}' -- use alphanumeric characters, hyphens, and underscores only"
        );
    }

    Ok(())
}

/// Run a closure with `XDG_CONFIG_HOME` set to a temp path, then restore the
/// original value.
///
/// The crate-wide environment lock is held for the whole call: the variable is
/// process-wide state, and a concurrent set/read from another test thread is a
/// data race.
#[cfg(test)]
pub(super) fn with_temp_config_dir<F>(f: F)
where
    F: FnOnce(&Path),
{
    let _guard = env_lock();

    let tmp = tempfile::tempdir().expect("tempdir");
    let orig = env::var("XDG_CONFIG_HOME").ok();

    // SAFETY: the environment lock is held for the whole body, so no other
    // test thread reads or writes the environment meanwhile.
    unsafe { env::set_var("XDG_CONFIG_HOME", tmp.path()) };

    f(tmp.path());

    match orig {
        // SAFETY: as above — the environment lock is still held.
        Some(v) => unsafe { env::set_var("XDG_CONFIG_HOME", v) },
        None => unsafe { env::remove_var("XDG_CONFIG_HOME") },
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    use clap::Parser as _;

    use super::*;
    use crate::commands::Cli;

    /// Regression: two of the three "save one" hints showed a
    /// `blueprint save <dir> <name>` form the command doesn't take.
    #[test]
    fn the_save_hint_is_an_invocation_the_cli_accepts() {
        let line = SAVE_BLUEPRINT_HINT
            .strip_prefix("Save one with: ")
            .expect("hint prefix");
        let args = line
            .split_whitespace()
            .map(|arg| if arg == "<name>" { "starter" } else { arg });

        let parsed = Cli::try_parse_from(args);
        assert!(parsed.is_ok(), "{line}: {:?}", parsed.err());
    }

    #[test]
    fn validate_name_valid() {
        assert!(validate_blueprint_name("blog").is_ok());
        assert!(validate_blueprint_name("my-blog").is_ok());
        assert!(validate_blueprint_name("blog_v2").is_ok());
        assert!(validate_blueprint_name("abc123").is_ok());
        assert!(validate_blueprint_name("my_blog_v2").is_ok());
        assert!(validate_blueprint_name("A-B-C").is_ok());
    }

    #[test]
    fn validate_name_invalid() {
        assert!(validate_blueprint_name("").is_err());
        assert!(validate_blueprint_name("bad name").is_err());
        assert!(validate_blueprint_name("bad/name").is_err());
        assert!(validate_blueprint_name("a.b").is_err());
        assert!(validate_blueprint_name("a\\b").is_err());
        assert!(validate_blueprint_name("a@b").is_err());
    }

    #[test]
    fn copy_dir_recursive_basic() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");

        fs::create_dir_all(src.join("collections")).unwrap();
        fs::create_dir_all(src.join("data")).unwrap();
        fs::write(src.join("crap.toml"), "# config").unwrap();
        fs::write(src.join("collections/posts.lua"), "-- posts").unwrap();
        fs::write(src.join("data/crap.db"), "binary").unwrap();

        fs::create_dir_all(&dst).unwrap();
        let skip_data = |p: &Path| p == src.join("data");
        copy_dir_recursive(&src, &dst, &skip_data).unwrap();

        assert!(dst.join("crap.toml").exists());
        assert!(dst.join("collections/posts.lua").exists());
        assert!(!dst.join("data").exists(), "data/ should be skipped");
    }

    #[test]
    fn copy_dir_recursive_nested_dirs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");

        fs::create_dir_all(src.join("a/b/c")).unwrap();
        fs::write(src.join("a/b/c/deep.txt"), "deep content").unwrap();
        fs::write(src.join("a/top.txt"), "top content").unwrap();

        fs::create_dir_all(&dst).unwrap();
        copy_dir_recursive(&src, &dst, &keep_all).unwrap();

        assert_eq!(
            fs::read_to_string(dst.join("a/b/c/deep.txt")).unwrap(),
            "deep content"
        );
        assert_eq!(
            fs::read_to_string(dst.join("a/top.txt")).unwrap(),
            "top content"
        );
    }

    /// Regression: a directory symlink was followed (`is_dir()` follows), so a
    /// link pointing at an ancestor recursed until the stack overflowed.
    #[cfg(unix)]
    #[test]
    fn copy_dir_recursive_does_not_follow_symlinks() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");

        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("sub/file.txt"), "x").unwrap();
        symlink(&src, src.join("sub/loop")).unwrap();
        symlink(src.join("sub/file.txt"), src.join("link.txt")).unwrap();

        fs::create_dir_all(&dst).unwrap();
        let skipped = copy_dir_recursive(&src, &dst, &keep_all).unwrap();

        assert!(dst.join("sub/file.txt").exists());
        assert!(!dst.join("sub/loop").exists());
        assert!(!dst.join("link.txt").exists());
        assert_eq!(skipped.len(), 2);
    }

    #[test]
    fn count_lua_files_basic() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("collections");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("posts.lua"), "").unwrap();
        fs::write(dir.join("tags.lua"), "").unwrap();
        fs::write(dir.join("readme.md"), "").unwrap();

        assert_eq!(count_lua_files(&dir), 2);
        assert_eq!(count_lua_files(&tmp.path().join("nope")), 0);
    }

    #[test]
    fn count_lua_files_mixed_extensions() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("test");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.lua"), "").unwrap();
        fs::write(dir.join("b.lua"), "").unwrap();
        fs::write(dir.join("c.txt"), "").unwrap();
        fs::write(dir.join("d.rs"), "").unwrap();
        fs::write(dir.join("e"), "").unwrap();

        assert_eq!(count_lua_files(&dir), 2);
    }

    #[test]
    fn count_lua_files_empty_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("empty");
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(count_lua_files(&dir), 0);
    }
}
