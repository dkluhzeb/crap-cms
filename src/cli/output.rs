//! Colored CLI output primitives. Auto-disables color when piped, and falls
//! back to ASCII glyphs when the terminal isn't UTF-8 capable.

use std::sync::OnceLock;

use console::{Term, style};

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
fn glyph(unicode: &'static str, ascii: &'static str) -> &'static str {
    if unicode_supported() { unicode } else { ascii }
}

/// Print a success message: `<glyph> msg` in green on stdout.
pub fn success(msg: &str) {
    println!("{} {}", style(glyph("✓", "+")).green().bold(), msg);
}

/// Print an error message: `<glyph> msg` in red bold on stderr.
pub fn error(msg: &str) {
    eprintln!("{} {}", style(glyph("✗", "x")).red().bold(), msg);
}

/// Print a warning message: `<glyph> msg` in yellow on stderr.
pub fn warning(msg: &str) {
    eprintln!("{} {}", style(glyph("⚠", "!")).yellow().bold(), msg);
}

/// Print an info message: `<glyph> msg` in blue on stdout.
pub fn info(msg: &str) {
    println!("{} {}", style(glyph("→", ">")).blue().bold(), msg);
}

/// Print a hint (dimmed) on stdout.
pub fn hint(msg: &str) {
    println!("  {}", style(msg).dim());
}

/// Print a section header: `─── title ───` in bold on stdout.
pub fn header(title: &str) {
    let bar = glyph("───", "---");
    println!();
    println!(
        "{} {} {}",
        style(bar).dim(),
        style(title).bold(),
        style(bar).dim()
    );
}

/// Print a wizard step indicator: `[n/total] msg` in cyan on stdout.
pub fn step(n: usize, total: usize, msg: &str) {
    let bar = glyph("───", "---");
    println!(
        "\n{} {} {}",
        style(bar).dim(),
        style(format!("{msg} [{n}/{total}]")).bold(),
        style(bar).dim()
    );
}

/// Print dimmed text on stdout.
pub fn dim(msg: &str) {
    println!("{}", style(msg).dim());
}

/// Print a key-value pair with bold key on stdout.
pub fn kv(key: &str, value: &str) {
    println!("{:<12} {}", style(format!("{key}:")).bold(), value);
}

/// Print a key-value pair with conditional coloring: green if good, red if not.
pub fn kv_status(key: &str, value: &str, good: bool) {
    let colored = if good {
        style(value.to_string()).green()
    } else {
        style(value.to_string()).red()
    };

    println!("{:<12} {}", style(format!("{key}:")).bold(), colored);
}

#[cfg(test)]
mod tests {
    use super::*;

    // These functions write to stdout/stderr. We verify they don't panic.
    // Actual color output depends on terminal capability.

    #[test]
    fn success_does_not_panic() {
        success("test message");
    }

    #[test]
    fn error_does_not_panic() {
        error("test error");
    }

    #[test]
    fn warning_does_not_panic() {
        warning("test warning");
    }

    #[test]
    fn info_does_not_panic() {
        info("test info");
    }

    #[test]
    fn hint_does_not_panic() {
        hint("test hint");
    }

    #[test]
    fn header_does_not_panic() {
        header("Test Section");
    }

    #[test]
    fn step_does_not_panic() {
        step(1, 5, "Server config");
    }

    #[test]
    fn dim_does_not_panic() {
        dim("dimmed text");
    }

    #[test]
    fn kv_does_not_panic() {
        kv("Key", "value");
    }

    #[test]
    fn kv_status_good() {
        kv_status("Status", "healthy", true);
    }

    #[test]
    fn kv_status_bad() {
        kv_status("Status", "failing", false);
    }

    #[test]
    fn glyph_picks_static_literal() {
        // Smoke-check the helper returns one of the inputs depending on cap.
        let g = glyph("✓", "+");
        assert!(g == "✓" || g == "+");
    }
}
