//! Progress spinner for long CLI operations.

use std::time::Duration;

use console::{Term, style};
use indicatif::{ProgressBar, ProgressStyle};

/// A spinner for long-running CLI operations.
///
/// Uses `ProgressBar::hidden()` when stdout is not a terminal (e.g., piped output).
pub struct Spinner {
    bar: ProgressBar,
}

impl Spinner {
    /// Create and start a new spinner with the given message.
    pub fn new(msg: &str) -> Self {
        let bar = if Term::stdout().is_term() {
            let pb = ProgressBar::new_spinner();

            pb.set_style(
                ProgressStyle::default_spinner()
                    .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"])
                    .template("{spinner} {msg}")
                    .expect("valid template"),
            );
            pb.set_message(msg.to_string());
            pb.enable_steady_tick(Duration::from_millis(80));

            pb
        } else {
            // Non-interactive: print the message and use a hidden bar
            println!("{}", msg);
            ProgressBar::hidden()
        };

        Self { bar }
    }

    /// Update the spinner message.
    pub fn set_message(&self, msg: &str) {
        self.bar.set_message(msg.to_string());
    }

    /// Finish with a success message: `✓ msg` in green.
    pub fn finish_success(&self, msg: &str) {
        self.bar
            .finish_with_message(format!("{} {}", style("✓").green().bold(), msg));
    }

    /// Finish with a warning message: `⚠ msg` in yellow.
    pub fn finish_warning(&self, msg: &str) {
        self.bar
            .finish_with_message(format!("{} {}", style("⚠").yellow().bold(), msg));
    }

    /// Finish with an error message: `✗ msg` in red.
    pub fn finish_error(&self, msg: &str) {
        self.bar
            .finish_with_message(format!("{} {}", style("✗").red().bold(), msg));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spinner_lifecycle() {
        let spin = Spinner::new("Loading...");
        spin.set_message("Still loading...");
        spin.finish_success("Done");
    }

    #[test]
    fn spinner_warning_finish() {
        let spin = Spinner::new("Checking...");
        spin.finish_warning("Partial success");
    }

    #[test]
    fn spinner_error_finish() {
        let spin = Spinner::new("Processing...");
        spin.finish_error("Failed");
    }
}
