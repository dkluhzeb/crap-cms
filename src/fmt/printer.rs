//! Re-indent and re-emit a tokenized Handlebars template.
//!
//! Indent is driven by both HTML element nesting *and* Handlebars block
//! nesting (`{{#if}}`, `{{#each}}`, `{{#> partial}}`, …). Each opens an
//! indent level for its body; `{{else}}` / `{{else if}}` returns to the
//! parent for the keyword line. Inline forms — when the entire block
//! body fits on one source line and stays under the line limit — are
//! preserved as-is.
//!
//! The printer is intentionally stack-based with no AST: every emitted
//! line knows its column from the nesting stack at that point in the
//! stream. This avoids whole-program re-balancing bugs.

use std::fmt::Write as _;

use anyhow::Result;

use crate::fmt::tokenizer::{Attr, Token, is_void, parse_attributes};

const INDENT: &str = "  ";
const LINE_LIMIT: usize = 100;

/// Booleans whose presence is meaningful and whose value (if any) is
/// dropped: rendered as `required` not `required="required"`.
const BOOLEAN_ATTRS: &[&str] = &[
    "checked",
    "disabled",
    "hidden",
    "multiple",
    "readonly",
    "required",
    "selected",
    "autofocus",
    "novalidate",
    "open",
    "default",
    "reversed",
    "ismap",
    "loop",
    "muted",
    "controls",
    "autoplay",
    "playsinline",
    "async",
    "defer",
    "formnovalidate",
];

