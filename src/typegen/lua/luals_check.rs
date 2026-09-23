//! Test-only checks that generated Lua is valid for the Lua Language Server
//! (`LuaLS`), the editor tooling the generated types exist for.
//!
//! Two layers:
//!
//! - [`grammar_violations`] — hermetic: every declared class/alias name,
//!   every `---@field` key, every Lua statement, and every function type
//!   nested in a table or parameter list is checked against the `LuaLS`
//!   annotation grammar (`script/parser/luadoc.lua`) and the Lua grammar.
//!   Runs everywhere.
//! - [`luals_diagnostics`] — runs a real `lua-language-server --check` over
//!   the files and reports its Warning-level diagnostics. Needs the binary:
//!   the `CRAP_LUALS` environment variable, `lua-language-server` on `PATH`,
//!   or a Mason install (`~/.local/share/nvim/mason/bin`). Without one the
//!   caller skips.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use crate::typegen::idents::{is_lua_name, lua_field_key};

/// Names a `LuaLS` binary explicitly, overriding the `PATH` / Mason lookup.
pub(in crate::typegen) const LUALS_ENV: &str = "CRAP_LUALS";

/// The `LuaLS` executable's file name.
const LUALS_BIN: &str = "lua-language-server";

/// Workspace settings for the check: the Lua version the CMS embeds, so the
/// result doesn't depend on a developer's global `LuaLS` configuration.
const LUARC: &str = r#"{ "runtime.version": "Lua 5.4" }"#;

// ───────────────────────────── grammar ─────────────────────────────

/// The annotation text after `---` (or `--- `) and the tag, when `line` is a
/// `---@<tag>` annotation.
fn annotation<'a>(line: &'a str, tag: &str) -> Option<&'a str> {
    let rest = line.strip_prefix("---")?.trim_start().strip_prefix('@')?;

    rest.strip_prefix(tag)?.strip_prefix(' ')
}

