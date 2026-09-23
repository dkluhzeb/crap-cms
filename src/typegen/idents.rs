//! Per-language identifier sanitization for the client-SDK and Lua generators.
//!
//! Field and collection/global names are validated only as `[A-Za-z0-9_]+`
//! (non-empty) — so they may be a target-language keyword, start with a digit,
//! or (after `PascalCase`ing) collide. Emitting them verbatim produces
//! non-compiling or shadowing client code. Every generator routes its emitted
//! identifiers through the function here for its language, which yields a valid,
//! idiomatic identifier while preserving the ORIGINAL name as the wire key
//! (serde rename / struct tag / quoted key), so the data contract is unchanged.
//!
//! Leading-digit names are prefixed with `n` (lowercase-field languages) or `N`
//! (type names and Go's exported fields, which must start uppercase).

/// Whether `name`'s first character is an ASCII digit (an invalid identifier
/// start in every target language).
fn starts_with_digit(name: &str) -> bool {
    name.chars().next().is_some_and(|c| c.is_ascii_digit())
}

// ─────────────────────────── Rust ───────────────────────────

/// Rust keywords that are valid as raw identifiers (`r#kw`). A raw identifier
/// keeps `kw` as its name (so serde needs no rename).
const RUST_RAW_KEYWORDS: &[&str] = &[
    "as", "break", "const", "continue", "else", "enum", "extern", "false", "fn", "for", "if",
    "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "static",
    "struct", "trait", "true", "type", "unsafe", "use", "where", "while", "async", "await", "dyn",
    "abstract", "become", "box", "do", "final", "macro", "override", "priv", "typeof", "unsized",
    "virtual", "yield", "try", "union", "gen",
];

/// Keywords that CANNOT be raw identifiers — they must be renamed with a
/// trailing underscore instead.
const RUST_RESERVED_NONRAW: &[&str] = &["self", "Self", "super", "crate"];

/// A sanitized Rust struct-field identifier and, when the identifier no longer
/// matches the wire key, the original name for `#[serde(rename = "...")]`.
pub(crate) struct RustField {
    pub ident: String,
    pub rename: Option<String>,
}

/// Sanitize a schema field name into a valid Rust struct field identifier.
pub(crate) fn rust_field(name: &str) -> RustField {
    if starts_with_digit(name) {
        RustField {
            ident: format!("n{name}"),
            rename: Some(name.to_string()),
        }
    } else if RUST_RESERVED_NONRAW.contains(&name) {
        RustField {
            ident: format!("{name}_"),
            rename: Some(name.to_string()),
        }
    } else if RUST_RAW_KEYWORDS.contains(&name) {
        // `r#type` keeps the name "type" — serde needs no rename.
        RustField {
            ident: format!("r#{name}"),
            rename: None,
        }
    } else {
        RustField {
            ident: name.to_string(),
            rename: None,
        }
    }
}

/// Sanitize a `PascalCase` base into a valid Rust type name. Fixes a leading
/// digit, and `Self` — Rust's one reserved word that survives `PascalCase`ing
/// (every other keyword is lowercase, so `PascalCase` dodges it). A type or enum
/// variant named `Self` is rejected by the compiler; `Self_` is not.
pub(crate) fn rust_type(pascal: &str) -> String {
    if starts_with_digit(pascal) {
        format!("N{pascal}")
    } else if pascal == "Self" {
        "Self_".to_string()
    } else {
        pascal.to_string()
    }
}

// ─────────────────────────── Go ───────────────────────────

/// Sanitize a `PascalCase` base into a valid EXPORTED Go identifier (type or
/// field). Go exported names must start with an uppercase letter, so a
/// leading-digit base is prefixed with `N`. Go keywords are lowercase, so the
/// `PascalCase` form never collides with one.
pub(crate) fn go_exported(pascal: &str) -> String {
    if starts_with_digit(pascal) {
        format!("N{pascal}")
    } else {
        pascal.to_string()
    }
}

