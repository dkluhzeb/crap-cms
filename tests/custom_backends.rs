//! Concept / functional confirmation that custom Lua-backed storage and
//! email providers work end-to-end through the **real** `HookRunner` VM pool.
//!
//! These exercise the production path — `[upload] storage = "custom"` /
//! `[email] provider = "custom"` with a handler registered in `init.lua`,
//! wired via `create_*_with_lease(hook_runner.lua_lease())` (the pooled
//! lease, as used by the admin/gRPC/scheduler servers) — rather than a
//! hand-built Lua state. They prove the whole chain is sound: every pool
//! VM runs `init.lua`, the registered handler lands as `crap._storage` /
//! `crap._email_send`, the pool lease checks out a VM that has it, and the
//! provider delegates correctly.
//!
//! `vm_pool_size = 1` is used so a handler backed by an in-VM Lua table is
//! consistent across `put`/`get` within the test. Real custom backends are
//! stateless and delegate to an external store via `crap.http`; see the
//! Uploads docs.

#![allow(clippy::missing_panics_doc)]

use std::fs;
use std::path::Path;

use crap_cms::config::{CrapConfig, EmailProvider, UploadStorage};
use crap_cms::core::email::create_email_provider_with_lease;
use crap_cms::core::upload::{StorageNotFound, create_storage_with_lease};
use crap_cms::hooks;
use crap_cms::hooks::lifecycle::HookRunner;

/// Temp config dir containing the given `init.lua`.
fn config_dir_with(init_lua: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    fs::write(tmp.path().join("init.lua"), init_lua).expect("write init.lua");
    tmp
}

/// Build a real `HookRunner` (whose pool VMs each run `init.lua`) from a
/// config dir — the same construction the servers use.
fn build_runner(config_dir: &Path, config: &CrapConfig) -> HookRunner {
    let registry = hooks::init_lua(config_dir, config).expect("init_lua");
    HookRunner::builder()
        .config_dir(config_dir)
        .registry(registry)
        .config(config)
        .build()
        .expect("build hook runner")
}

fn custom_storage_config() -> CrapConfig {
    let mut config = CrapConfig::test_default();
    config.upload.storage = UploadStorage::Custom;
    config.hooks.vm_pool_size = 1;
    config
}

fn custom_email_config() -> CrapConfig {
    let mut config = CrapConfig::test_default();
    config.email.provider = EmailProvider::Custom;
    config.hooks.vm_pool_size = 1;
    config
}

#[test]
fn custom_storage_round_trips_through_the_pool() {
    let tmp = config_dir_with(
        r"
        local files = {}
        crap.storage.register({
            put = function(key, data, content_type) files[key] = data end,
            get = function(key) return files[key] end,
            delete = function(key) files[key] = nil end,
        })
        ",
    );
    let config = custom_storage_config();
    let runner = build_runner(tmp.path(), &config);

    let storage = create_storage_with_lease(tmp.path(), &config.upload, runner.lua_lease())
        .expect("custom storage");

    assert_eq!(
        storage.kind(),
        "custom",
        "config 'custom' must resolve to the Lua-backed backend"
    );

    storage
        .put("media/a.txt", b"hello world", "text/plain")
        .expect("put");
    assert_eq!(storage.get("media/a.txt").expect("get"), b"hello world");
    assert!(storage.exists("media/a.txt").expect("exists"));

    // Missing key → typed StorageNotFound (so the serve handler answers 404,
    // not a transient 503).
    let err = storage
        .get("media/missing.txt")
        .expect_err("missing must error");
    assert!(
        err.downcast_ref::<StorageNotFound>().is_some(),
        "missing key must be StorageNotFound, got: {err:#}"
    );

    storage.delete("media/a.txt").expect("delete");
    assert!(!storage.exists("media/a.txt").expect("exists after delete"));
}

#[test]
fn custom_storage_binary_round_trips_through_the_pool() {
    let tmp = config_dir_with(
        r"
        local files = {}
        crap.storage.register({
            put = function(key, data, content_type) files[key] = data end,
            get = function(key) return files[key] end,
            delete = function(key) files[key] = nil end,
        })
        ",
    );
    let config = custom_storage_config();
    let runner = build_runner(tmp.path(), &config);
    let storage = create_storage_with_lease(tmp.path(), &config.upload, runner.lua_lease())
        .expect("custom storage");

    // Full 0..=255 byte range proves binary passes through Lua strings intact.
    let bytes: Vec<u8> = (0..=255).collect();
    storage
        .put("media/blob.bin", &bytes, "application/octet-stream")
        .expect("put");
    assert_eq!(storage.get("media/blob.bin").expect("get"), bytes);
}

#[test]
fn custom_email_invokes_handler_with_opts_through_the_pool() {
    // The handler raises, echoing the opts back, so the error proves both
    // that the custom provider (not the log placeholder) was used AND that
    // the to/subject/html opts were plumbed through correctly.
    let tmp = config_dir_with(
        r#"
        crap.email.register({
            send = function(opts)
                error("SENT:" .. opts.to .. "|" .. opts.subject .. "|" .. opts.html)
            end,
        })
        "#,
    );
    let config = custom_email_config();
    let runner = build_runner(tmp.path(), &config);

    let provider = create_email_provider_with_lease(&config.email, runner.lua_lease())
        .expect("custom email provider");

    assert_eq!(provider.kind(), "custom");

    let err = provider
        .send("u@example.com", "Welcome", "<p>hi</p>", None)
        .expect_err("handler raises");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("SENT:u@example.com|Welcome|<p>hi</p>"),
        "handler must receive the opts; got: {msg}"
    );
}

#[test]
fn custom_email_success_path_through_the_pool() {
    // A handler that validates opts then returns normally → send() is Ok,
    // proving the success path (incl. the optional text body) end-to-end.
    let tmp = config_dir_with(
        r#"
        crap.email.register({
            send = function(opts)
                assert(opts.to == "ok@example.com", "wrong to")
                assert(opts.subject == "Hi", "wrong subject")
                assert(opts.text == "plain body", "wrong text")
            end,
        })
        "#,
    );
    let config = custom_email_config();
    let runner = build_runner(tmp.path(), &config);
    let provider = create_email_provider_with_lease(&config.email, runner.lua_lease())
        .expect("custom email provider");

    provider
        .send("ok@example.com", "Hi", "<p>hi</p>", Some("plain body"))
        .expect("send should succeed when the handler returns normally");
}

#[test]
fn custom_provider_without_registration_fails_loudly() {
    // `storage = "custom"` but init.lua never registers a handler: the first
    // operation must surface a clear error, not silently fall back to local.
    let tmp = config_dir_with("-- no crap.storage.register here\n");
    let config = custom_storage_config();
    let runner = build_runner(tmp.path(), &config);
    let storage = create_storage_with_lease(tmp.path(), &config.upload, runner.lua_lease())
        .expect("custom storage");

    let err = storage
        .put("media/a.txt", b"x", "text/plain")
        .expect_err("unregistered custom backend must error");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("crap._storage") || msg.contains("crap.storage.register"),
        "error should point at the missing registration; got: {msg}"
    );
}
