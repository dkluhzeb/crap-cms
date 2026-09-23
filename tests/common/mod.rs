//! Shared source-scanning helpers for the `tests/` guard scans.
//!
//! `tests/common/` is a subdirectory, so Cargo never compiles it as a test
//! binary of its own; each guard pulls it in with `mod common;`.

/// The production part of `src`, scrubbed so a textual scan can only match
/// live code. Line numbers are preserved: test code is blanked line for line
/// and comments are removed in place, so a match reports the file's real line.
///
/// Three things make a naive `contains` scan lie about what production code
/// does:
///
/// 1. Code that only exists under a test gate. Splitting on the literal
///    `#[cfg(test)]` is not enough — `#[cfg(all(test, feature = "sqlite"))]`
///    and the whole-file inner `#![cfg(test)]` are further spellings, a doc
///    comment that merely *mentions* the attribute is not a gate, and a gated
///    helper in the middle of a file (`#[cfg(test)] impl …`) must hide only
///    itself, not the production code that follows it.
/// 2. Comments, where any needle can appear without the code doing it.
/// 3. Braces inside string and char literals, which throw off brace matching
///    (`format!("{x}")`, a literal `'{'`).
///
/// So this removes comments and neutralizes braces inside literals first,
/// then blanks every test-gated item — the file's inline test module, a gated
/// `use`, a gated helper — up to the end of that item. Rust source only — the
/// literal and comment rules are Rust's. A scan must therefore not look for a
/// needle containing `{` or `}` inside a literal.
pub(crate) fn production_code(src: &str) -> String {
    production(&scrub(src))
}

/// `src` with every test-gated item blanked line for line. An inner
/// `#![cfg(test)]` gates the whole file, so nothing after it is kept.
fn production(src: &str) -> String {
    let lines: Vec<&str> = src.split_inclusive('\n').collect();
    let mut out = String::with_capacity(src.len());
    let mut idx = 0;

    while idx < lines.len() {
        let trimmed = lines[idx].trim_start();

        if trimmed.starts_with("#![cfg(") && is_test_predicate(trimmed) {
            break;
        }

        if trimmed.starts_with("#[cfg(") && is_test_predicate(trimmed) {
            let end = gated_item_end(&lines, idx);
            out.extend(
                lines[idx..end]
                    .iter()
                    .map(|line| &line[line.trim_end_matches('\n').len()..]),
            );
            idx = end;
            continue;
        }

        out.push_str(lines[idx]);
        idx += 1;
    }

    out
}

/// True when the `#[cfg(…)]` / `#![cfg(…)]` attribute opening `line` compiles
/// only under `test`: no build with `test` off satisfies the predicate. So
/// `all(test, …)` gates, while `not(test)`, `any(test, …)`,
/// `all(any(test, feature = "x"), …)` and a negated other atom
/// (`not(tarpaulin_include)`, `not(feature = "x")`) are production code and
/// are kept.
fn is_test_predicate(line: &str) -> bool {
    let Some(open) = line.find("cfg(") else {
        return false;
    };
    let Some(close) = line[open..].rfind(")]") else {
        return false;
    };
    let predicate = line[open + 4..open + close].trim();

    !outcomes_without_test(predicate).can_hold
}

/// Which values a `cfg` predicate can take with `test` off and every other
/// atom free to be on or off.
#[derive(Clone, Copy)]
struct Outcomes {
    can_hold: bool,
    can_fail: bool,
}

/// Evaluate a `cfg` predicate with `test` off and every other atom unknown.
/// Atoms are treated as independent, so a contradiction such as
/// `all(x, not(x))` reads as satisfiable — the safe side for a guard, which
/// then scans the item rather than hiding it.
fn outcomes_without_test(predicate: &str) -> Outcomes {
    let predicate = predicate.trim();

    if let Some(inner) = combinator_args(predicate, "all(") {
        let parts: Vec<Outcomes> = inner.iter().map(|a| outcomes_without_test(a)).collect();

        return Outcomes {
            can_hold: parts.iter().all(|o| o.can_hold),
            can_fail: parts.iter().any(|o| o.can_fail),
        };
    }

    if let Some(inner) = combinator_args(predicate, "any(") {
        let parts: Vec<Outcomes> = inner.iter().map(|a| outcomes_without_test(a)).collect();

        return Outcomes {
            can_hold: parts.iter().any(|o| o.can_hold),
            can_fail: parts.iter().all(|o| o.can_fail),
        };
    }

    if let Some(inner) = combinator_args(predicate, "not(") {
        let inner = outcomes_without_test(inner.first().copied().unwrap_or_default());

        return Outcomes {
            can_hold: inner.can_fail,
            can_fail: inner.can_hold,
        };
    }

    let is_test = predicate == "test";

    Outcomes {
        can_hold: !is_test,
        can_fail: true,
    }
}

