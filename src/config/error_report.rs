//! Every problem one pass over a project's configuration or definitions found,
//! reported together.
//!
//! Loading stops at the first problem otherwise, and an upgrade that trips
//! several independent ones costs one fix-and-restart cycle each. A check that
//! can go on after a problem — the next `crap.toml` section, the next
//! definition file, the next validation — records it here and goes on; the
//! caller turns the report into one error listing them all.

use std::fmt;

use anyhow::{Error, Result};

/// The problems a pass found, each one line (an error's whole context chain).
#[derive(Debug, Default)]
pub struct ErrorReport {
    problems: Vec<String>,
}

impl ErrorReport {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a problem: an error with its whole context chain.
    pub fn push(&mut self, error: &Error) {
        self.problems.push(format!("{error:#}"));
    }

    /// Record a problem described by `message`.
    pub fn push_message(&mut self, message: impl Into<String>) {
        self.problems.push(message.into());
    }

    /// Record `result`'s error, if any, and hand back its value.
    pub fn check<T>(&mut self, result: Result<T>) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(error) => {
                self.push(&error);
                None
            }
        }
    }

    /// Take over every problem `other` recorded.
    pub fn merge(&mut self, other: Self) {
        self.problems.extend(other.problems);
    }

    /// Take over every problem `other` recorded, each prefixed `context: `.
    pub fn merge_under(&mut self, context: &str, other: Self) {
        self.problems.extend(
            other
                .problems
                .into_iter()
                .map(|problem| format!("{context}: {problem}")),
        );
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.problems.is_empty()
    }

    /// `Ok` when nothing was recorded, else one error listing every problem.
    ///
    /// # Errors
    ///
    /// The report itself, when it holds a problem.
    pub fn into_result(self) -> Result<()> {
        if self.is_empty() {
            return Ok(());
        }

        Err(Error::new(self))
    }
}

/// One problem is shown as it is; several as a sorted, numbered list headed by
/// their count, so a report is stable from run to run.
impl fmt::Display for ErrorReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let [only] = self.problems.as_slice() {
            return f.write_str(only);
        }

        let mut problems: Vec<&String> = self.problems.iter().collect();
        problems.sort();

        write!(f, "{} problems:", problems.len())?;

        for (n, problem) in problems.iter().enumerate() {
            write!(f, "\n  {}. {problem}", n + 1)?;
        }

        Ok(())
    }
}

impl std::error::Error for ErrorReport {}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;

    use super::*;

    #[test]
    fn one_problem_reads_as_itself() {
        let mut report = ErrorReport::new();
        report.push_message("server.admin_port must be > 0");

        let err = report.into_result().unwrap_err();
        assert_eq!(err.to_string(), "server.admin_port must be > 0");
    }

    /// Every problem is listed, sorted, each with its context chain.
    #[test]
    fn several_problems_are_listed_sorted() {
        let mut report = ErrorReport::new();
        report.push_message("b problem");
        report.check::<()>(Err(anyhow!("root").context("a problem")));

        let text = report.into_result().unwrap_err().to_string();
        assert_eq!(text, "2 problems:\n  1. a problem: root\n  2. b problem");
    }

    #[test]
    fn merged_problems_carry_the_context() {
        let mut inner = ErrorReport::new();
        inner.push_message("one");
        inner.push_message("two");

        let mut report = ErrorReport::new();
        report.merge_under("Invalid locale configuration", inner);

        let text = report.into_result().unwrap_err().to_string();
        assert!(
            text.contains("1. Invalid locale configuration: one"),
            "{text}"
        );
        assert!(
            text.contains("2. Invalid locale configuration: two"),
            "{text}"
        );
    }

    #[test]
    fn an_empty_report_is_ok() {
        let mut report = ErrorReport::new();
        assert_eq!(report.check(Ok(3)), Some(3));
        assert!(report.into_result().is_ok());
    }
}
