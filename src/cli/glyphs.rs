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

/// Whether a `CRAP_*` boolean env toggle is enabled.
///
/// Accepts the common truthy spellings — `1`, `true`, `yes`, `on` —
/// case-insensitively and after trimming, so `CRAP_NO_UNICODE=true` works
/// as a user would expect (not only the exact string `1`). Anything else
/// (including unset, empty, `0`, `false`) reads as disabled. This accepted
/// set is a frozen contract — see `docs/src/internals/frozen-contracts.md`.
fn env_flag_enabled(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// Whether to use Unicode glyphs in CLI output.
///
/// Resolution order:
/// 1. `CRAP_NO_UNICODE` truthy → force ASCII (escape hatch for tests, niche terms).
/// 2. `CRAP_FORCE_UNICODE` truthy → force Unicode (override broken detection).
/// 3. `console::Term::stdout().features().wants_emoji()` — checks
///    `LANG`/`LC_ALL`/`LC_CTYPE` on Unix and the active codepage on Windows.
///
/// "Truthy" is `1`/`true`/`yes`/`on` (see [`env_flag_enabled`]).
fn unicode_supported() -> bool {
    *UNICODE.get_or_init(|| {
        if std::env::var("CRAP_NO_UNICODE").is_ok_and(|v| env_flag_enabled(&v)) {
            return false;
        }
        if std::env::var("CRAP_FORCE_UNICODE").is_ok_and(|v| env_flag_enabled(&v)) {
            return true;
        }
        Term::stdout().features().wants_emoji()
    })
}

/// Braille spinner frames — smooth animation on Unicode-capable terminals.
const BRAILLE_TICKS: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// ASCII spinner frames — fallback for terminals without Unicode support.
const ASCII_TICKS: &[&str] = &["-", "\\", "|", "/"];

/// Spinner tick frames, honouring the same Unicode-capability check as the
/// other glyphs. Braille when supported, a plain ASCII spinner otherwise —
/// so `CRAP_NO_UNICODE=1` (or a non-UTF-8 tty) doesn't emit mojibake during
/// the running animation, matching the fallback story for the static glyphs.
pub(super) fn spinner_ticks() -> &'static [&'static str] {
    if unicode_supported() {
        BRAILLE_TICKS
    } else {
        ASCII_TICKS
    }
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

    #[test]
    fn env_flag_accepts_common_truthy_spellings() {
        for v in ["1", "true", "yes", "on", "TRUE", "Yes", " on ", "ON"] {
            assert!(env_flag_enabled(v), "{v:?} should be truthy");
        }
        for v in ["0", "false", "no", "off", "", "  ", "maybe", "2"] {
            assert!(!env_flag_enabled(v), "{v:?} should be falsy");
        }
    }

    #[test]
    fn spinner_ticks_are_one_of_the_two_sets() {
        let ticks = spinner_ticks();
        assert!(ticks == BRAILLE_TICKS || ticks == ASCII_TICKS);
        // The ASCII fallback must contain no multibyte characters.
        assert!(ASCII_TICKS.iter().all(|t| t.is_ascii() && !t.is_empty()));
        assert!(BRAILLE_TICKS.iter().all(|t| !t.is_empty()));
    }
}
