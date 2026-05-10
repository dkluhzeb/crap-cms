//! Supported output languages for type generation + the `Language`
//! enum's CLI parsing / file-extension / label accessors.

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
    pub fn from_name(s: &str) -> Option<Self> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_from_str_all_variants() {
        assert_eq!(Language::from_name("lua"), Some(Language::Lua));
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
    fn language_from_str_case_insensitive() {
        assert_eq!(Language::from_name("LUA"), Some(Language::Lua));
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
}
