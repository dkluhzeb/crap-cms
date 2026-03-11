//! Type generation for multiple languages from the collection registry.
//!
//! - `lua` — LuaLS annotations for hook/init IDE support (internal)
//! - `typescript` — TypeScript interfaces for gRPC clients
//! - `go` — Go structs with json tags
//! - `python` — Python dataclasses
//! - `rust` — Rust structs with serde derives

mod go;
mod lua;
mod python;
mod rust_types;
mod typescript;

use anyhow::Result;
use std::path::{Path, PathBuf};

use crate::core::field::{FieldDefinition, FieldType};
use crate::core::Registry;

/// Embedded Lua API type definitions — kept in sync with the CMS binary version.
const LUA_API_TYPES: &str = include_str!("../../types/crap.lua");

/// Supported output languages for type generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Lua,
    Typescript,
    Go,
    Python,
    Rust,
}

impl Language {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "lua" => Some(Self::Lua),
            "ts" | "typescript" => Some(Self::Typescript),
            "go" | "golang" => Some(Self::Go),
            "py" | "python" => Some(Self::Python),
            "rs" | "rust" => Some(Self::Rust),
            _ => None,
        }
    }

    pub fn file_extension(&self) -> &'static str {
        match self {
            Self::Lua => "lua",
            Self::Typescript => "ts",
            Self::Go => "go",
            Self::Python => "py",
            Self::Rust => "rs",
        }
    }

    pub fn all() -> &'static [Self] {
        &[
            Self::Lua,
            Self::Typescript,
            Self::Go,
            Self::Python,
            Self::Rust,
        ]
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Lua => "lua",
            Self::Typescript => "ts",
            Self::Go => "go",
            Self::Python => "py",
            Self::Rust => "rs",
        }
    }
}

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

// ---------------------------------------------------------------------------
// Shared helpers used by multiple generators
// ---------------------------------------------------------------------------

/// Convert a slug like "site_settings" to PascalCase "SiteSettings".
pub(crate) fn to_pascal_case(slug: &str) -> String {
    slug.split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(c) => {
                    let mut s = c.to_uppercase().to_string();
                    s.push_str(&chars.collect::<String>());
                    s
                }
                None => String::new(),
            }
        })
        .collect()
}

/// Whether a field should be treated as optional in generated types.
pub(crate) fn is_optional(field: &FieldDefinition) -> bool {
    !field.required || field.field_type == FieldType::Checkbox
}

/// Whether a field's relationship config marks it as has-many.
/// Returns `false` when there is no `RelationshipConfig` (i.e. defaults to has-one).
pub(crate) fn rel_has_many(field: &FieldDefinition) -> bool {
    field.relationship.as_ref().is_some_and(|rc| rc.has_many)
}

/// Get sorted collection slugs from the registry.
pub(crate) fn sorted_collection_slugs(registry: &Registry) -> Vec<&String> {
    let mut slugs: Vec<&String> = registry.collections.keys().collect();
    slugs.sort();
    slugs
}

