//! A structural code emitter: tracks indentation and balanced blocks so a
//! printer can't leak an unclosed brace or mis-indent a line. Language-agnostic
//! — each printer supplies its own tokens (`{`, `}`, `interface`, …).

/// Emits indented lines into a growing `String`. The [`block`](Self::block)
/// helper guarantees a matching closer, making an unbalanced brace structurally
/// impossible.
pub(super) struct CodeWriter {
    out: String,
    depth: usize,
    unit: &'static str,
}

impl CodeWriter {
    /// Create a writer whose indentation step is `unit` (e.g. `"  "` or `"\t"`).
    pub(super) fn new(unit: &'static str) -> Self {
        Self {
            out: String::new(),
            depth: 0,
            unit,
        }
    }

    /// Emit one line at the current indentation, followed by a newline. An empty
    /// `s` emits a bare newline with no indentation.
    pub(super) fn line(&mut self, s: &str) {
        if !s.is_empty() {
            for _ in 0..self.depth {
                self.out.push_str(self.unit);
            }
            self.out.push_str(s);
        }
        self.out.push('\n');
    }

    /// Emit a blank line.
    pub(super) fn blank(&mut self) {
        self.out.push('\n');
    }

    /// Emit `header`, run `body` one level deeper, then emit `close` back at the
    /// header's level. The closer always matches — the caller cannot forget it.
    pub(super) fn block(&mut self, header: &str, close: &str, body: impl FnOnce(&mut Self)) {
        self.line(header);
        self.depth += 1;
        body(self);
        self.depth -= 1;
        self.line(close);
    }

    /// Consume the writer and return the accumulated source.
    pub(super) fn finish(self) -> String {
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_indents_and_closes() {
        let mut w = CodeWriter::new("  ");
        w.block("struct S {", "}", |w| {
            w.line("a: i32,");
            w.block("inner: T {", "},", |w| w.line("b: bool,"));
        });
        assert_eq!(
            w.finish(),
            "struct S {\n  a: i32,\n  inner: T {\n    b: bool,\n  },\n}\n"
        );
    }

    #[test]
    fn blank_line_has_no_indent() {
        let mut w = CodeWriter::new("\t");
        w.block("x {", "}", |w| {
            w.line("y");
            w.blank();
        });
        assert_eq!(w.finish(), "x {\n\ty\n\n}\n");
    }
}