/// The arguments of `predicate` when it is the combinator `name` (`"all("`).
fn combinator_args<'a>(predicate: &'a str, name: &str) -> Option<Vec<&'a str>> {
    predicate
        .strip_prefix(name)
        .and_then(|p| p.strip_suffix(')'))
        .map(split_cfg_args)
}

/// Top-level comma-separated arguments of a `cfg` combinator.
fn split_cfg_args(inner: &str) -> Vec<&str> {
    let mut args = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;

    for (idx, c) in inner.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                args.push(inner[start..idx].trim());
                start = idx + 1;
            }
            _ => {}
        }
    }

    args.push(inner[start..].trim());

    args
}

/// Index of the first line after the item gated by the attribute on
/// `lines[gate]`. Further attribute lines belong to the item; the item then
/// ends at a `;` reached before any `{` (a `use`, a `mod x;`) or on the line
/// its braces close. The attribute text itself is not scanned, so a same-line
/// `#[cfg(test)] mod tests;` still ends on its line.
fn gated_item_end(lines: &[&str], gate: usize) -> usize {
    let mut depth = 0i32;
    let mut opened = false;

    for (idx, line) in lines.iter().enumerate().skip(gate) {
        let trimmed = line.trim_start();
        let scanned = if trimmed.starts_with("#[") {
            trimmed.split_once(']').map_or("", |(_, rest)| rest)
        } else {
            trimmed
        };

        for c in scanned.chars() {
            match c {
                '{' => {
                    depth += 1;
                    opened = true;
                }
                '}' => depth -= 1,
                ';' if !opened => return idx + 1,
                _ => {}
            }
        }

        if opened && depth == 0 {
            return idx + 1;
        }
    }

    lines.len()
}

/// Drop comments and neutralize braces inside literals.
fn scrub(src: &str) -> String {
    let chars: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;

    while i < chars.len() {
        if let Some(end) = skip_literal(&chars, i) {
            // A brace inside a literal is text, not structure.
            out.extend(chars[i..end].iter().map(|c| match *c {
                '{' | '}' => ' ',
                other => other,
            }));
            i = end;
            continue;
        }

        if chars[i] == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }

        if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
            let end = end_of_block_comment(&chars, i + 2);
            out.extend(chars[i..end].iter().filter(|c| **c == '\n'));
            i = end;
            continue;
        }

        out.push(chars[i]);
        i += 1;
    }

    out
}

/// Index just past the string, raw-string or char literal starting at `i`, or
/// `None` when `i` does not start one — a `'` opens a lifetime far more often
/// than a char literal.
fn skip_literal(chars: &[char], i: usize) -> Option<usize> {
    match *chars.get(i)? {
        '"' => Some(end_of_quoted(chars, i + 1)),
        'r' if matches!(chars.get(i + 1), Some('"' | '#')) => skip_raw_string(chars, i),
        '\'' => skip_char_literal(chars, i),
        _ => None,
    }
}

/// Index just past the closing `"` of a quoted string opened before `from`.
fn end_of_quoted(chars: &[char], from: usize) -> usize {
    let mut i = from;

    while i < chars.len() {
        match chars[i] {
            '\\' => i += 2,
            '"' => return i + 1,
            _ => i += 1,
        }
    }

    chars.len()
}

/// Index just past an `r"…"` / `r#"…"#` literal starting at `i`.
fn skip_raw_string(chars: &[char], i: usize) -> Option<usize> {
    let mut open = i + 1;
    let mut hashes = 0;

    while chars.get(open) == Some(&'#') {
        hashes += 1;
        open += 1;
    }

    if chars.get(open) != Some(&'"') {
        return None;
    }

    let mut j = open + 1;

    while j < chars.len() {
        if chars[j] == '"' && chars[j + 1..].iter().take(hashes).all(|c| *c == '#') {
            return Some(j + 1 + hashes);
        }

        j += 1;
    }

    Some(chars.len())
}

/// Index just past a `'x'` / `'\n'` char literal starting at `i`, or `None`
/// when the quote opens a lifetime.
fn skip_char_literal(chars: &[char], i: usize) -> Option<usize> {
    if chars.get(i + 1) == Some(&'\\') {
        // The escaped character (`'\''`, `'\n'`, `'\u{..}'`) is skipped as a
        // unit; the literal ends at the first `'` after it.
        let mut j = i + 3;

        while j < chars.len() && chars[j] != '\'' {
            j += 1;
        }

        return Some((j + 1).min(chars.len()));
    }

    if chars.get(i + 2) == Some(&'\'') {
        return Some(i + 3);
    }

    None
}

/// Index just past the `*/` closing a block comment opened before `from`.
fn end_of_block_comment(chars: &[char], from: usize) -> usize {
    let mut depth = 1;
    let mut i = from;

    while i < chars.len() && depth > 0 {
        if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
            depth += 1;
            i += 2;
        } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
            depth -= 1;
            i += 2;
        } else {
            i += 1;
        }
    }

    i
}