/// De-duplicate an identifier against the names already used in the same scope —
/// e.g. two schema field names (`first_name` / `firstName`) that `PascalCase` to
/// the same Go identifier, or two option values that reduce to the same enum
/// variant. Appends `_2`, `_3`, … until unique. `seen` is updated with the
/// returned name.
pub(crate) fn dedup(name: String, seen: &mut std::collections::HashSet<String>) -> String {
    if seen.insert(name.clone()) {
        return name;
    }
    let mut n = 2u32;
    loop {
        let candidate = format!("{name}_{n}");
        if seen.insert(candidate.clone()) {
            return candidate;
        }
        n += 1;
    }
}

/// Convert an arbitrary option value into a valid `PascalCase` enum-variant /
/// const identifier: split on non-alphanumerics, uppercase each word start,
/// prefix a leading digit with `N`, and fall back to `Value` when nothing
/// alphanumeric remains. The raw value is preserved separately (serde rename /
/// const literal), so this only has to be a *valid, stable* identifier; callers
/// de-duplicate collisions via [`dedup`].
pub(crate) fn variant_ident(value: &str) -> String {
    let mut out = String::new();
    let mut boundary = true;
    for c in value.chars() {
        if c.is_ascii_alphanumeric() {
            if boundary {
                out.push(c.to_ascii_uppercase());
                boundary = false;
            } else {
                out.push(c);
            }
        } else {
            boundary = true;
        }
    }

    if out.is_empty() {
        out.push_str("Value");
    } else if out.as_bytes()[0].is_ascii_digit() {
        out.insert(0, 'N');
    }
    out
}

// ─────────────────────────── Python ───────────────────────────

/// Python keywords (and soft keywords used as reserved here). A field named one
/// of these is an invalid attribute/parameter name.
const PYTHON_KEYWORDS: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue",
    "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import",
    "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while",
    "with", "yield", "match", "case",
];

/// Sanitize a schema field name into a valid Python attribute name, returning
/// the attribute plus the original wire name when it was changed. A keyword
/// gets a trailing underscore (PEP 8 convention); a leading digit is prefixed.
pub(crate) fn python_field(name: &str) -> (String, Option<String>) {
    if starts_with_digit(name) {
        (format!("n{name}"), Some(name.to_string()))
    } else if PYTHON_KEYWORDS.contains(&name) {
        (format!("{name}_"), Some(name.to_string()))
    } else {
        (name.to_string(), None)
    }
}

/// Sanitize a `PascalCase` base into a valid Python class name (fixes a leading
/// digit; `PascalCase` avoids the lowercase Python keywords).
pub(crate) fn python_class(pascal: &str) -> String {
    if starts_with_digit(pascal) {
        format!("N{pascal}")
    } else {
        pascal.to_string()
    }
}

// ─────────────────────────── TypeScript ───────────────────────────

/// Render a schema field name as a TypeScript interface property key. TS keys
/// can be keywords bare, but a non-identifier key (a leading digit) must be a
/// quoted string literal. Field names are `[A-Za-z0-9_]+`, so quoting is needed
/// only for the leading-digit case.
pub(crate) fn ts_key(name: &str) -> String {
    if starts_with_digit(name) {
        format!("\"{name}\"")
    } else {
        name.to_string()
    }
}

/// Sanitize a `PascalCase` base into a valid TypeScript type name (fixes a
/// leading digit).
pub(crate) fn ts_type(pascal: &str) -> String {
    if starts_with_digit(pascal) {
        format!("N{pascal}")
    } else {
        pascal.to_string()
    }
}

// ─────────────────────────── Lua ───────────────────────────

/// Lua 5.4's reserved words. A key spelled as one is a syntax error in dotted
/// access (`data.end`) and must be indexed with a quoted string.
const LUA_KEYWORDS: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if", "in",
    "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
];

/// The words the `LuaLS` annotation parser reads as a field's scope when they
/// open a `---@field` line (`---@field private name type`). A field spelled as
/// one would lose its name to the scope, so its key is written quoted.
const LUALS_FIELD_SCOPES: &[&str] = &["public", "protected", "private", "package"];