pub fn print(tokens: &[Token<'_>]) -> Result<String> {
    let mut out = String::new();
    let mut depth: usize = 0;
    let mut at_line_start = true;

    let mut i = 0;
    while i < tokens.len() {
        let t = &tokens[i];

        // Detect inline runs: a block-opener whose matching close is in
        // the same source line as the opener and whose total reflow
        // length stays within LINE_LIMIT. We render those verbatim
        // (apart from leading-indent normalisation) without recursing
        // into the depth machinery.
        if let Some((rendered, consumed)) = try_render_inline(tokens, i, depth)? {
            ensure_line_indent(&mut out, &mut at_line_start, depth);
            out.push_str(&rendered);
            at_line_start = rendered.ends_with('\n');
            i += consumed;
            continue;
        }

        match t {
            Token::Text(s) => {
                emit_text(s, &mut out, &mut at_line_start, depth);
            }

            Token::RawText(s) => {
                emit_raw_text(s, &mut out, &mut at_line_start);
            }

            Token::HtmlComment(raw) => {
                ensure_line_indent(&mut out, &mut at_line_start, depth);
                emit_verbatim_block(raw, &mut out, depth);
                newline(&mut out, &mut at_line_start);
            }

            Token::HbsComment(raw) => {
                ensure_line_indent(&mut out, &mut at_line_start, depth);
                emit_verbatim_block(raw, &mut out, depth);
                newline(&mut out, &mut at_line_start);
            }

            Token::HtmlStart {
                name,
                attrs_raw,
                self_closed,
            } => {
                emit_start_tag(
                    name,
                    attrs_raw,
                    *self_closed,
                    depth,
                    &mut out,
                    &mut at_line_start,
                )?;
                if !self_closed && !is_void(name) {
                    depth += 1;
                }
            }

            Token::HtmlEnd { name } => {
                depth = depth.saturating_sub(1);
                ensure_line_indent(&mut out, &mut at_line_start, depth);
                write!(&mut out, "</{name}>").unwrap();
                newline(&mut out, &mut at_line_start);
            }

            Token::HbsBlockOpen(raw) | Token::HbsPartialOpen(raw) => {
                ensure_line_indent(&mut out, &mut at_line_start, depth);
                out.push_str(&normalize_mustache(raw));
                newline(&mut out, &mut at_line_start);
                depth += 1;
            }

            Token::HbsElse(raw) => {
                let parent = depth.saturating_sub(1);
                ensure_line_indent(&mut out, &mut at_line_start, parent);
                out.push_str(&normalize_mustache(raw));
                newline(&mut out, &mut at_line_start);
            }

            Token::HbsBlockClose(raw) | Token::HbsPartialClose(raw) => {
                depth = depth.saturating_sub(1);
                ensure_line_indent(&mut out, &mut at_line_start, depth);
                out.push_str(&normalize_mustache(raw));
                newline(&mut out, &mut at_line_start);
            }

            Token::HbsExpr(raw) => {
                ensure_line_indent(&mut out, &mut at_line_start, depth);
                out.push_str(&normalize_mustache(raw));
                // Don't force a newline — expressions often appear
                // inline within text. The next token decides.
            }
        }

        i += 1;
    }

    Ok(post_process(&out))
}

/// If `tokens[at]` opens a block whose corresponding close is on the
/// same source-line and the rendered inline form fits within
/// LINE_LIMIT, render the full run on one line. Returns
/// `(rendered_line, tokens_consumed)` or `None` to fall through.
fn try_render_inline(
    tokens: &[Token<'_>],
    at: usize,
    depth: usize,
) -> Result<Option<(String, usize)>> {
    let opener = &tokens[at];
    // Inline collapse rules:
    //   - Block helpers (`{{#if}}`, `{{#> partial}}`) never inline.
    //   - HTML tags with 2+ attributes never inline (rule 3 stacks them).
    //   - HTML tags with 0-1 plain attributes may inline iff the body
    //     is a single short text/expression run with no nested
    //     elements or block helpers.
    let (need_close_kind, opener_str) = match opener {
        Token::HtmlStart {
            name,
            attrs_raw,
            self_closed,
        } if !self_closed && !is_void(name) => {
            let attrs = parse_attributes(attrs_raw)?;
            let inline_attr_ok = attrs.len() <= 1
                && attrs
                    .iter()
                    .all(|a| matches!(a, Attr::Plain { .. } | Attr::HbsExpr(_)));
            if !inline_attr_ok {
                return Ok(None);
            }
            let mut buf = String::with_capacity(name.len() + attrs_raw.len() + 2);
            buf.push('<');
            buf.push_str(name);
            for a in &attrs {
                buf.push(' ');
                buf.push_str(&render_attr(a));
            }
            buf.push('>');
            (BlockKind::HtmlEnd(name.clone()), buf)
        }
        _ => return Ok(None),
    };

    // Walk forward until we find the matching closer at the same depth.
    let mut depth_stack: Vec<BlockKind> = vec![need_close_kind.clone()];
    let mut j = at + 1;
    while j < tokens.len() {
        match &tokens[j] {
            Token::HtmlStart {
                name, self_closed, ..
            } if !self_closed && !is_void(name) => {
                depth_stack.push(BlockKind::HtmlEnd(name.clone()));
            }
            Token::HtmlEnd { name } => {
                if let Some(BlockKind::HtmlEnd(open)) = depth_stack.last()
                    && open == name
                {
                    depth_stack.pop();
                    if depth_stack.is_empty() {
                        break;
                    }
                }
            }
            Token::HbsBlockOpen(_) => depth_stack.push(BlockKind::HbsClose),
            Token::HbsBlockClose(_) => {
                if matches!(depth_stack.last(), Some(BlockKind::HbsClose)) {
                    depth_stack.pop();
                    if depth_stack.is_empty() {
                        break;
                    }
                }
            }
            Token::HbsPartialOpen(_) => depth_stack.push(BlockKind::HbsPartialClose),
            Token::HbsPartialClose(_) => {
                if matches!(depth_stack.last(), Some(BlockKind::HbsPartialClose)) {
                    depth_stack.pop();
                    if depth_stack.is_empty() {
                        break;
                    }
                }
            }
            // Inline runs disqualify when an `{{else}}` appears at the
            // outermost level — multi-branch blocks never collapse.
            Token::HbsElse(_) if depth_stack.len() == 1 => return Ok(None),
            _ => {}
        }
        j += 1;
    }
    if j >= tokens.len() {
        return Ok(None); // unmatched — let the main loop handle errors.
    }

    // Check that none of the body tokens introduce structural newlines.
    // For HTML: a nested block-level start that itself has a body
    // disqualifies. For text: must not contain `\n` in a way that
    // suggests the source was multi-line.
    if !body_is_single_logical_line(&tokens[at + 1..j])? {
        return Ok(None);
    }

    let mut buf = String::new();
    buf.push_str(&opener_str);
    for tok in &tokens[at + 1..j] {
        match tok {
            Token::Text(s) => buf.push_str(&collapse_inline_whitespace(s)),
            Token::HtmlStart {
                name,
                attrs_raw,
                self_closed,
            } => buf.push_str(&render_self_or_void_inline(name, attrs_raw, *self_closed)?),
            Token::HtmlEnd { name } => write!(&mut buf, "</{name}>").unwrap(),
            Token::HtmlComment(raw) => buf.push_str(raw),
            Token::HbsComment(raw) => buf.push_str(raw),
            Token::HbsExpr(raw) => buf.push_str(&normalize_mustache(raw)),
            Token::HbsBlockOpen(raw) => buf.push_str(&normalize_mustache(raw)),
            Token::HbsBlockClose(raw) => buf.push_str(&normalize_mustache(raw)),
            Token::HbsElse(raw) => buf.push_str(&normalize_mustache(raw)),
            Token::HbsPartialOpen(raw) => buf.push_str(&normalize_mustache(raw)),
            Token::HbsPartialClose(raw) => buf.push_str(&normalize_mustache(raw)),
            // Unreachable: body_is_single_logical_line bails on RawText
            // before we get here.
            Token::RawText(_) => return Ok(None),
        }
    }
    // Append the matching closer (`tokens[j]`).
    match &tokens[j] {
        Token::HtmlEnd { name } => write!(&mut buf, "</{name}>").unwrap(),
        Token::HbsBlockClose(raw) | Token::HbsPartialClose(raw) => {
            buf.push_str(&normalize_mustache(raw));
        }
        _ => unreachable!(),
    }

    let rendered = buf.trim_end().to_string();
    if rendered.contains('\n') {
        return Ok(None);
    }
    if rendered.len() + depth * INDENT.len() > LINE_LIMIT {
        return Ok(None);
    }
    Ok(Some((rendered + "\n", j - at + 1)))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BlockKind {
    HtmlEnd(String),
    HbsClose,
    HbsPartialClose,
}

fn body_is_single_logical_line(body: &[Token<'_>]) -> Result<bool> {
    for tok in body {
        match tok {
            Token::Text(s) => {
                // Reject text that has internal blank lines or trailing
                // newlines that aren't pure trim-space.
                if s.contains('\n') {
                    let trimmed = s.trim();
                    if !trimmed.is_empty() && trimmed.contains('\n') {
                        return Ok(false);
                    }
                }
            }
            Token::HtmlComment(s) | Token::HbsComment(s) => {
                if s.contains('\n') {
                    return Ok(false);
                }
            }
            // Nested block-level openers that themselves have bodies
            // disqualify the inline collapse — we only inline atoms
            // and one-liner siblings.
            Token::HtmlStart {
                name, self_closed, ..
            } => {
                if !self_closed && !is_void(name) {
                    return Ok(false);
                }
            }
            Token::HbsBlockOpen(_) | Token::HbsPartialOpen(_) => return Ok(false),
            // RawText body (inside <script>/<style>/<pre>/<textarea>)
            // is verbatim and never collapses to inline form.
            Token::RawText(_) => return Ok(false),
            _ => {}
        }
    }
    Ok(true)
}

fn render_self_or_void_inline(name: &str, attrs_raw: &str, self_closed: bool) -> Result<String> {
    let attrs = parse_attributes(attrs_raw)?;
    let mut out = String::new();
    out.push('<');
    out.push_str(name);
    for a in &attrs {
        out.push(' ');
        out.push_str(&render_attr(a));
    }
    if self_closed || is_void(name) {
        out.push_str(" />");
    } else {
        out.push('>');
    }
    Ok(out)
}

fn emit_start_tag(
    name: &str,
    attrs_raw: &str,
    self_closed: bool,
    depth: usize,
    out: &mut String,
    at_line_start: &mut bool,
) -> Result<()> {
    let attrs = parse_attributes(attrs_raw)?;
    let void = is_void(name);
    let close = if self_closed || void { " />" } else { ">" };

    // Inline form: zero or one attribute (no embedded HBS block).
    let inline_eligible = attrs
        .iter()
        .all(|a| matches!(a, Attr::Plain { .. } | Attr::HbsExpr(_)))
        && attrs.len() <= 1;
    if inline_eligible {
        ensure_line_indent(out, at_line_start, depth);
        out.push('<');
        out.push_str(name);
        for a in &attrs {
            out.push(' ');
            out.push_str(&render_attr(a));
        }
        out.push_str(close);
        newline(out, at_line_start);
        return Ok(());
    }

    // Stacked form: one attribute per line, closing `>` on its own line.
    ensure_line_indent(out, at_line_start, depth);
    out.push('<');
    out.push_str(name);
    let attr_indent = INDENT.repeat(depth + 1);
    for a in &attrs {
        out.push('\n');
        out.push_str(&attr_indent);
        out.push_str(&render_attr(a));
    }
    out.push('\n');
    out.push_str(&INDENT.repeat(depth));
    out.push_str(close.trim_start());
    newline(out, at_line_start);
    Ok(())
}

fn render_attr(a: &Attr) -> String {
    match a {
        Attr::Plain { name, value } => {
            // Boolean attributes collapse to bare form ONLY when the
            // value is absent, empty, or echoes the attribute name —
            // i.e. `selected`, `selected=""`, `selected="selected"`.
            // A meaningful value (`selected='{{json …}}'`) is kept,
            // because the same attribute name can carry data on
            // custom elements.
            let collapse_boolean = BOOLEAN_ATTRS.contains(&name.as_str())
                && match value {
                    None => true,
                    Some(v) => v.is_empty() || v.eq_ignore_ascii_case(name),
                };
            if collapse_boolean {
                return name.clone();
            }
            match value {
                None => name.clone(),
                Some(v) => {
                    // Default to double quotes (style guide). Switch to
                    // single quotes if the value contains a literal `"`
                    // — common for inline JSON in `content='{"...":...}'`.
                    if v.contains('"') && !v.contains('\'') {
                        format!("{name}='{v}'")
                    } else {
                        format!("{name}=\"{v}\"")
                    }
                }
            }
        }
        Attr::HbsExpr(s) | Attr::HbsBlock(s) => normalize_mustache(s),
    }
}

/// Normalise spacing inside a single mustache token. Trims leading and
/// trailing whitespace inside `{{` / `}}`. Doesn't touch sub-expressions.
fn normalize_mustache(raw: &str) -> String {
    if raw.starts_with("{{!") {
        // Comments are passthrough.
        return raw.to_string();
    }
    let (open, close, body) = if raw.starts_with("{{{") {
        ("{{{", "}}}", &raw[3..raw.len() - 3])
    } else {
        ("{{", "}}", &raw[2..raw.len() - 2])
    };
    let trimmed = body.trim();
    format!("{open}{trimmed}{close}")
}

fn collapse_inline_whitespace(s: &str) -> String {
    // Inline runs preserve internal whitespace but never embed newlines.
    s.replace('\n', " ")
        .replace("  ", " ")
        .trim_matches(' ')
        .to_string()
}

/// Emit raw-content body (`<script>`, `<style>`, `<pre>`, `<textarea>`)
/// verbatim. The start tag's `newline()` already left `at_line_start =
/// true`, so we strip a single leading newline from the body to avoid
/// doubling. We also strip a trailing whitespace-only run after the
/// last newline — that's the source's indent before `</tag>` and is
/// contextual rather than content. Preserving it would render a blank
/// line before the close tag once `post_process` trims the trailing
/// spaces. Whitespace-only bodies (empty `<script>` tags whose source
/// happened to span multiple lines) are dropped entirely. Trailing
/// state is set by whether the body ends on a newline.
fn emit_raw_text(s: &str, out: &mut String, at_line_start: &mut bool) {
    if s.trim().is_empty() {
        return;
    }
    let body = s.strip_prefix('\n').unwrap_or(s);
    let body = match body.rfind('\n') {
        Some(i) if body[i + 1..].trim().is_empty() => &body[..=i],
        _ => body,
    };
    if body.is_empty() {
        return;
    }
    out.push_str(body);
    *at_line_start = body.ends_with('\n');
}

fn emit_text(s: &str, out: &mut String, at_line_start: &mut bool, depth: usize) {
    // Split text into segments separated by '\n', but preserve internal
    // structure of multi-line text (e.g. paragraph content).
    if s.trim().is_empty() {
        // Pure whitespace between block-level elements: emit at most
        // one blank line.
        let newlines = s.bytes().filter(|b| *b == b'\n').count();
        if newlines >= 2 && !*at_line_start {
            // Already on its own line — fall through to blank insertion.
        }
        if newlines >= 2 {
            // Ensure exactly one blank line.
            if !out.ends_with("\n\n") && !out.is_empty() {
                if !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push('\n');
                *at_line_start = true;
            }
        } else if !*at_line_start && newlines >= 1 {
            // Mid-line text run that ends with newline → close the line.
        }
        return;
    }

    // Non-empty text: split into lines, emit each line at the current
    // indent. Leading/trailing whitespace within each line is trimmed.
    let lines: Vec<&str> = s.split('\n').collect();
    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if idx > 0 && idx < lines.len() - 1 {
                // Blank line in the middle of a text run → preserve as
                // a blank line between content lines.
                if !out.ends_with("\n\n") {
                    if !*at_line_start {
                        out.push('\n');
                    }
                    out.push('\n');
                    *at_line_start = true;
                }
            }
            continue;
        }
        if *at_line_start {
            out.push_str(&INDENT.repeat(depth));
        }
        out.push_str(trimmed);
        // If this isn't the last line, terminate it.
        if idx < lines.len() - 1 {
            out.push('\n');
            *at_line_start = true;
        } else {
            // End-of-text-run: if the source ended with a newline, also
            // terminate; otherwise leave inline (next token continues
            // on the same line).
            if s.ends_with('\n') {
                out.push('\n');
                *at_line_start = true;
            } else {
                *at_line_start = false;
            }
        }
    }
}

fn ensure_line_indent(out: &mut String, at_line_start: &mut bool, depth: usize) {
    if !*at_line_start {
        out.push('\n');
        *at_line_start = true;
    }
    if *at_line_start {
        out.push_str(&INDENT.repeat(depth));
        *at_line_start = false;
    }
}

fn newline(out: &mut String, at_line_start: &mut bool) {
    if !out.ends_with('\n') {
        out.push('\n');
    }
    *at_line_start = true;
}

fn emit_verbatim_block(raw: &str, out: &mut String, _depth: usize) {
    // Comments are emitted exactly as-is. Internal indentation,
    // line-breaks, and structure are preserved. The leading position
    // is determined by the surrounding context.
    out.push_str(raw);
}

/// Final cleanup pass — collapses 3+ consecutive newlines to 2 (one
/// blank line max) and ensures a single trailing newline.
fn post_process(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut newline_run = 0;
    for ch in s.chars() {
        if ch == '\n' {
            newline_run += 1;
            if newline_run <= 2 {
                out.push('\n');
            }
        } else {
            newline_run = 0;
            out.push(ch);
        }
    }
    // Strip trailing newlines, then add exactly one.
    while out.ends_with('\n') {
        out.pop();
    }
    out.push('\n');
    // Strip trailing whitespace per line.
    out.lines()
        .map(|l| l.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

#[cfg(test)]
mod tests {
    use crate::fmt::format;

    fn fmt(s: &str) -> String {
        format(s).expect("format should succeed")
    }

    #[test]
    fn basic_html_indent() {
        let src = "<div><p>hi</p></div>";
        assert_eq!(fmt(src), "<div>\n  <p>hi</p>\n</div>\n");
    }

    #[test]
    fn block_helper_indents_body() {
        let src = "{{#if x}}<p>hi</p>{{/if}}";
        let out = fmt(src);
        assert_eq!(out, "{{#if x}}\n  <p>hi</p>\n{{/if}}\n");
    }

    #[test]
    fn else_returns_to_parent_level() {
        let src = "{{#if x}}<p>a</p>{{else}}<p>b</p>{{/if}}";
        let out = fmt(src);
        assert_eq!(
            out,
            "{{#if x}}\n  <p>a</p>\n{{else}}\n  <p>b</p>\n{{/if}}\n"
        );
    }

    #[test]
    fn partial_block_indents_body() {
        let src = "{{#> partials/field}}<input />{{/partials/field}}";
        let out = fmt(src);
        assert!(out.contains("\n  <input />"));
        assert!(out.starts_with("{{#> partials/field}}"));
        assert!(out.contains("\n{{/partials/field}}"));
    }

    #[test]
    fn attrs_stacked_when_multiple() {
        let src = r#"<a class="card" href="/x" hx-get="/x">y</a>"#;
        let out = fmt(src);
        let expected = "<a\n  class=\"card\"\n  href=\"/x\"\n  hx-get=\"/x\"\n>\n  y\n</a>\n";
        assert_eq!(out, expected);
    }

    #[test]
    fn attrs_single_attr_short_body_inlines() {
        let src = r#"<a href="/x">y</a>"#;
        let out = fmt(src);
        assert_eq!(out, "<a href=\"/x\">y</a>\n");
    }

    #[test]
    fn attrs_single_attr_with_nested_element_stacks() {
        let src = r#"<p class="x"><span>y</span></p>"#;
        let out = fmt(src);
        // Body contains a nested element — multi-line.
        assert_eq!(out, "<p class=\"x\">\n  <span>y</span>\n</p>\n");
    }

    #[test]
    fn boolean_attr_bare() {
        let src = r#"<input required="required" type="text" />"#;
        let out = fmt(src);
        assert!(out.contains("required\n"));
        assert!(!out.contains("required=\""));
    }

    #[test]
    fn void_self_closes() {
        let src = "<br>";
        let out = fmt(src);
        assert_eq!(out, "<br />\n");
    }

    #[test]
    fn mustache_compact_spacing() {
        let src = "<p>{{ t  label  }}</p>";
        let out = fmt(src);
        assert_eq!(out, "<p>{{t  label}}</p>\n");
    }

    #[test]
    fn comment_preserved_verbatim() {
        let src = "{{!-- doc\nlines --}}<div></div>";
        let out = fmt(src);
        assert!(out.starts_with("{{!-- doc\nlines --}}"));
    }

    #[test]
    fn idempotent_basic() {
        let src = "<div>{{#if x}}<p>{{name}}</p>{{/if}}</div>";
        let once = fmt(src);
        let twice = fmt(&once);
        assert_eq!(once, twice);
    }

    #[test]
    fn idempotent_with_partial() {
        let src =
            "{{#> partials/field}}\n  <input type=\"text\" name=\"foo\" />\n{{/partials/field}}\n";
        let once = fmt(src);
        let twice = fmt(&once);
        assert_eq!(once, twice);
    }

    #[test]
    fn nested_blocks_indent_correctly() {
        let src = "{{#if a}}{{#if b}}<p>x</p>{{/if}}{{/if}}";
        let out = fmt(src);
        let expected = "{{#if a}}\n  {{#if b}}\n    <p>x</p>\n  {{/if}}\n{{/if}}\n";
        assert_eq!(out, expected);
    }

    #[test]
    fn html_indent_inside_each() {
        let src = "{{#each xs}}<li>{{this}}</li>{{/each}}";
        let out = fmt(src);
        let expected = "{{#each xs}}\n  <li>{{this}}</li>\n{{/each}}\n";
        assert_eq!(out, expected);
    }

    #[test]
    fn final_newline_added() {
        let src = "<p>x</p>";
        assert!(fmt(src).ends_with("\n"));
        // Don't double up if input already ends with newline.
        assert!(!fmt("<p>x</p>\n").ends_with("\n\n"));
    }

    #[test]
    fn preserve_trim_returns_short_run_inline() {
        // Single-line block with simple body should stay one line.
        let src = "<p>{{x}}</p>";
        assert_eq!(fmt(src), "<p>{{x}}</p>\n");
    }

    #[test]
    fn script_body_preserved_verbatim() {
        // Multi-line JS — internal whitespace, quotes, and indent must
        // pass through unchanged. The formatter must not re-flow this.
        let src = "<script>\n  var x = {\n    \"a\": 1,\n    \"b\": 2\n  };\n</script>";
        let out = fmt(src);
        assert!(
            out.contains("\n  var x = {\n    \"a\": 1,\n    \"b\": 2\n  };\n"),
            "script body must pass through verbatim, got:\n{out}"
        );
    }

    #[test]
    fn script_json_with_inline_mustache_preserves_quoting() {
        // The bug this is the regression test for: a `<script
        // type="application/json">` data island with `{{t "key"}}`
        // mustaches inside JSON string values must render as
        // single-line JSON pairs — not split with each mustache on
        // its own indented line, which produced JSON strings with
        // literal newlines and broke `JSON.parse` at runtime.
        let src = "<script type=\"application/json\" id=\"i18n\">\n  {\n    \"a\": \"{{t \"a\"}}\",\n    \"b\": \"{{t \"b\"}}\"\n  }\n</script>";
        let out = fmt(src);
        assert!(
            out.contains("\"a\": \"{{t \"a\"}}\""),
            "mustache must stay inside the JSON string value, got:\n{out}"
        );
        assert!(
            out.contains("\"b\": \"{{t \"b\"}}\""),
            "mustache must stay inside the JSON string value, got:\n{out}"
        );
    }

    #[test]
    fn script_idempotent_with_json_body() {
        let src = "<script type=\"application/json\" id=\"i18n\">\n  {\n    \"a\": \"{{t \"a\"}}\",\n    \"b\": \"{{t \"b\"}}\"\n  }\n</script>";
        let once = fmt(src);
        let twice = fmt(&once);
        assert_eq!(once, twice, "format must be idempotent on script bodies");
    }

    #[test]
    fn pre_body_preserved_verbatim() {
        let src = "<pre>line1\n  indented\n\nblank above</pre>";
        let out = fmt(src);
        assert!(out.contains("line1\n  indented\n\nblank above"));
    }

    #[test]
    fn empty_script_renders_open_close_pair() {
        let src = "<script src=\"/x.js\"></script>";
        let out = fmt(src);
        assert!(out.contains("<script") && out.contains("</script>"));
    }
}
