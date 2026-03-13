use std::{fs, sync::Mutex};

use super::*;
use crate::scaffold::init::LUA_API_TYPES;

/// Mutex to serialize tests that mutate XDG_CONFIG_HOME.
static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn test_validate_blueprint_name() {
    assert!(validate_blueprint_name("blog").is_ok());
    assert!(validate_blueprint_name("my-blog").is_ok());
    assert!(validate_blueprint_name("blog_v2").is_ok());
    assert!(validate_blueprint_name("").is_err());
    assert!(validate_blueprint_name("bad name").is_err());
    assert!(validate_blueprint_name("bad/name").is_err());
}

#[test]
fn test_copy_dir_recursive() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");

    // Build a small tree
    fs::create_dir_all(src.join("collections")).unwrap();
    fs::create_dir_all(src.join("data")).unwrap();
    fs::write(src.join("crap.toml"), "# config").unwrap();
    fs::write(src.join("collections/posts.lua"), "-- posts").unwrap();
    fs::write(src.join("data/crap.db"), "binary").unwrap();

    fs::create_dir_all(&dst).unwrap();
    copy_dir_recursive(&src, &dst, &["data"]).unwrap();

    assert!(dst.join("crap.toml").exists());
    assert!(dst.join("collections/posts.lua").exists());
    assert!(!dst.join("data").exists(), "data/ should be skipped");
}

#[test]
fn test_blueprint_save_requires_crap_toml() {
    let tmp = tempfile::tempdir().expect("tempdir");
    // Empty dir — no crap.toml
    let result = blueprint_save(tmp.path(), "test", false);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("crap.toml"));
}

#[test]
fn test_blueprint_use_not_found() {
    let result = blueprint_use("nonexistent_test_bp_12345", None);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not found"));
}

#[test]
fn test_blueprint_remove_not_found() {
    let result = blueprint_remove("nonexistent_test_bp_12345");
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not found"));
}

#[test]
fn test_count_lua_files() {
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
fn test_copy_dir_recursive_nested_dirs() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");

    // Build a deeper nested tree
    fs::create_dir_all(src.join("a/b/c")).unwrap();
    fs::write(src.join("a/b/c/deep.txt"), "deep content").unwrap();
    fs::write(src.join("a/top.txt"), "top content").unwrap();

    fs::create_dir_all(&dst).unwrap();
    copy_dir_recursive(&src, &dst, &[]).unwrap();

    assert!(dst.join("a/b/c/deep.txt").exists());
    assert_eq!(
        fs::read_to_string(dst.join("a/b/c/deep.txt")).unwrap(),
        "deep content"
    );
    assert!(dst.join("a/top.txt").exists());
    assert_eq!(
        fs::read_to_string(dst.join("a/top.txt")).unwrap(),
        "top content"
    );
}

#[test]
fn test_copy_dir_recursive_skip_multiple() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");

    fs::create_dir_all(src.join("data")).unwrap();
    fs::create_dir_all(src.join("uploads")).unwrap();
    fs::create_dir_all(src.join("types")).unwrap();
    fs::create_dir_all(src.join("collections")).unwrap();
    fs::write(src.join("crap.toml"), "# config").unwrap();
    fs::write(src.join("data/crap.db"), "db").unwrap();
    fs::write(src.join("uploads/photo.jpg"), "photo").unwrap();
    fs::write(src.join("types/crap.lua"), "types").unwrap();
    fs::write(src.join("collections/posts.lua"), "posts").unwrap();

    fs::create_dir_all(&dst).unwrap();
    copy_dir_recursive(&src, &dst, BLUEPRINT_SKIP).unwrap();

    assert!(dst.join("crap.toml").exists());
    assert!(dst.join("collections/posts.lua").exists());
    assert!(!dst.join("data").exists(), "data/ should be skipped");
    assert!(!dst.join("uploads").exists(), "uploads/ should be skipped");
    assert!(!dst.join("types").exists(), "types/ should be skipped");
}