/// Whether `name` is a Lua identifier usable as a bare key — `t.name` in code,
/// `---@field name` in an annotation: an ASCII letter or `_` followed by ASCII
/// letters, digits and `_`, and not a reserved word. A name starting with a
/// digit is not one: the `LuaLS` annotation tokenizer reads its leading digits
/// as an integer.
pub(crate) fn is_lua_name(name: &str) -> bool {
    let mut chars = name.chars();

    let Some(first) = chars.next() else {
        return false;
    };

    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !LUA_KEYWORDS.contains(&name)
}

/// A quoted-string index (`["key"]`) — how Lua code and `LuaLS` annotations
/// both spell a key that isn't a bare identifier.
fn lua_quoted_index(key: &str) -> String {
    format!("[\"{}\"]", escape_str(key))
}

/// Index expression for a `<parent>.<key>` access in generated per-project
/// Lua. A key that isn't a bare Lua identifier (a leading digit, a reserved
/// word) is bracket-indexed with a quoted string — dotted, it's a syntax
/// error: `crap.collections["2fa"]`, `crap.collections["end"]`.
pub(crate) fn lua_index(parent: &str, key: &str) -> String {
    if is_lua_name(key) {
        format!("{parent}.{key}")
    } else {
        format!("{parent}{}", lua_quoted_index(key))
    }
}

/// The key of a `---@field` annotation for a table key named `name`: bare when
/// it is a Lua identifier the `LuaLS` parser takes as a field name, otherwise
/// the quoted index form (`---@field ["2fa"]? string`), which `LuaLS` keys by
/// the string's content — so `data["2fa"]` resolves to the field. An optional
/// marker follows either form.
pub(crate) fn lua_field_key(name: &str) -> String {
    if is_lua_name(name) && !LUALS_FIELD_SCOPES.contains(&name) {
        name.to_string()
    } else {
        lua_quoted_index(name)
    }
}

