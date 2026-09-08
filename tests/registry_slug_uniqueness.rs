//! A slug names either a collection or a global, never both: sharing one
//! across kinds makes the MCP surface (which keys exposure and gating by slug
//! alone) treat the global as the collection. Re-defining the same kind stays
//! legal — that is the plugin extension pattern.

use std::fs;

use crap_cms::config::CrapConfig;
use crap_cms::hooks;

fn init_with(files: &[(&str, &str)]) -> Result<(), String> {
    let tmp = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(tmp.path().join("collections")).unwrap();
    fs::create_dir_all(tmp.path().join("globals")).unwrap();
    for (name, body) in files {
        fs::write(tmp.path().join(name), body).unwrap();
    }

    hooks::init_lua(tmp.path(), &CrapConfig::test_default())
        .map(|_| ())
        .map_err(|e| format!("{e:#}"))
}

/// Re-defining the same collection is how a plugin extends it (read the
/// definition, append fields, define again) — that must keep working.
#[test]
fn redefining_a_collection_extends_it() {
    init_with(&[
        (
            "collections/a.lua",
            "crap.collections.define(\"posts\", { fields = { { name = \"title\", type = \"text\" } } })",
        ),
        (
            "collections/b.lua",
            "crap.collections.define(\"posts\", { fields = { { name = \"title\", type = \"text\" }, { name = \"body\", type = \"text\" } } })",
        ),
    ])
    .expect("a plugin-style redefinition is legal");
}

#[test]
fn a_global_sharing_a_collection_slug_fails_init() {
    let err = init_with(&[
        (
            "collections/settings.lua",
            "crap.collections.define(\"settings\", { fields = { { name = \"title\", type = \"text\" } } })",
        ),
        (
            "globals/settings.lua",
            "crap.globals.define(\"settings\", { fields = { { name = \"tagline\", type = \"text\" } } })",
        ),
    ])
    .expect_err("the shared slug must fail init");
    assert!(err.contains("already defined"), "{err}");
}

#[test]
fn distinct_slugs_init_fine() {
    init_with(&[
        (
            "collections/posts.lua",
            "crap.collections.define(\"posts\", { fields = { { name = \"title\", type = \"text\" } } })",
        ),
        (
            "globals/site.lua",
            "crap.globals.define(\"site\", { fields = { { name = \"tagline\", type = \"text\" } } })",
        ),
    ])
    .expect("distinct slugs are fine");
}
