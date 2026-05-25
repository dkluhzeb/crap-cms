//! Shared CLI glyphs with terminal-capability fallback.
//!
//! `output`, `spinner`, and `theme` all need the same `✓ / ⚠ / ✗ / →`
//! glyphs and the same fallback story (`+ / ! / x / >` when the
//! terminal doesn't advertise Unicode emoji support). Centralizing
//! here means the `CRAP_NO_UNICODE=1` / `CRAP_FORCE_UNICODE=1`
//! escape hatches work uniformly across the three surfaces — before
//! this lift, only `output` honoured them; `spinner` and `theme`
//! hard-coded the Unicode glyphs.

use std::sync::OnceLock;

use console::Term;

/// Cached UTF-8 capability check. Computed once per process.
static UNICODE: OnceLock<bool> = OnceLock::new();

/// Whether to use Unicode glyphs in CLI output.
///
/// Resolution order:
/// 1. `CRAP_NO_UNICODE=1` → force ASCII (escape hatch for tests, niche terms).
/// 2. `CRAP_FORCE_UNICODE=1` → force Unicode (override broken detection).
/// 3. `console::Term::stdout().features().wants_emoji()` — checks
///    `LANG`/`LC_ALL`/`LC_CTYPE` on Unix and the active codepage on Windows.
fn unicode_supported() -> bool {
    *UNICODE.get_or_init(|| {
        if std::env::var("CRAP_NO_UNICODE").is_ok_and(|v| v == "1") {
            return false;
        }
        if std::env::var("CRAP_FORCE_UNICODE").is_ok_and(|v| v == "1") {
            return true;
        }
        Term::stdout().features().wants_emoji()
    })
}

/// Pick `unicode` when the terminal can render it, else `ascii`. Both args
/// must be `&'static str` literals so the return type can stay `&'static str`.
fn pick(unicode: &'static str, ascii: &'static str) -> &'static str {
    if unicode_supported() { unicode } else { ascii }
}

/// `✓` (or `+` fallback) — used for success messages.
pub(super) fn success() -> &'static str {
    pick("✓", "+")
}

/// `⚠` (or `!` fallback) — used for warnings.
pub(super) fn warning() -> &'static str {
    pick("⚠", "!")
}

/// `✗` (or `x` fallback) — used for errors.
pub(super) fn error() -> &'static str {
    pick("✗", "x")
}

/// `→` (or `>` fallback) — used for info messages.
pub(super) fn info() -> &'static str {
    pick("→", ">")
}

/// `?` — prompt prefix (same in both Unicode and ASCII).
pub(super) fn prompt() -> &'static str {
    "?"
}

/// `───` (or `---` fallback) — header / step bars.
pub(super) fn bar() -> &'static str {
    pick("───", "---")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyphs_return_one_of_their_inputs() {
        // The exact glyph depends on terminal capability; verify each
        // helper returns one of its declared variants rather than
        // something arbitrary.
        assert!(success() == "✓" || success() == "+");
        assert!(warning() == "⚠" || warning() == "!");
        assert!(error() == "✗" || error() == "x");
        assert!(info() == "→" || info() == ">");
        assert!(bar() == "───" || bar() == "---");
        assert_eq!(prompt(), "?");
    }
}