/// Escape a string for embedding inside a double-quoted string literal —
/// backslash, double-quote, newline, and carriage return. These four escapes
/// are valid across Lua, TypeScript/JS, Go, Python, and Rust, so all generators
/// share it for config values (e.g. select-option values) interpolated into
/// generated code.
pub(crate) fn escape_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Rust ──
    #[test]
    fn rust_field_keyword_uses_raw_no_rename() {
        let f = rust_field("type");
        assert_eq!(f.ident, "r#type");
        assert_eq!(f.rename, None, "r#type already serializes as \"type\"");
    }

    #[test]
    fn rust_field_nonraw_keyword_gets_underscore_and_rename() {
        let f = rust_field("self");
        assert_eq!(f.ident, "self_");
        assert_eq!(f.rename.as_deref(), Some("self"));
    }

    #[test]
    fn rust_field_leading_digit_prefixed_and_renamed() {
        let f = rust_field("2fa");
        assert_eq!(f.ident, "n2fa");
        assert_eq!(f.rename.as_deref(), Some("2fa"));
    }

    #[test]
    fn rust_field_plain_is_unchanged() {
        let f = rust_field("title");
        assert_eq!(f.ident, "title");
        assert_eq!(f.rename, None);
    }

    #[test]
    fn rust_type_fixes_leading_digit() {
        assert_eq!(rust_type("2fa"), "N2fa");
        assert_eq!(rust_type("Posts"), "Posts");
    }

    #[test]
    fn rust_type_guards_self() {
        // `Self` is the one reserved word `PascalCase` doesn't dodge.
        assert_eq!(rust_type("Self"), "Self_");
    }

    // ── Go ──
    #[test]
    fn go_exported_fixes_leading_digit_to_uppercase_start() {
        assert_eq!(go_exported("2fa"), "N2fa");
        assert_eq!(go_exported("FirstName"), "FirstName");
    }

    #[test]
    fn dedup_disambiguates_collisions() {
        let mut seen = std::collections::HashSet::new();
        assert_eq!(dedup("FirstName".into(), &mut seen), "FirstName");
        assert_eq!(dedup("FirstName".into(), &mut seen), "FirstName_2");
        assert_eq!(dedup("FirstName".into(), &mut seen), "FirstName_3");
        assert_eq!(dedup("Other".into(), &mut seen), "Other");
    }

    #[test]
    fn variant_ident_pascalizes_arbitrary_values() {
        assert_eq!(variant_ident("draft"), "Draft");
        assert_eq!(variant_ident("in progress"), "InProgress");
        assert_eq!(variant_ident("draft-mode"), "DraftMode");
        assert_eq!(variant_ident("2fa"), "N2fa");
        assert_eq!(variant_ident("--"), "Value");
    }

    // ── Python ──
    #[test]
    fn python_field_keyword_gets_trailing_underscore() {
        let (attr, wire) = python_field("class");
        assert_eq!(attr, "class_");
        assert_eq!(wire.as_deref(), Some("class"));
    }

    #[test]
    fn python_field_leading_digit_prefixed() {
        let (attr, wire) = python_field("3d");
        assert_eq!(attr, "n3d");
        assert_eq!(wire.as_deref(), Some("3d"));
    }

    #[test]
    fn python_field_plain_unchanged() {
        let (attr, wire) = python_field("title");
        assert_eq!(attr, "title");
        assert_eq!(wire, None);
    }

    #[test]
    fn python_class_fixes_leading_digit() {
        assert_eq!(python_class("2fa"), "N2fa");
    }

    // ── TypeScript ──
    #[test]
    fn ts_key_quotes_leading_digit_only() {
        assert_eq!(ts_key("2fa"), "\"2fa\"");
        assert_eq!(ts_key("type"), "type", "keywords are valid bare TS keys");
        assert_eq!(ts_key("title"), "title");
    }

    #[test]
    fn ts_type_fixes_leading_digit() {
        assert_eq!(ts_type("2fa"), "N2fa");
    }

    // ── Lua ──
    #[test]
    fn lua_index_brackets_leading_digit() {
        assert_eq!(
            lua_index("crap.collections", "2fa"),
            "crap.collections[\"2fa\"]"
        );
        assert_eq!(
            lua_index("crap.collections", "posts"),
            "crap.collections.posts"
        );
    }

    /// Regression: a slug spelled as a Lua reserved word was dot-indexed
    /// (`crap.collections.end`), a syntax error in the generated file.
    #[test]
    fn lua_index_brackets_reserved_words() {
        assert_eq!(
            lua_index("crap.collections", "end"),
            "crap.collections[\"end\"]"
        );
        assert_eq!(
            lua_index("crap.globals", "function"),
            "crap.globals[\"function\"]"
        );
        assert_eq!(
            lua_index("crap.collections", "ends"),
            "crap.collections.ends"
        );
    }

    #[test]
    fn is_lua_name_follows_the_lua_identifier_grammar() {
        for name in ["title", "_x", "a1", "Posts", "ends"] {
            assert!(is_lua_name(name), "{name}");
        }

        for name in ["", "2fa", "9", "end", "nil", "goto", "a-b"] {
            assert!(!is_lua_name(name), "{name}");
        }
    }

    /// Regression: a leading-digit field name was written bare
    /// (`---@field 2fa? string`), which `LuaLS` tokenizes as the integer `2`
    /// and the name `fa`.
    #[test]
    fn lua_field_key_quotes_non_identifiers() {
        assert_eq!(lua_field_key("2fa"), "[\"2fa\"]");
        assert_eq!(lua_field_key("2fa__en"), "[\"2fa__en\"]");
        assert_eq!(lua_field_key("end"), "[\"end\"]");
        assert_eq!(lua_field_key("title"), "title");
        assert_eq!(lua_field_key("seo__2fa"), "seo__2fa");
    }

    /// Regression: a field named after a `LuaLS` scope word
    /// (`---@field private? string`) had its name read as the scope.
    #[test]
    fn lua_field_key_quotes_luals_scope_words() {
        for name in ["public", "protected", "private", "package"] {
            assert_eq!(lua_field_key(name), format!("[\"{name}\"]"));
        }
    }

    #[test]
    fn escape_str_escapes_quote_backslash_newline_tab() {
        assert_eq!(escape_str("a\"b\\c\nd\te"), "a\\\"b\\\\c\\nd\\te");
        assert_eq!(escape_str("plain"), "plain");
    }
}