#[test]
fn test_copy_dir_recursive_skip_only_top_level() {
    // Skip should only apply at the top level of copy_dir_recursive
    let tmp = tempfile::tempdir().expect("tempdir");
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");

    // "data" at top level should be skipped, but nested "data" should not
    fs::create_dir_all(src.join("data")).unwrap();
    fs::create_dir_all(src.join("subdir/data")).unwrap();
    fs::write(src.join("data/file.txt"), "top data").unwrap();
    fs::write(src.join("subdir/data/file.txt"), "nested data").unwrap();

    fs::create_dir_all(&dst).unwrap();
    copy_dir_recursive(&src, &dst, &["data"]).unwrap();

    assert!(
        !dst.join("data").exists(),
        "top-level data/ should be skipped"
    );
    // Nested "data" inside "subdir" should NOT be skipped because skip
    // only applies at the top level; the recursive call passes &[].
    assert!(
        dst.join("subdir/data/file.txt").exists(),
        "nested data/ should NOT be skipped"
    );
}

#[test]
fn test_count_lua_files_mixed_extensions() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("test");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("a.lua"), "").unwrap();
    fs::write(dir.join("b.lua"), "").unwrap();
    fs::write(dir.join("c.txt"), "").unwrap();
    fs::write(dir.join("d.rs"), "").unwrap();
    fs::write(dir.join("e"), "").unwrap(); // no extension
    assert_eq!(count_lua_files(&dir), 2);
}

#[test]
fn test_count_lua_files_empty_dir() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("empty");
    fs::create_dir_all(&dir).unwrap();
    assert_eq!(count_lua_files(&dir), 0);
}

#[test]
fn test_validate_blueprint_name_special_chars() {
    assert!(validate_blueprint_name("abc123").is_ok());
    assert!(validate_blueprint_name("my_blog_v2").is_ok());
    assert!(validate_blueprint_name("A-B-C").is_ok());
    assert!(validate_blueprint_name("a.b").is_err());
    assert!(validate_blueprint_name("a/b").is_err());
    assert!(validate_blueprint_name("a\\b").is_err());
    assert!(validate_blueprint_name("a b").is_err());
    assert!(validate_blueprint_name("a@b").is_err());
}

#[test]
fn test_blueprint_roundtrip() {
    // Save a blueprint and use it to create a new project
    let tmp = tempfile::tempdir().expect("tempdir");

    // Create a fake config dir
    let config = tmp.path().join("my-config");
    fs::create_dir_all(config.join("collections")).unwrap();
    fs::create_dir_all(config.join("data")).unwrap();
    fs::create_dir_all(config.join("uploads")).unwrap();
    fs::write(config.join("crap.toml"), "[server]\nadmin_port = 4000\n").unwrap();
    fs::write(config.join("init.lua"), "-- hello").unwrap();
    fs::write(config.join("collections/posts.lua"), "-- posts").unwrap();
    fs::write(config.join("data/crap.db"), "should be skipped").unwrap();
    fs::write(config.join("uploads/photo.jpg"), "should be skipped").unwrap();

    // Save as blueprint (use a custom dir to avoid polluting real config)
    // We test the internal helpers instead of the full save/use flow
    // since those depend on the global config dir
    let bp_dir = tmp.path().join("blueprints");
    fs::create_dir_all(&bp_dir).unwrap();
    let bp_target = bp_dir.join("my-blog");
    fs::create_dir_all(&bp_target).unwrap();

    copy_dir_recursive(&config, &bp_target, BLUEPRINT_SKIP).unwrap();
    write_manifest(&bp_target).unwrap();

    // Verify blueprint contents
    assert!(bp_target.join("crap.toml").exists());
    assert!(bp_target.join("init.lua").exists());
    assert!(bp_target.join("collections/posts.lua").exists());
    assert!(!bp_target.join("data").exists(), "data/ should be excluded");
    assert!(
        !bp_target.join("uploads").exists(),
        "uploads/ should be excluded"
    );

    // Verify manifest was created
    assert!(bp_target.join(MANIFEST_FILENAME).exists());
    let manifest = read_manifest(&bp_target)
        .unwrap()
        .expect("manifest should exist");
    assert_eq!(manifest.crap_version, env!("CARGO_PKG_VERSION"));
    assert!(manifest.created_at.is_some());

    // "Use" the blueprint to create a new project
    let new_project = tmp.path().join("new-project");
    fs::create_dir_all(&new_project).unwrap();
    copy_dir_recursive(&bp_target, &new_project, &[]).unwrap();

    assert!(new_project.join("crap.toml").exists());
    assert!(new_project.join("init.lua").exists());
    assert!(new_project.join("collections/posts.lua").exists());

    // Manifest is also copied (it's part of the blueprint)
    assert!(new_project.join(MANIFEST_FILENAME).exists());

    let toml = fs::read_to_string(new_project.join("crap.toml")).unwrap();
    assert!(toml.contains("admin_port = 4000"));
}