/// Whether `name` is a valid declared class/alias name for this generator:
/// dot-separated segments of ASCII letters, digits and `_`, the first
/// segment starting with a letter or `_`. `LuaLS` tokenizes a leading digit as
/// an integer, but a later segment may start with one (`crap.data.2fa` is a
/// single name token).
fn is_type_name(name: &str) -> bool {
    let starts_ok = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');

    starts_ok
        && name.split('.').all(|segment| {
            !segment.is_empty()
                && segment
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

/// The class/alias name declared by `line`, if any.
fn declared_name(line: &str) -> Option<&str> {
    let rest = annotation(line, "class").or_else(|| annotation(line, "alias"))?;
    let rest = rest.strip_prefix("(exact)").map_or(rest, str::trim_start);

    rest.split_whitespace().next()
}

/// The key of a `---@field` line: up to the optional marker or the space
/// before the type.
fn field_key(line: &str) -> Option<&str> {
    let rest = annotation(line, "field")?;
    let end = rest.find(['?', ' ']).unwrap_or(rest.len());

    Some(&rest[..end])
}

/// Whether a `---@field` key is valid: an index form (`["2fa"]`, `[string]`)
/// or a bare name the generator would write bare.
fn is_valid_field_key(key: &str) -> bool {
    if key.starts_with('[') {
        return key.ends_with(']');
    }

    lua_field_key(key) == key
}

/// Whether `path` is a Lua variable path: dotted identifiers, optionally
/// ending in a quoted-string index.
fn is_lua_path(path: &str) -> bool {
    let (dotted, index) = match path.find('[') {
        Some(at) => (&path[..at], Some(&path[at..])),
        None => (path, None),
    };

    let index_ok = index.is_none_or(|i| {
        i.len() > 4
            && i.starts_with("[\"")
            && i.ends_with("\"]")
            && !i[2..i.len() - 2].contains('"')
    });

    index_ok && dotted.split('.').all(is_lua_name)
}

/// Whether a non-comment Lua statement line is well-formed for the shapes
/// the generators emit: `function <path>(…) end`, `local <name> = …`, and
/// `<path> = …`.
fn is_valid_statement(line: &str) -> bool {
    if let Some(rest) = line.strip_prefix("function ") {
        return rest
            .find('(')
            .is_some_and(|at| rest[..at].split('.').all(is_lua_name));
    }

    if let Some(rest) = line.strip_prefix("local ") {
        return rest.split(" = ").next().is_some_and(is_lua_name);
    }

    line.split_once(" = ")
        .is_some_and(|(target, _)| is_lua_path(target))
}

/// The byte index just past the bracket group opening at `open`, or the line
/// length when it never closes.
fn skip_group(bytes: &[u8], open: usize) -> usize {
    let mut depth = 0usize;

    for (i, &b) in bytes.iter().enumerate().skip(open) {
        match b {
            b'(' | b'{' | b'[' | b'<' => depth += 1,
            b')' | b'}' | b']' | b'>' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
    }

    bytes.len()
}

/// Whether the return list starting at `from` runs into a `,` before its
/// enclosing bracket closes — the comma `LuaLS` would read as a further
/// return value.
fn return_list_hits_comma(bytes: &[u8], from: usize) -> bool {
    let mut depth = 0usize;

    for &b in &bytes[from..] {
        match b {
            b'(' | b'{' | b'[' | b'<' => depth += 1,
            b')' | b'}' | b']' | b'>' if depth == 0 => return false,
            b')' | b'}' | b']' | b'>' => depth -= 1,
            b',' if depth == 0 => return true,
            _ => {}
        }
    }

    false
}

/// Whether `line` holds a `name: fun(…): T` table field or parameter that is
/// followed by another one. `LuaLS` reads the `, next: …` as more (named)
/// return values of the function, silently swallowing the rest of the table
/// or parameter list; the function type must be parenthesized.
fn swallows_following_members(line: &str) -> bool {
    let bytes = line.as_bytes();
    let mut from = 0;

    while let Some(at) = line[from..].find(": fun(").map(|i| from + i) {
        let close = skip_group(bytes, at + ": fun".len());
        from = at + 1;

        let rest = &line[close..];
        if !rest.starts_with(':') {
            continue;
        }

        if return_list_hits_comma(bytes, close + 1) {
            return true;
        }
    }

    false
}

/// Every construct in `out` that `LuaLS` (or Lua) would misparse, one message
/// per offending line.
pub(in crate::typegen) fn grammar_violations(out: &str) -> Vec<String> {
    let mut violations = Vec::new();

    for (n, line) in out.lines().enumerate() {
        if let Some(problem) = line_problem(line) {
            violations.push(format!("line {}: {problem}: {line}", n + 1));
        }
    }

    violations
}

/// What is wrong with one line, if anything.
fn line_problem(line: &str) -> Option<&'static str> {
    if let Some(name) = declared_name(line) {
        return (!is_type_name(name)).then_some("invalid class/alias name");
    }

    if let Some(key) = field_key(line) {
        return (!is_valid_field_key(key)).then_some("invalid field key");
    }

    if line.starts_with("--") {
        return swallows_following_members(line)
            .then_some("unparenthesized function type followed by another member");
    }

    if line.trim().is_empty() {
        return None;
    }

    (!is_valid_statement(line)).then_some("invalid Lua statement")
}

// ───────────────────────────── LuaLS ─────────────────────────────

/// The `LuaLS` binary at `path`, when it is a file.
fn existing(path: PathBuf) -> Option<PathBuf> {
    path.is_file().then_some(path)
}

/// Locate a `LuaLS` binary: `CRAP_LUALS`, then `PATH`, then a Mason install.
fn find_luals() -> Option<PathBuf> {
    if let Some(explicit) = env::var_os(LUALS_ENV).filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(explicit));
    }

    let on_path = env::var_os("PATH")
        .and_then(|paths| env::split_paths(&paths).find_map(|dir| existing(dir.join(LUALS_BIN))));

    on_path.or_else(|| {
        let home = PathBuf::from(env::var_os("HOME")?);

        existing(home.join(".local/share/nvim/mason/bin").join(LUALS_BIN))
    })
}

/// Strip ANSI colour sequences from `LuaLS`'s pretty report.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();

    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }

        for c in chars.by_ref() {
            if c.is_ascii_alphabetic() {
                break;
            }
        }
    }

    out
}

