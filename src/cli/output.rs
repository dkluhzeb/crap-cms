//! Colored CLI output primitives. Auto-disables color when piped, and falls
//! back to ASCII glyphs (via [`super::glyphs`]) when the terminal isn't
//! UTF-8 capable.

use console::style;

use super::glyphs;

/// Print a success message: `<glyph> msg` in green on stdout.
pub fn success(msg: &str) {
    println!("{} {}", style(glyphs::success()).green().bold(), msg);
}

/// Print an error message: `<glyph> msg` in red bold on stderr.
pub fn error(msg: &str) {
    eprintln!("{} {}", style(glyphs::error()).red().bold(), msg);
}

/// Print a warning message: `<glyph> msg` in yellow on stderr.
pub fn warning(msg: &str) {
    eprintln!("{} {}", style(glyphs::warning()).yellow().bold(), msg);
}

/// Print an info message: `<glyph> msg` in blue on stdout.
pub fn info(msg: &str) {
    println!("{} {}", style(glyphs::info()).blue().bold(), msg);
}

/// Print a hint (dimmed) on stdout.
pub fn hint(msg: &str) {
    println!("  {}", style(msg).dim());
}

/// Print a section header: `─── title ───` in bold on stdout.
pub fn header(title: &str) {
    let bar = glyphs::bar();
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
    let bar = glyphs::bar();
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
}