#[test]
fn test_write_and_read_manifest() {
    let tmp = tempfile::tempdir().expect("tempdir");
    write_manifest(tmp.path()).unwrap();

    let manifest = read_manifest(tmp.path())
        .unwrap()
        .expect("manifest should exist");
    assert_eq!(manifest.crap_version, env!("CARGO_PKG_VERSION"));
    assert!(manifest.created_at.is_some());

    // Verify the file is valid TOML
    let content = fs::read_to_string(tmp.path().join(MANIFEST_FILENAME)).unwrap();
    assert!(content.contains("crap_version"));
    assert!(content.contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn test_read_manifest_missing_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    // No manifest file — should return None (backward compatible)
    let result = read_manifest(tmp.path()).unwrap();
    assert!(result.is_none());
}

#[test]
fn test_check_blueprint_version_exact_match() {
    assert!(check_blueprint_version_against("0.1.0", "0.1.0").is_none());
    assert!(check_blueprint_version_against("1.2.3", "1.2.3").is_none());
}

#[test]
fn test_check_blueprint_version_prefix_match() {
    assert!(check_blueprint_version_against("0.1", "0.1.0").is_none());
    assert!(check_blueprint_version_against("0.1", "0.1.5").is_none());
    assert!(check_blueprint_version_against("1", "1.2.3").is_none());
}

#[test]
fn test_check_blueprint_version_mismatch() {
    let msg = check_blueprint_version_against("0.2.0", "0.1.0");
    assert!(msg.is_some());
    let msg = msg.unwrap();
    assert!(msg.contains("0.2.0"));
    assert!(msg.contains("0.1.0"));

    let msg = check_blueprint_version_against("1.0.0", "0.1.0");
    assert!(msg.is_some());
}

#[test]
fn test_check_blueprint_version_current() {
    // Current version should always match itself
    assert!(check_blueprint_version(env!("CARGO_PKG_VERSION")).is_none());
}

#[test]
fn test_check_blueprint_version_no_false_prefix() {
    // "0.1" should NOT match "0.10.0" — prefix must be followed by a dot
    let msg = check_blueprint_version_against("0.1", "0.10.0");
    assert!(msg.is_some(), "0.1 should not match 0.10.0");
}

#[test]
fn test_manifest_roundtrip_serialization() {
    let manifest = BlueprintManifest {
        crap_version: "1.2.3".to_string(),
        created_at: Some("2026-02-28T12:00:00+00:00".to_string()),
    };
    let serialized = toml::to_string_pretty(&manifest).unwrap();
    let deserialized: BlueprintManifest = toml::from_str(&serialized).unwrap();
    assert_eq!(deserialized.crap_version, "1.2.3");
    assert_eq!(
        deserialized.created_at.as_deref(),
        Some("2026-02-28T12:00:00+00:00")
    );
}

#[test]
fn test_blueprint_use_writes_types() {
    // Simulate a blueprint_use by copying a blueprint and verifying types/crap.lua
    let tmp = tempfile::tempdir().expect("tempdir");

    // Create a fake blueprint source (no types/ — just like a real blueprint)
    let bp_source = tmp.path().join("blueprint");
    fs::create_dir_all(bp_source.join("collections")).unwrap();
    fs::write(bp_source.join("crap.toml"), "[server]\nadmin_port = 3000\n").unwrap();
    fs::write(bp_source.join("collections/posts.lua"), "-- posts").unwrap();

    // Copy it like blueprint_use does
    let target = tmp.path().join("new-project");
    fs::create_dir_all(&target).unwrap();
    copy_dir_recursive(&bp_source, &target, &[]).unwrap();

    // Write types like blueprint_use does
    let types_dir = target.join("types");
    fs::create_dir_all(&types_dir).unwrap();
    fs::write(types_dir.join("crap.lua"), LUA_API_TYPES).unwrap();

    // Verify types/crap.lua exists and is non-empty
    let types_file = target.join("types/crap.lua");
    assert!(
        types_file.exists(),
        "types/crap.lua should exist after blueprint use"
    );
    let content = fs::read_to_string(&types_file).unwrap();
    assert!(!content.is_empty(), "types/crap.lua should be non-empty");
}

#[test]
fn test_manifest_without_created_at() {
    // created_at is optional — old manifests might not have it
    let content = "crap_version = \"0.1.0\"\n";
    let manifest: BlueprintManifest = toml::from_str(content).unwrap();
    assert_eq!(manifest.crap_version, "0.1.0");
    assert!(manifest.created_at.is_none());
}

#[test]
fn test_read_manifest_invalid_toml() {
    let tmp = tempfile::tempdir().expect("tempdir");
    // Write a file with invalid TOML content
    fs::write(tmp.path().join(MANIFEST_FILENAME), "not valid toml [[[").unwrap();
    let result = read_manifest(tmp.path());
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("Failed to parse manifest"), "got: {}", err);
}

