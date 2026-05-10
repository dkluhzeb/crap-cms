//! File-output entry points: `generate`, `generate_lang`,
//! `generate_proto_conversion`. Pick a [`Language`] backend, render
//! the registry, write to disk.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::core::Registry;

use super::{Language, go, lua, python, rust_proto, rust_types, typescript};

/// Embedded Lua API type definitions — kept in sync with the CMS binary version.
const LUA_API_TYPES: &str = include_str!("../../types/crap.lua");

/// Generate Lua type definitions (default behavior, used on server startup).
/// Writes to `<config_dir>/types/generated.lua`.
pub fn generate(config_dir: &Path, registry: &Registry) -> Result<PathBuf> {
    generate_lang(config_dir, registry, Language::Lua, None)
}

/// Generate type definitions for a specific language.
/// Writes to `<output_dir>/generated.<ext>` (defaults to `<config_dir>/types/`).
/// Also writes `crap.lua` API surface types (keeps them in sync with CMS binary version).
pub fn generate_lang(
    config_dir: &Path,
    registry: &Registry,
    lang: Language,
    output_dir: Option<&Path>,
) -> Result<PathBuf> {
    let types_dir = output_dir
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| config_dir.join("types"));
    std::fs::create_dir_all(&types_dir)?;

    // Always write the API surface types (keeps them in sync with CMS version)
    std::fs::write(types_dir.join("crap.lua"), LUA_API_TYPES)?;

    let output = render(registry, lang);
    let filename = format!("generated.{}", lang.file_extension());
    let path = types_dir.join(filename);
    std::fs::write(&path, output)?;
    Ok(path)
}

/// Generate proto conversion code for Rust (prost_types → typed structs).
/// Writes to `<output_dir>/generated_proto.rs`.
pub fn generate_proto_conversion(
    config_dir: &Path,
    registry: &Registry,
    proto_mod: &str,
    output_dir: Option<&Path>,
) -> Result<PathBuf> {
    let types_dir = output_dir
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| config_dir.join("types"));
    std::fs::create_dir_all(&types_dir)?;

    let output = rust_proto::render(registry, proto_mod);
    let path = types_dir.join("generated_proto.rs");
    std::fs::write(&path, output)?;
    Ok(path)
}

/// Render type definitions for the given language.
fn render(registry: &Registry, lang: Language) -> String {
    match lang {
        Language::Lua => lua::render(registry),
        Language::Typescript => typescript::render(registry),
        Language::Go => go::render(registry),
        Language::Python => python::render(registry),
        Language::Rust => rust_types::render(registry),
    }
}
