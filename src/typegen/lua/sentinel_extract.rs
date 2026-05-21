//! Extract `@typegen-start ... @typegen-end` annotated regions from a
//! pure-Lua helper file and render them as standalone doc blocks for
//! `types/crap.lua`.
//!
//! A helper file looks like:
//!
//! ```text
//! -- @typegen-start crap.util — table helpers
//!
//! --- Deep merge two tables.
//! --- @param a table
//! --- @param b table
//! --- @return table
//! function util.deep_merge(a, b) ... end
//!
//! -- @typegen-end
//! ```
//!
//! Each annotated block is emitted as:
//!
//! ```text
//! -- ── crap.util — table helpers ──────────────────────────────
//!
//! --- Deep merge two tables.
//! --- @param a table
//! --- @param b table
//! --- @return table
//! function crap.util.deep_merge(a, b) end
//! ```
//!
//! The extractor reads ahead of each `function util.<name>(<args>)` line
//! and captures the contiguous `---` doc block immediately above. The
//! implementation body is dropped — only the signature + doc is kept,
//! with `function util.` rewritten to `function crap.util.`.

/// Emit all `@typegen-start ... @typegen-end` regions in `source` into
/// `out`. Section headers come from the trailing text of the start
/// sentinel — e.g. `-- @typegen-start crap.util — table helpers`
/// produces a `-- ── crap.util — table helpers ─` divider before the
/// doc/fn pairs.
pub fn render_sentinel_blocks(source: &str, out: &mut String) {
    let mut lines = source.lines().peekable();
    while let Some(line) = lines.next() {
        let Some(title) = line.strip_prefix("-- @typegen-start ").map(str::trim) else {
            continue;
        };

        out.push_str("-- ── ");
        out.push_str(title);
        out.push(' ');
        // Pad with box-drawing dashes to roughly the original 60-col rule.
        let target = 60usize;
        let used = "-- ── ".chars().count() + title.chars().count() + 1;
        let dashes = target.saturating_sub(used);
        for _ in 0..dashes {
            out.push('─');
        }
        out.push_str("\n\n");

        let mut doc_buf: Vec<&str> = Vec::new();
        for inner in lines.by_ref() {
            if inner.trim_start().starts_with("-- @typegen-end") {
                break;
            }

            if inner.starts_with("---") {
                doc_buf.push(inner);
                continue;
            }

            if let Some(sig) = parse_function_signature(inner) {
                for d in &doc_buf {
                    out.push_str(d);
                    out.push('\n');
                }
                out.push_str("function crap.util.");
                out.push_str(&sig);
                out.push_str(" end\n\n");
                doc_buf.clear();
                continue;
            }

            // Blank line between fns — keep doc_buf only if it was started.
            if inner.trim().is_empty() {
                doc_buf.clear();
            }
        }
    }
}

/// Parse `function util.<name>(<args>)` and return `<name>(<args>)`. Any
/// other line returns `None`.
fn parse_function_signature(line: &str) -> Option<String> {
    let rest = line.trim_start().strip_prefix("function util.")?;
    let paren = rest.find(')')?;
    Some(rest[..=paren].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_single_block() {
        let src = "\
-- @typegen-start crap.util — table helpers

--- Deep merge two tables.
--- @param a table
--- @param b table
--- @return table
function util.deep_merge(a, b)
    return {}
end

-- @typegen-end
";
        let mut out = String::new();
        render_sentinel_blocks(src, &mut out);

        assert!(out.contains("-- ── crap.util — table helpers"));
        assert!(out.contains("--- Deep merge two tables."));
        assert!(out.contains("function crap.util.deep_merge(a, b) end"));
        assert!(!out.contains("return {}"));
    }

    #[test]
    fn extracts_multiple_blocks() {
        let src = "\
local util = crap.util

-- @typegen-start crap.util — table helpers

--- A
function util.a() end

-- @typegen-end

-- @typegen-start crap.util — string helpers

--- B
function util.b(x) return x end

-- @typegen-end
";
        let mut out = String::new();
        render_sentinel_blocks(src, &mut out);

        assert!(out.contains("-- ── crap.util — table helpers"));
        assert!(out.contains("-- ── crap.util — string helpers"));
        assert!(out.contains("function crap.util.a() end"));
        assert!(out.contains("function crap.util.b(x) end"));
    }

    #[test]
    fn drops_orphan_doc_at_blank_line() {
        // Doc-comments not immediately followed by a function (separated
        // by a blank line) are dropped — protects against stray doc
        // blocks bleeding into the next function's docs.
        let src = "\
-- @typegen-start s

--- stale doc

--- real doc
function util.fn() end

-- @typegen-end
";
        let mut out = String::new();
        render_sentinel_blocks(src, &mut out);

        assert!(out.contains("--- real doc"));
        assert!(!out.contains("--- stale doc"));
    }
}
