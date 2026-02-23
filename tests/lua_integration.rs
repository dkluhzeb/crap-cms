use std::path::PathBuf;

#[test]
fn init_lua_loads_example_config() {
    let config_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("example");
    let config = crap_cms::config::CrapConfig::default();
    let registry = crap_cms::hooks::init_lua(&config_dir, &config)
        .expect("Failed to initialize Lua VM with example config");

    let reg = registry.read().unwrap();

    // Example config defines "posts" and "pages" collections
    assert!(
        reg.get_collection("posts").is_some(),
        "posts collection not found"
    );
    assert!(
        reg.get_collection("pages").is_some(),
        "pages collection not found"
    );

    // Check posts has expected fields
    let posts = reg.get_collection("posts").unwrap();
    assert_eq!(posts.display_name(), "Posts");
    assert_eq!(posts.singular_name(), "Post");
    assert_eq!(posts.title_field(), Some("title"));
    assert!(posts.fields.iter().any(|f| f.name == "title"));
    assert!(posts.fields.iter().any(|f| f.name == "slug"));
    assert!(posts.fields.iter().any(|f| f.name == "status"));
    assert!(posts.fields.iter().any(|f| f.name == "content"));

    // Check pages collection
    let pages = reg.get_collection("pages").unwrap();
    assert_eq!(pages.display_name(), "Pages");
    assert!(pages.fields.iter().any(|f| f.name == "title"));
    assert!(pages.fields.iter().any(|f| f.name == "published"));

    // Example config defines "site_settings" global
    assert!(
        reg.get_global("site_settings").is_some(),
        "site_settings global not found"
    );
    let settings = reg.get_global("site_settings").unwrap();
    assert!(settings.fields.iter().any(|f| f.name == "site_name"));
}
