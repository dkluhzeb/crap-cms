//! File-output entry points for each typegen artifact. The CLI's
//! `typegen lua`, `typegen client --lang X`, and `typegen proto` map
//! one-to-one to [`generate_lua`], [`generate_client`], and
//! [`generate_proto`]. Each writes only its own files — no implicit
//! cross-artifact side effects.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::Result;

use crate::core::Registry;

use super::{Language, go, lua, python, rust_proto, rust_types, typescript};

/// Embedded Lua API surface — kept in sync with the CMS binary version.
/// Copied verbatim into the user's project on `typegen lua` (and on
/// `serve` startup under `dev_mode`).
const LUA_API_TYPES: &str = include_str!("../../types/crap.lua");

/// Server-side Lua extension types. Writes two files into
/// `<types_dir>/`:
/// - `crap.lua` — the binary's embedded Lua API surface (`crap.*`
///   namespace, hook context types, function aliases).
/// - `hooks.lua` — per-collection narrowed hook/data/doc shapes
///   rendered from the loaded registry.
///
/// # Errors
///
/// Returns an error if the types directory can't be created or either
/// output file can't be written.
pub fn generate_lua(
    config_dir: &Path,
    registry: &Registry,
    output_dir: Option<&Path>,
) -> Result<Vec<PathBuf>> {
    let types_dir = resolve_types_dir(config_dir, output_dir);
    fs::create_dir_all(&types_dir)?;

    let crap_path = types_dir.join("crap.lua");
    fs::write(&crap_path, LUA_API_TYPES)?;

    let hooks_path = types_dir.join("hooks.lua");
    fs::write(&hooks_path, lua::render(registry))?;

    Ok(vec![crap_path, hooks_path])
}

/// Consumer-side per-collection types for the given client language.
/// Writes `<types_dir>/client.<ext>`.
///
/// # Errors
///
/// Returns an error if the types directory can't be created or the
/// output file can't be written.
pub fn generate_client(
    config_dir: &Path,
    registry: &Registry,
    lang: Language,
    output_dir: Option<&Path>,
) -> Result<PathBuf> {
    let types_dir = resolve_types_dir(config_dir, output_dir);
    fs::create_dir_all(&types_dir)?;

    let path = types_dir.join(format!("client.{}", lang.file_extension()));
    fs::write(&path, render_client(registry, lang))?;
    Ok(path)
}

/// `From<proto::Document>` conversion code for Rust gRPC servers.
/// Writes `<types_dir>/proto.rs`. `proto_mod` is the Rust module path
/// to the prost-generated proto types (e.g. `"crate::proto"`).
///
/// # Errors
///
/// Returns an error if the types directory can't be created or the
/// output file can't be written.
pub fn generate_proto(
    config_dir: &Path,
    registry: &Registry,
    proto_mod: &str,
    output_dir: Option<&Path>,
) -> Result<PathBuf> {
    let types_dir = resolve_types_dir(config_dir, output_dir);
    fs::create_dir_all(&types_dir)?;

    let path = types_dir.join("proto.rs");
    fs::write(&path, rust_proto::render(registry, proto_mod))?;
    Ok(path)
}

fn resolve_types_dir(config_dir: &Path, output_dir: Option<&Path>) -> PathBuf {
    output_dir.map_or_else(|| config_dir.join("types"), Path::to_path_buf)
}

fn render_client(registry: &Registry, lang: Language) -> String {
    match lang {
        Language::Typescript => typescript::render(registry),
        Language::Go => go::render(registry),
        Language::Python => python::render(registry),
        Language::Rust => rust_types::render(registry),
    }
}