/// Helper: run a closure with XDG_CONFIG_HOME set to a temp path,
/// then restore the original value. Serialized via ENV_LOCK.
fn with_temp_config_dir<F>(f: F)
where
    F: FnOnce(&std::path::Path),
{
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().expect("tempdir");
    let orig = std::env::var("XDG_CONFIG_HOME").ok();
    unsafe { std::env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    f(tmp.path());
    match orig {
        Some(v) => unsafe { std::env::set_var("XDG_CONFIG_HOME", v) },
        None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
    }
}

#[test]
fn test_list_blueprint_names_empty() {
    with_temp_config_dir(|_| {
        // blueprints dir doesn't exist yet
        let names = list_blueprint_names().unwrap();
        assert!(names.is_empty());
    });
}

#[test]
fn test_list_blueprint_names_with_entries() {
    with_temp_config_dir(|config_home| {
        let bp_dir = config_home.join("crap-cms").join("blueprints");
        fs::create_dir_all(bp_dir.join("alpha")).unwrap();
        fs::create_dir_all(bp_dir.join("beta")).unwrap();
        // A regular file should be ignored (only dirs are listed)
        fs::write(bp_dir.join("not-a-dir.txt"), "ignored").unwrap();

        let names = list_blueprint_names().unwrap();
        assert_eq!(names, vec!["alpha", "beta"]);
    });
}

#[test]
fn test_blueprint_list_no_blueprints_dir() {
    with_temp_config_dir(|_| {
        // blueprints dir doesn't exist — should print a "none saved" message and succeed
        let result = blueprint_list();
        assert!(result.is_ok());
    });
}

#[test]
fn test_blueprint_list_empty_blueprints_dir() {
    with_temp_config_dir(|config_home| {
        // blueprints dir exists but is empty
        let bp_dir = config_home.join("crap-cms").join("blueprints");
        fs::create_dir_all(&bp_dir).unwrap();
        let result = blueprint_list();
        assert!(result.is_ok());
    });
}

#[test]
fn test_blueprint_list_with_blueprints() {
    with_temp_config_dir(|config_home| {
        let bp_dir = config_home.join("crap-cms").join("blueprints");

        // Create two blueprints with collections/globals and manifests
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
        // No manifest (backward compat)

        let result = blueprint_list();
        assert!(result.is_ok());
    });
}

#[test]
fn test_blueprint_save_already_exists_no_force() {
    with_temp_config_dir(|config_home| {
        let tmp = tempfile::tempdir().expect("tempdir");

        // Create a valid config dir
        fs::write(tmp.path().join("crap.toml"), "").unwrap();

        // Pre-create the blueprint target so it "already exists"
        let bp_dir = config_home.join("crap-cms").join("blueprints");
        let bp_target = bp_dir.join("my-bp");
        fs::create_dir_all(&bp_target).unwrap();

        let result = blueprint_save(tmp.path(), "my-bp", false);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("already exists"), "got: {}", err);
        assert!(err.contains("--force"), "got: {}", err);
    });
}