/// Get sorted global slugs from the registry.
pub(crate) fn sorted_global_slugs(registry: &Registry) -> Vec<&String> {
    let mut slugs: Vec<&String> = registry.globals.keys().collect();
    slugs.sort();
    slugs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::field::FieldDefinition;

    #[test]
    fn to_pascal_case_single_word() {
        assert_eq!(to_pascal_case("posts"), "Posts");
    }

    #[test]
    fn to_pascal_case_multi_word() {
        assert_eq!(to_pascal_case("site_settings"), "SiteSettings");
    }

    #[test]
    fn to_pascal_case_already_pascal() {
        assert_eq!(to_pascal_case("Posts"), "Posts");
    }

    #[test]
    fn to_pascal_case_empty() {
        assert_eq!(to_pascal_case(""), "");
    }

    #[test]
    fn to_pascal_case_three_words() {
        assert_eq!(to_pascal_case("my_cool_thing"), "MyCoolThing");
    }

    #[test]
    fn rel_has_many_with_has_many_config() {
        use crate::core::field::RelationshipConfig;
        let f = FieldDefinition::builder("", crate::core::field::FieldType::Upload)
            .relationship(RelationshipConfig::new("media", true))
            .build();
        assert!(rel_has_many(&f));
    }

    #[test]
    fn rel_has_many_with_has_one_config() {
        use crate::core::field::RelationshipConfig;
        let f = FieldDefinition::builder("", crate::core::field::FieldType::Upload)
            .relationship(RelationshipConfig::new("media", false))
            .build();
        assert!(!rel_has_many(&f));
    }

    #[test]
    fn rel_has_many_with_no_config() {
        let f = FieldDefinition::builder("", crate::core::field::FieldType::Upload).build();
        assert!(!rel_has_many(&f));
    }

    #[test]
    fn is_optional_required_field() {
        let f = FieldDefinition::builder("", FieldType::Text)
            .required(true)
            .build();
        assert!(!is_optional(&f));
    }

    #[test]
    fn is_optional_non_required_field() {
        let f = FieldDefinition::builder("", FieldType::Text)
            .required(false)
            .build();
        assert!(is_optional(&f));
    }

    #[test]
    fn is_optional_checkbox_always_optional() {
        let f = FieldDefinition::builder("", FieldType::Checkbox)
            .required(true)
            .build();
        assert!(is_optional(&f), "checkbox should always be optional");
    }

    #[test]
    fn language_from_str_all_variants() {
        assert_eq!(Language::from_str("lua"), Some(Language::Lua));
        assert_eq!(Language::from_str("ts"), Some(Language::Typescript));
        assert_eq!(Language::from_str("typescript"), Some(Language::Typescript));
        assert_eq!(Language::from_str("go"), Some(Language::Go));
        assert_eq!(Language::from_str("golang"), Some(Language::Go));
        assert_eq!(Language::from_str("py"), Some(Language::Python));
        assert_eq!(Language::from_str("python"), Some(Language::Python));
        assert_eq!(Language::from_str("rs"), Some(Language::Rust));
        assert_eq!(Language::from_str("rust"), Some(Language::Rust));
    }

    #[test]
    fn language_from_str_case_insensitive() {
        assert_eq!(Language::from_str("LUA"), Some(Language::Lua));
        assert_eq!(Language::from_str("TypeScript"), Some(Language::Typescript));
    }

    #[test]
    fn language_from_str_invalid() {
        assert_eq!(Language::from_str("java"), None);
        assert_eq!(Language::from_str(""), None);
    }

    #[test]
    fn language_file_extension() {
        assert_eq!(Language::Lua.file_extension(), "lua");
        assert_eq!(Language::Typescript.file_extension(), "ts");
        assert_eq!(Language::Go.file_extension(), "go");
        assert_eq!(Language::Python.file_extension(), "py");
        assert_eq!(Language::Rust.file_extension(), "rs");
    }

    #[test]
    fn language_label() {
        assert_eq!(Language::Lua.label(), "lua");
        assert_eq!(Language::Typescript.label(), "ts");
        assert_eq!(Language::Go.label(), "go");
        assert_eq!(Language::Python.label(), "py");
        assert_eq!(Language::Rust.label(), "rs");
    }

    #[test]
    fn language_all_contains_five() {
        assert_eq!(Language::all().len(), 5);
    }

    fn make_collection(slug: &str) -> crate::core::CollectionDefinition {
        crate::core::CollectionDefinition::new(slug)
    }

    fn make_global(slug: &str) -> crate::core::collection::GlobalDefinition {
        crate::core::collection::GlobalDefinition::new(slug)
    }

    #[test]
    fn sorted_collection_slugs_sorted() {
        let mut registry = Registry::default();
        registry
            .collections
            .insert("zebra".into(), make_collection("zebra"));
        registry
            .collections
            .insert("alpha".into(), make_collection("alpha"));
        registry
            .collections
            .insert("middle".into(), make_collection("middle"));
        let slugs = sorted_collection_slugs(&registry);
        assert_eq!(
            slugs,
            vec![
                &"alpha".to_string(),
                &"middle".to_string(),
                &"zebra".to_string()
            ]
        );
    }

    #[test]
    fn sorted_global_slugs_sorted() {
        let mut registry = Registry::default();
        registry
            .globals
            .insert("settings".into(), make_global("settings"));
        registry
            .globals
            .insert("about".into(), make_global("about"));
        let slugs = sorted_global_slugs(&registry);
        assert_eq!(slugs, vec![&"about".to_string(), &"settings".to_string()]);
    }
}
