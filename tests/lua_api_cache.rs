//! Integration tests for `[cache] backend = "custom"`: the
//! `crap.cache.register` handler registered by `init.lua` backs a
//! [`CustomCache`] through the hook runner's pooled VM lease.

#![allow(clippy::missing_panics_doc)]

use std::sync::Arc;

use crap_cms::{
    config::{CacheBackend, CacheConfig, CrapConfig},
    core::{Registry, cache::create_cache_with_lease},
    hooks::HookRunner,
};

/// A single-VM hook runner over a config dir whose `init.lua` is `init_lua`.
///
/// One VM (`vm_pool_size = max_vm_pool_size = 1`) keeps the Lua-table store
/// deterministic: every leased call lands on the same VM. (A real custom
/// cache must use a *shared* external store — each pool VM runs its own
/// `init.lua` — which is exactly what the docs warn about.)
fn runner_with_init_lua(init_lua: &str) -> (tempfile::TempDir, HookRunner) {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("init.lua"), init_lua).expect("write init.lua");

    let mut config = CrapConfig::test_default();
    config.hooks.vm_pool_size = 1;
    config.hooks.max_vm_pool_size = 1;

    let shared = Registry::shared();
    let registry = Registry::snapshot(&shared);

    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .expect("create hook runner");

    (tmp, runner)
}

const COUNTING_CACHE: &str = r"
local store = {}
crap.cache.register({
    get = function(key) return store[key] end,
    set = function(key, value) store[key] = value end,
    delete = function(key) store[key] = nil end,
    clear = function() store = {} end,
})
";

#[test]
fn custom_cache_roundtrips_through_the_pooled_lease() {
    let (_tmp, runner) = runner_with_init_lua(COUNTING_CACHE);

    let config = CacheConfig {
        backend: CacheBackend::Custom,
        ..Default::default()
    };
    let cache = create_cache_with_lease(&config, runner.lua_lease()).expect("create cache");
    assert_eq!(cache.kind(), "custom");

    assert!(cache.get("populate:posts:a").unwrap().is_none());
    cache.set("populate:posts:a", b"{\"id\":\"a\"}").unwrap();
    assert_eq!(
        cache.get("populate:posts:a").unwrap().unwrap(),
        b"{\"id\":\"a\"}"
    );
    assert!(cache.has("populate:posts:a").unwrap());

    cache.delete("populate:posts:a").unwrap();
    assert!(cache.get("populate:posts:a").unwrap().is_none());

    cache.set("k1", b"v1").unwrap();
    cache.set("k2", b"v2").unwrap();
    cache.clear().unwrap();
    assert!(cache.get("k1").unwrap().is_none());
    assert!(cache.get("k2").unwrap().is_none());
}

/// Regression: `backend = "custom"` without a `crap.cache.register` call
/// used to be a silent permanent no-op (a placeholder whose `get` always
/// missed). It now fails at startup with a message naming the fix.
#[test]
fn custom_cache_without_registration_fails_boot() {
    let (_tmp, runner) = runner_with_init_lua("-- no cache registration here\n");

    let config = CacheConfig {
        backend: CacheBackend::Custom,
        ..Default::default()
    };
    let err = create_cache_with_lease(&config, runner.lua_lease())
        .err()
        .expect("custom backend without registration must fail boot")
        .to_string();
    assert!(err.contains("crap.cache.register"), "{err}");
}
