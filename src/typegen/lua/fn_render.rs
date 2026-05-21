//! Render a [`LuaFnSpec`] as the `function crap.X.Y(...) end` Lua block
//! with `--- @param` and `--- @return` annotations above. Also hosts
//! `format_lua_section_header` — the helper `lua_table!` calls when its
//! `header:` attr is set.
//!
//! Output shape per spec:
//!
//! ```lua
//! --- {doc line 1}
//! --- {doc line 2}
//! --- @param {param.name} {param.ty}  {param.doc}
//! --- @return {return.ty}  {return.doc}
//! function crap.X.Y({param.name}, ...) end
//!
//! ```
//!
//! (Trailing blank line so successive function blocks read as paragraphs.)

use super::fn_spec::LuaFnSpec;

/// Append one Lua function block (docs + `@param` / `@return` + signature)
/// to `out`.
pub fn format_lua_fn_spec(out: &mut String, spec: &LuaFnSpec) {
    push_doc_block(out, spec.doc);

    for p in spec.params {
        out.push_str("--- @param ");
        out.push_str(p.name);
        out.push(' ');
        out.push_str(p.ty);
        if !p.doc.is_empty() {
            out.push_str("  ");
            out.push_str(p.doc);
        }
        out.push('\n');
    }

    if let Some(r) = &spec.returns {
        out.push_str("--- @return ");
        out.push_str(r.ty);
        if !r.doc.is_empty() {
            out.push_str("  ");
            out.push_str(r.doc);
        }
        out.push('\n');
    }

    out.push_str("function ");
    out.push_str(spec.path);
    out.push('(');
    for (i, p) in spec.params.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(p.name);
    }
    out.push_str(") end\n\n");
}

/// Emit doc-comment lines as Lua `--- ` prose. Empty doc lines emit
/// `---\n` (no trailing space). Mirrors the `LuaAnnotation` derive's
/// `push_doc_line` so generated output is uniform.
fn push_doc_block(out: &mut String, lines: &[&str]) {
    for line in lines {
        if line.is_empty() {
            out.push_str("---\n");
        } else {
            out.push_str("--- ");
            out.push_str(line);
            out.push('\n');
        }
    }
}

/// Emit the standard `crap.X` namespace preamble: section divider, doc
/// prose lines (split on `\n`), the `--- @class crap.X` declaration, and
/// the `crap.X = {}` initializer. Used by [`lua_table!`] when a
/// `header:` attribute is supplied so the namespace stub doesn't have
/// to live in a hand-written `.lua` block file. Output ends with a
/// trailing blank line so function blocks that follow read as their
/// own paragraph.
///
/// The divider is normalized to 64 visual columns (matching the most
/// common width in the original on-disk file) so every namespace ends
/// up with the same alignment regardless of path length.
pub fn format_lua_section_header(out: &mut String, path: &str, doc: &str) {
    const TARGET_WIDTH: usize = 64;

    let prefix_cols = "-- ── ".chars().count() + path.chars().count() + 1;
    let dashes = TARGET_WIDTH.saturating_sub(prefix_cols);

    out.push_str("-- ── ");
    out.push_str(path);
    out.push(' ');
    for _ in 0..dashes {
        out.push('─');
    }
    out.push_str("\n\n");

    let lines: Vec<&str> = doc.split('\n').collect();
    push_doc_block(out, &lines);

    out.push_str("--- @class ");
    out.push_str(path);
    out.push('\n');
    out.push_str(path);
    out.push_str(" = {}\n\n");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typegen::lua::fn_spec::{LuaFnSpec, LuaParam, LuaReturn};

    #[test]
    fn renders_simple_fn_with_one_string_param() {
        let spec = LuaFnSpec {
            path: "crap.util.slugify",
            doc: &["Generate a URL-safe slug from a string."],
            params: &[LuaParam {
                name: "s",
                ty: "string",
                doc: "",
            }],
            returns: Some(LuaReturn {
                ty: "string",
                doc: "",
            }),
        };

        let mut out = String::new();
        format_lua_fn_spec(&mut out, &spec);

        let expected = concat!(
            "--- Generate a URL-safe slug from a string.\n",
            "--- @param s string\n",
            "--- @return string\n",
            "function crap.util.slugify(s) end\n",
            "\n",
        );
        assert_eq!(out, expected, "render drift:\n{out}");
    }

    #[test]
    fn renders_no_params_no_return() {
        let spec = LuaFnSpec {
            path: "crap.util.nanoid",
            doc: &["Generate a unique nanoid."],
            params: &[],
            returns: Some(LuaReturn {
                ty: "string",
                doc: "Random nanoid string.",
            }),
        };

        let mut out = String::new();
        format_lua_fn_spec(&mut out, &spec);

        let expected = concat!(
            "--- Generate a unique nanoid.\n",
            "--- @return string  Random nanoid string.\n",
            "function crap.util.nanoid() end\n",
            "\n",
        );
        assert_eq!(out, expected);
    }

    #[test]
    fn renders_void_return_skipped() {
        let spec = LuaFnSpec {
            path: "crap.log.info",
            doc: &["Log an info-level message."],
            params: &[LuaParam {
                name: "msg",
                ty: "string",
                doc: "Log message.",
            }],
            returns: None,
        };

        let mut out = String::new();
        format_lua_fn_spec(&mut out, &spec);

        let expected = concat!(
            "--- Log an info-level message.\n",
            "--- @param msg string  Log message.\n",
            "function crap.log.info(msg) end\n",
            "\n",
        );
        assert_eq!(out, expected);
    }

    #[test]
    fn renders_multi_line_doc_and_two_params() {
        let spec = LuaFnSpec {
            path: "crap.collections.define",
            doc: &[
                "Define a new collection.",
                "",
                "Call this in collection definition files.",
            ],
            params: &[
                LuaParam {
                    name: "slug",
                    ty: "string",
                    doc: "Collection slug.",
                },
                LuaParam {
                    name: "config",
                    ty: "crap.CollectionConfig",
                    doc: "Collection configuration.",
                },
            ],
            returns: None,
        };

        let mut out = String::new();
        format_lua_fn_spec(&mut out, &spec);

        let expected = concat!(
            "--- Define a new collection.\n",
            "---\n",
            "--- Call this in collection definition files.\n",
            "--- @param slug string  Collection slug.\n",
            "--- @param config crap.CollectionConfig  Collection configuration.\n",
            "function crap.collections.define(slug, config) end\n",
            "\n",
        );
        assert_eq!(out, expected);
    }
}
