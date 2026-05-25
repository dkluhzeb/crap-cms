//! Supported **client** output languages for type generation. The
//! `Language` enum exclusively names languages the `typegen client`
//! subcommand can emit per-collection types for. Lua is server-side
//! (see `dispatch::generate_lua`) and lives outside this enum.

/// Client (consumer) output languages — Lua is server-side and not
/// represented here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Typescript,
    Go,
    Python,
    Rust,
}

impl Language {
    #[must_use]
    pub fn from_name(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "ts" | "typescript" => Some(Self::Typescript),
            "go" | "golang" => Some(Self::Go),
            "py" | "python" => Some(Self::Python),
            "rs" | "rust" => Some(Self::Rust),
            _ => None,
        }
    }

    #[must_use]
    pub fn file_extension(&self) -> &'static str {
        match self {
            Self::Typescript => "ts",
            Self::Go => "go",
            Self::Python => "py",
            Self::Rust => "rs",
        }
    }

    #[must_use]
    pub fn all() -> &'static [Self] {
        &[Self::Typescript, Self::Go, Self::Python, Self::Rust]
    }

    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Typescript => "ts",
            Self::Go => "go",
            Self::Python => "py",
            Self::Rust => "rs",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_from_str_all_variants() {
        assert_eq!(Language::from_name("ts"), Some(Language::Typescript));
        assert_eq!(
            Language::from_name("typescript"),
            Some(Language::Typescript)
        );
        assert_eq!(Language::from_name("go"), Some(Language::Go));
        assert_eq!(Language::from_name("golang"), Some(Language::Go));
        assert_eq!(Language::from_name("py"), Some(Language::Python));
        assert_eq!(Language::from_name("python"), Some(Language::Python));
        assert_eq!(Language::from_name("rs"), Some(Language::Rust));
        assert_eq!(Language::from_name("rust"), Some(Language::Rust));
    }

    #[test]
    fn language_from_str_lua_rejected() {
        // Lua is server-side; `typegen lua` lives outside the client
        // languages enum and `--lang lua` must not parse.
        assert_eq!(Language::from_name("lua"), None);
    }

    #[test]
    fn language_from_str_case_insensitive() {
        assert_eq!(
            Language::from_name("TypeScript"),
            Some(Language::Typescript)
        );
    }

    #[test]
    fn language_from_str_invalid() {
        assert_eq!(Language::from_name("java"), None);
        assert_eq!(Language::from_name(""), None);
    }

    #[test]
    fn language_file_extension() {
        assert_eq!(Language::Typescript.file_extension(), "ts");
        assert_eq!(Language::Go.file_extension(), "go");
        assert_eq!(Language::Python.file_extension(), "py");
        assert_eq!(Language::Rust.file_extension(), "rs");
    }

    #[test]
    fn language_label() {
        assert_eq!(Language::Typescript.label(), "ts");
        assert_eq!(Language::Go.label(), "go");
        assert_eq!(Language::Python.label(), "py");
        assert_eq!(Language::Rust.label(), "rs");
    }

    #[test]
    fn language_all_contains_four() {
        assert_eq!(Language::all().len(), 4);
    }
}