#[test]
fn test_blueprint_save_force_overwrites() {
    with_temp_config_dir(|config_home| {
        let tmp = tempfile::tempdir().expect("tempdir");

        // Create a valid config dir
        fs::write(
            tmp.path().join("crap.toml"),
            "[server]\nadmin_port = 3000\n",
        )
        .unwrap();
        fs::write(tmp.path().join("init.lua"), "-- hello").unwrap();

        // Pre-create the blueprint target with old content
        let bp_dir = config_home.join("crap-cms").join("blueprints");
        let bp_target = bp_dir.join("overwrite-bp");
        fs::create_dir_all(&bp_target).unwrap();
        fs::write(bp_target.join("old-file.txt"), "old content").unwrap();

        // Force overwrite
        let result = blueprint_save(tmp.path(), "overwrite-bp", true);
        assert!(
            result.is_ok(),
            "blueprint_save with force failed: {:?}",
            result
        );

        // Old file should be gone, new content should be there
        assert!(
            !bp_target.join("old-file.txt").exists(),
            "old file should be removed"
        );
        assert!(
            bp_target.join("crap.toml").exists(),
            "crap.toml should be copied"
        );
        assert!(
            bp_target.join(MANIFEST_FILENAME).exists(),
            "manifest should be written"
        );
    });
}

#[test]
fn test_blueprint_save_success() {
    with_temp_config_dir(|config_home| {
        let tmp = tempfile::tempdir().expect("tempdir");

        // Create a valid config dir with various contents
        fs::create_dir_all(tmp.path().join("collections")).unwrap();
        fs::create_dir_all(tmp.path().join("data")).unwrap();
        fs::create_dir_all(tmp.path().join("uploads")).unwrap();
        fs::create_dir_all(tmp.path().join("types")).unwrap();
        fs::write(
            tmp.path().join("crap.toml"),
            "[server]\nadmin_port = 3000\n",
        )
        .unwrap();
        fs::write(tmp.path().join("collections/posts.lua"), "-- posts").unwrap();
        fs::write(tmp.path().join("data/crap.db"), "should skip").unwrap();
        fs::write(tmp.path().join("uploads/photo.jpg"), "should skip").unwrap();
        fs::write(tmp.path().join("types/crap.lua"), "should skip").unwrap();

        let result = blueprint_save(tmp.path(), "new-bp", false);
        assert!(result.is_ok(), "blueprint_save failed: {:?}", result);

        let bp_dir = config_home.join("crap-cms").join("blueprints");
        let bp_target = bp_dir.join("new-bp");
        assert!(bp_target.exists());
        assert!(bp_target.join("crap.toml").exists());
        assert!(bp_target.join("collections/posts.lua").exists());
        assert!(bp_target.join(MANIFEST_FILENAME).exists());
        // Runtime artifacts must be excluded
        assert!(!bp_target.join("data").exists());
        assert!(!bp_target.join("uploads").exists());
        assert!(!bp_target.join("types").exists());
    });
}

#[test]
fn test_blueprint_remove_success() {
    with_temp_config_dir(|config_home| {
        let bp_dir = config_home.join("crap-cms").join("blueprints");
        let bp_target = bp_dir.join("remove-me");
        fs::create_dir_all(&bp_target).unwrap();
        fs::write(bp_target.join("crap.toml"), "").unwrap();

        let result = blueprint_remove("remove-me");
        assert!(result.is_ok(), "blueprint_remove failed: {:?}", result);
        assert!(!bp_target.exists(), "blueprint directory should be removed");
    });
}