/// Run `LuaLS --check` over a workspace holding `files` (`(file name,
/// content)`) at Warning level.
///
/// `None` when no `LuaLS` binary is available; otherwise `Ok(())` for a
/// clean check or `Err(report)` with the diagnostics.
pub(in crate::typegen) fn luals_diagnostics(files: &[(&str, &str)]) -> Option<Result<(), String>> {
    let bin = find_luals()?;
    let root = tempfile::tempdir().expect("create LuaLS scratch dir");

    Some(run_check(&bin, root.path(), files))
}

/// Write `files` into `<root>/workspace` and the pinned config beside it;
/// returns the workspace and config paths. The config, logs and meta files
/// stay outside the checked workspace.
fn write_workspace(root: &Path, files: &[(&str, &str)]) -> (PathBuf, PathBuf) {
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace).expect("create LuaLS workspace");

    for (name, content) in files {
        fs::write(workspace.join(name), content).expect("write LuaLS workspace file");
    }

    let config = root.join("luarc.json");
    fs::write(&config, LUARC).expect("write LuaLS config");

    (workspace, config)
}

/// [`luals_diagnostics`] against a found binary, in a fresh scratch `root`.
fn run_check(bin: &Path, root: &Path, files: &[(&str, &str)]) -> Result<(), String> {
    let (workspace, config) = write_workspace(root, files);

    let output = Command::new(bin)
        .arg(format!("--check={}", workspace.display()))
        .arg("--checklevel=Warning")
        .arg(format!("--configpath={}", config.display()))
        .arg(format!("--logpath={}", root.join("log").display()))
        .arg(format!("--metapath={}", root.join("meta").display()))
        .output()
        .map_err(|e| format!("could not run {}: {e}", bin.display()))?;

    let report = strip_ansi(&String::from_utf8_lossy(&output.stdout));
    let clean = output.status.success()
        && (report.contains("no problems found") || !report.contains("problems found"));

    if clean {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);

    Err(format!("{stderr}\n{report}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_names_follow_the_luals_name_token() {
        for name in ["crap.data.Posts", "crap.data.2fa", "crap.hook.global_end"] {
            assert!(is_type_name(name), "{name}");
        }

        for name in ["2fa", "crap..x", "crap.data.", "crap.a-b"] {
            assert!(!is_type_name(name), "{name}");
        }
    }

    #[test]
    fn field_keys_must_be_bare_names_or_indexes() {
        for line in [
            "---@field title? string",
            "---@field [\"2fa\"]? string",
            "--- @field [string] any",
            "---@field [\"end\"] string",
        ] {
            assert_eq!(line_problem(line), None, "{line}");
        }

        for line in [
            "---@field 2fa? string",
            "---@field end string",
            "---@field private? string",
        ] {
            assert_eq!(line_problem(line), Some("invalid field key"), "{line}");
        }
    }

    #[test]
    fn statements_must_be_valid_lua() {
        for line in [
            "function _coll_2fa.hook(fn) end",
            "local _coll_2fa = {}",
            "crap.collections[\"2fa\"] = _coll_2fa",
            "crap.globals.settings = _glob_settings",
            "crap.util = {}",
        ] {
            assert_eq!(line_problem(line), None, "{line}");
        }

        for line in [
            "function crap.collections.2fa.hook(fn) end",
            "crap.collections.2fa = _coll_2fa",
            "crap.globals.end = _glob_end",
        ] {
            assert_eq!(line_problem(line), Some("invalid Lua statement"), "{line}");
        }
    }

    /// Regression: a handler table type listed `get: fun(key: string):
    /// string?, delete: …` — `LuaLS` read `delete: …` as a named second return
    /// of `get`.
    #[test]
    fn returning_function_members_must_be_parenthesized() {
        let bad = "--- @param h { get: fun(k: string): string?, delete: fun(k: string) }";
        assert!(swallows_following_members(bad));

        for ok in [
            "--- @param h { get: (fun(k: string): string?), delete: fun(k: string) }",
            "--- @param h { delete: fun(k: string), has?: fun(k: string): boolean }",
            "---@overload fun(field: \"x\", fn: fun(value: string, ctx: crap.C): string?)",
            "--- @param h { get: fun(k: string): table<string, any> }",
            "---@return fun(a: string): string, integer",
        ] {
            assert!(!swallows_following_members(ok), "{ok}");
        }
    }

    #[test]
    fn strip_ansi_removes_colour_sequences() {
        assert_eq!(strip_ansi("\u{1b}[31mred\u{1b}[0m plain"), "red plain");
    }
}
