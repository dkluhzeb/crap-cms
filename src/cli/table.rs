//! Simple CLI table renderer with auto-calculated column widths.

use std::fmt::Write as _;

use console::{measure_text_width, style};

/// Terminal display width of `s` (ANSI escapes stripped, wide chars counted
/// as 2, combining marks as 0). Column widths and padding must use this, not
/// `str::len()` (bytes) or char count, or non-ASCII content misaligns.
fn display_width(s: &str) -> usize {
    measure_text_width(s)
}

/// Left-align `s` in a field of `width` display columns by appending spaces.
/// Unlike `format!("{s:<width$}")` — which pads to a *char* count — this pads
/// to a *display* width, so CJK/emoji/combining content lines up.
fn pad_display(s: &str, width: usize) -> String {
    let pad = width.saturating_sub(display_width(s));
    format!("{s}{}", " ".repeat(pad))
}

/// A simple table for CLI output with bold headers and auto-calculated column widths.
pub struct Table {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl Table {
    /// Create a new table with the given column headers.
    #[must_use]
    pub fn new(headers: Vec<&str>) -> Self {
        Self {
            headers: headers
                .into_iter()
                .map(std::string::ToString::to_string)
                .collect(),
            rows: Vec::new(),
        }
    }

    /// Add a row. The number of cells should match the header count.
    pub fn row(&mut self, cells: Vec<&str>) {
        self.rows.push(
            cells
                .into_iter()
                .map(std::string::ToString::to_string)
                .collect(),
        );
    }

    /// Calculate column widths based on content (headers + rows), measured in
    /// terminal display columns so multibyte content aligns.
    fn column_widths(&self) -> Vec<usize> {
        let col_count = self.headers.len();
        let mut widths: Vec<usize> = self.headers.iter().map(|h| display_width(h)).collect();

        for row in &self.rows {
            for (i, cell) in row.iter().enumerate() {
                let w = display_width(cell);
                if i < col_count && w > widths[i] {
                    widths[i] = w;
                }
            }
        }

        // Add padding (2 spaces between columns)
        for w in &mut widths {
            *w += 2;
        }

        widths
    }

    /// Print the table to stdout: header, separator, data rows.
    pub fn print(&self) {
        let widths = self.column_widths();

        self.print_header(&widths);
        Self::print_separator(&widths);
        self.print_rows(&widths);
    }

    /// Print the bold header row.
    fn print_header(&self, widths: &[usize]) {
        let mut line = String::new();

        for (i, h) in self.headers.iter().enumerate() {
            let w = widths.get(i).copied().unwrap_or(display_width(h) + 2);
            let _ = write!(line, "{}", style(pad_display(h, w)).bold());
        }

        println!("{line}");
    }

    /// Print a dimmed horizontal separator line.
    fn print_separator(widths: &[usize]) {
        let total_width: usize = widths.iter().sum();

        println!("{}", style("─".repeat(total_width)).dim());
    }

    /// Print all data rows with aligned columns.
    fn print_rows(&self, widths: &[usize]) {
        for row in &self.rows {
            let mut line = String::new();

            for (i, cell) in row.iter().enumerate() {
                let w = widths.get(i).copied().unwrap_or(display_width(cell) + 2);
                let _ = write!(line, "{}", pad_display(cell, w));
            }

            println!("{line}");
        }
    }

    /// Print a footer line (e.g., row count) after the table.
    pub fn footer(&self, msg: &str) {
        println!("\n{}", style(msg).dim());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_new_and_print_no_panic() {
        let mut table = Table::new(vec!["ID", "Name", "Status"]);
        table.row(vec!["abc123", "Alice", "active"]);
        table.row(vec!["def456", "Bob", "locked"]);
        table.print();
        table.footer("2 user(s)");
    }

    #[test]
    fn column_widths_respect_content() {
        let mut table = Table::new(vec!["ID", "Name"]);
        table.row(vec!["a-very-long-id-string", "A"]);
        let widths = table.column_widths();
        // Widths should be at least as wide as the longest content + padding
        assert!(widths[0] >= "a-very-long-id-string".len() + 2);
        assert!(widths[1] >= "Name".len() + 2);
    }

    #[test]
    fn empty_table_prints_headers_only() {
        let table = Table::new(vec!["Col1", "Col2"]);
        table.print(); // Should not panic
    }

    #[test]
    fn table_with_mismatched_row_length() {
        let mut table = Table::new(vec!["A", "B", "C"]);
        table.row(vec!["1", "2"]); // Fewer cells than headers
        table.print(); // Should not panic
    }

    #[test]
    fn column_widths_use_display_width_not_bytes() {
        // "李明" is 2 chars / 6 bytes / 4 display columns. The width must be
        // driven by display columns (4), not bytes (6) or chars (2).
        let mut table = Table::new(vec!["Name"]);
        table.row(vec!["李明"]);
        table.row(vec!["Bob"]);
        let widths = table.column_widths();
        // max(display "Name"=4, "李明"=4, "Bob"=3) + 2 padding = 6.
        assert_eq!(widths[0], 6);
    }

    #[test]
    fn pad_display_aligns_wide_and_narrow_cells() {
        // Regression: byte/char-based padding shifted the next column on rows
        // with wide characters. After padding to the same display width, the
        // rendered cells must occupy the same number of terminal columns.
        let wide = pad_display("李明", 6);
        let narrow = pad_display("Bob", 6);
        assert_eq!(display_width(&wide), 6);
        assert_eq!(display_width(&narrow), 6);
        assert_eq!(display_width(&wide), display_width(&narrow));
    }

    #[test]
    fn pad_display_handles_combining_marks() {
        // "e\u{301}" (e + combining acute) is 2 chars but 1 display column.
        let combined = pad_display("e\u{301}", 4);
        assert_eq!(display_width(&combined), 4);
    }
}