#[test]
fn test_blueprint_use_overwrite_protection() {
    with_temp_config_dir(|config_home| {
        let tmp = tempfile::tempdir().expect("tempdir");

        // Create a blueprint
        let bp_dir = config_home.join("crap-cms").join("blueprints");
        let bp_source = bp_dir.join("existing-bp");
        fs::create_dir_all(&bp_source).unwrap();
        fs::write(bp_source.join("crap.toml"), "[server]\nadmin_port = 3000\n").unwrap();

        // Create a target directory that already has a crap.toml
        let target = tmp.path().join("my-project");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("crap.toml"), "already here").unwrap();

        let result = blueprint_use("existing-bp", Some(target.clone()));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("already contains a crap.toml"), "got: {}", err);
    });
}

#[test]
fn test_blueprint_use_not_found_with_others_available() {
    with_temp_config_dir(|config_home| {
        // Create a different blueprint so "available blueprints" branch fires
        let bp_dir = config_home.join("crap-cms").join("blueprints");
        fs::create_dir_all(bp_dir.join("other-bp")).unwrap();

        let result = blueprint_use("missing-bp", None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not found"), "got: {}", err);
        assert!(
            err.contains("other-bp"),
            "should list available blueprints, got: {}",
            err
        );
    });
}

#[test]
fn test_blueprint_use_version_mismatch_warning() {
    with_temp_config_dir(|config_home| {
        let tmp = tempfile::tempdir().expect("tempdir");

        // Create a blueprint with a mismatched version in its manifest
        let bp_dir = config_home.join("crap-cms").join("blueprints");
        let bp_source = bp_dir.join("old-version-bp");
        fs::create_dir_all(&bp_source).unwrap();
        fs::write(bp_source.join("crap.toml"), "[server]\nadmin_port = 3000\n").unwrap();

        // Write a manifest with a different (fake old) version
        let old_manifest = BlueprintManifest {
            crap_version: "0.0.1-old".to_string(),
            created_at: None,
        };
        let content = toml::to_string_pretty(&old_manifest).unwrap();
        fs::write(bp_source.join(MANIFEST_FILENAME), content).unwrap();

        // Use the blueprint into a fresh target — should succeed despite version mismatch
        // (it warns but doesn't block)
        let target = tmp.path().join("new-project");
        let result = blueprint_use("old-version-bp", Some(target.clone()));
        assert!(
            result.is_ok(),
            "blueprint_use should succeed with version mismatch: {:?}",
            result
        );
        assert!(
            target.join("crap.toml").exists(),
            "project should be created"
        );
    });
}

#[test]
fn test_blueprint_use_success() {
    with_temp_config_dir(|config_home| {
        let tmp = tempfile::tempdir().expect("tempdir");

        // Create a blueprint with matching version manifest
        let bp_dir = config_home.join("crap-cms").join("blueprints");
        let bp_source = bp_dir.join("good-bp");
        fs::create_dir_all(bp_source.join("collections")).unwrap();
        fs::write(bp_source.join("crap.toml"), "[server]\nadmin_port = 3000\n").unwrap();
        fs::write(bp_source.join("collections/posts.lua"), "-- posts").unwrap();
        write_manifest(&bp_source).unwrap();

        let target = tmp.path().join("my-new-project");
        let result = blueprint_use("good-bp", Some(target.clone()));
        assert!(result.is_ok(), "blueprint_use failed: {:?}", result);

        assert!(target.join("crap.toml").exists());
        assert!(target.join("collections/posts.lua").exists());
        // types/crap.lua must be regenerated from compiled source
        assert!(target.join("types/crap.lua").exists());
        let types_content = fs::read_to_string(target.join("types/crap.lua")).unwrap();
        assert!(!types_content.is_empty());
    });
}
